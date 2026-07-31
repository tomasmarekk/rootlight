//! Native Unix process backend and fail-closed adapter isolation launcher.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions, Permissions},
    io::{self, Read as _, Write as _},
    os::unix::{
        fs::{OpenOptionsExt as _, PermissionsExt as _},
        process::CommandExt as _,
    },
    path::{Path, PathBuf},
    process::{
        Child as StdChild, ChildStderr as StdChildStderr, ChildStdin as StdChildStdin,
        ChildStdout as StdChildStdout, Command, ExitStatus, Stdio,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use nix::unistd::setsid;
use nix::{
    errno::Errno,
    sys::resource::{Resource, rlim_t, setrlimit},
    sys::signal::{Signal, killpg},
    unistd::Pid,
};

#[cfg(target_os = "macos")]
use super::macos;

use crate::{
    AdapterExecutableDigest, AdapterIsolationReport, AdapterProcessCommand, AdapterSandboxLimits,
    IsolatedAdapterEntry, MAX_ADAPTER_EXECUTABLE_BYTES, ProcessCommand, ProcessError, StdioMode,
    adapter::copy_authenticated_executable,
};

const LAUNCHER_ARGUMENT: &str = "--rootlight-native-isolation-launcher";
const LAUNCHER_SEPARATOR: &str = "--";
const HANDSHAKE: &[u8] = b"rootlight-native-isolated/1\n";
#[cfg(target_os = "macos")]
const MACOS_UNLINK_READY: &[u8] = b"rootlight-macos-unlink/1\n";
#[cfg(target_os = "macos")]
const MACOS_UNLINK_ACK: &[u8] = b"\x06";
#[cfg(target_os = "macos")]
const MACOS_FAILURE_PREFIX: &str = "rootlight-macos-launcher-failure/1:";
#[cfg(target_os = "macos")]
const MACOS_DIAGNOSTIC_LIMIT: u64 = 4096;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(2);
const EXECUTABLE_BUSY_RETRY_LIMIT: usize = 8;
const ADAPTER_DESCRIPTOR_LIMIT: u64 = 64;
const STAGED_EXECUTABLE: &str = "adapter";
#[cfg(target_os = "macos")]
const MACOS_RESOURCE_STAGE: &str = "--rootlight-macos-resource-stage";
#[cfg(target_os = "macos")]
const MACOS_SANDBOX_STAGE: &str = "--rootlight-macos-sandbox-stage";

#[derive(Debug)]
pub(crate) struct ChildProcess {
    child: StdChild,
}

pub(crate) type ChildStdin = StdChildStdin;
pub(crate) type ChildStdout = StdChildStdout;
pub(crate) type ChildStderr = StdChildStderr;

impl ChildProcess {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.child
            .try_wait()
            .map_err(|source| ProcessError::io("query child status", source))
    }

    pub(crate) fn terminate(&mut self) -> Result<(), ProcessError> {
        self.child
            .kill()
            .map_err(|source| ProcessError::io("terminate child", source))
    }
}

#[derive(Debug)]
pub(crate) struct KillOnCloseJob;

impl KillOnCloseJob {
    pub(crate) fn new() -> Result<Self, ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn active_processes(&self) -> Result<u32, ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn terminate(&self, _exit_code: u32) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn wait_empty(&self, _deadline: Instant) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }

    pub(crate) fn handoff(self, _child: &ChildProcess) -> Result<(), ProcessError> {
        Err(ProcessError::UnsupportedPlatform)
    }
}

#[derive(Debug)]
struct ImmutableWorkspace {
    _directory: tempfile::TempDir,
    root: PathBuf,
    executable: PathBuf,
    #[cfg(target_os = "macos")]
    executable_digest: AdapterExecutableDigest,
}

impl ImmutableWorkspace {
    fn stage(
        source: &Path,
        expected_digest: Option<AdapterExecutableDigest>,
    ) -> Result<Self, ProcessError> {
        let (mut input, source_bytes) = open_adapter_executable(source)?;
        let directory = tempfile::Builder::new()
            .prefix("rootlight-adapter-")
            .tempdir()
            .map_err(|error| ProcessError::io("create adapter workspace", error))?;
        let executable = directory.path().join(STAGED_EXECUTABLE);
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&executable)
            .map_err(|error| ProcessError::io("create staged adapter executable", error))?;
        let executable_digest =
            copy_authenticated_executable(&mut input, source_bytes, expected_digest, &mut output)?;
        #[cfg(not(target_os = "macos"))]
        let _ = executable_digest;
        output
            .sync_all()
            .map_err(|error| ProcessError::io("sync staged adapter executable", error))?;
        drop(output);
        let root = fs::canonicalize(directory.path())
            .map_err(|error| ProcessError::io("resolve staged adapter workspace", error))?;
        let executable = fs::canonicalize(&executable)
            .map_err(|error| ProcessError::io("resolve staged adapter executable", error))?;
        if executable.parent() != Some(root.as_path()) {
            return Err(ProcessError::InvalidInput(
                "the staged adapter executable escaped its workspace".to_owned(),
            ));
        }
        fs::set_permissions(&executable, Permissions::from_mode(0o555))
            .map_err(|error| ProcessError::io("seal staged adapter executable", error))?;
        fs::set_permissions(&root, Permissions::from_mode(0o555))
            .map_err(|error| ProcessError::io("seal adapter workspace", error))?;

        Ok(Self {
            _directory: directory,
            root,
            executable,
            #[cfg(target_os = "macos")]
            executable_digest,
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(target_os = "macos")]
    const fn executable_digest(&self) -> AdapterExecutableDigest {
        self.executable_digest
    }

    #[cfg(target_os = "macos")]
    fn unlink_executable(&self) -> Result<(), ProcessError> {
        fs::set_permissions(&self.root, Permissions::from_mode(0o700))
            .map_err(|error| ProcessError::io("open adapter workspace for unlink", error))?;
        let unlink_result = fs::remove_file(&self.executable)
            .map_err(|error| ProcessError::io("unlink staged adapter executable", error));
        let seal_result = fs::set_permissions(&self.root, Permissions::from_mode(0o500))
            .map_err(|error| ProcessError::io("reseal unlinked adapter workspace", error));
        unlink_result?;
        seal_result
    }
}

fn open_adapter_executable(source: &Path) -> Result<(File, u64), ProcessError> {
    if !source.is_absolute() {
        return Err(ProcessError::InvalidInput(
            "the adapter executable path must be absolute".to_owned(),
        ));
    }
    let input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .map_err(|error| ProcessError::io("open adapter executable without links", error))?;
    let metadata = input
        .metadata()
        .map_err(|error| ProcessError::io("inspect opened adapter executable", error))?;
    if !metadata.is_file() {
        return Err(ProcessError::InvalidInput(
            "the adapter executable must be a regular file".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_ADAPTER_EXECUTABLE_BYTES {
        return Err(ProcessError::InvalidInput(
            "the adapter executable size is outside the hard limit".to_owned(),
        ));
    }
    Ok((input, metadata.len()))
}

impl Drop for ImmutableWorkspace {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.root, Permissions::from_mode(0o700));
        let _ = fs::set_permissions(&self.executable, Permissions::from_mode(0o700));
    }
}

#[derive(Debug)]
pub(crate) struct IsolatedAdapterProcess {
    child: StdChild,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    workspace: ImmutableWorkspace,
    input_limit: usize,
    output_limit: usize,
    diagnostic_limit: usize,
    process_group: Pid,
}

impl IsolatedAdapterProcess {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) const fn input_limit(&self) -> usize {
        self.input_limit
    }

    pub(crate) const fn output_limit(&self) -> usize {
        self.output_limit
    }

    pub(crate) const fn diagnostic_limit(&self) -> usize {
        self.diagnostic_limit
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, ProcessError> {
        self.child
            .try_wait()
            .map_err(|error| ProcessError::io("query isolated adapter status", error))
    }

    pub(crate) fn terminate(&self) -> Result<(), ProcessError> {
        match killpg(self.process_group, Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => Ok(()),
            Err(error) => Err(nix_error("terminate adapter process group", error)),
        }
    }

    pub(crate) fn wait_empty(&self, deadline: Instant) -> Result<(), ProcessError> {
        loop {
            match killpg(self.process_group, None) {
                Err(Errno::ESRCH) => return Ok(()),
                Ok(()) | Err(Errno::EPERM) => {}
                Err(error) => return Err(nix_error("query adapter process group", error)),
            }
            if Instant::now() >= deadline {
                return Err(ProcessError::Deadline {
                    operation: "wait for adapter process group",
                });
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }

    pub(crate) fn fail_closed_cleanup(mut self) -> Result<(), ProcessError> {
        self.terminate()?;
        let _ = self.child.wait();
        self.wait_empty(Instant::now() + Duration::from_secs(2))
    }
}

impl Drop for IsolatedAdapterProcess {
    fn drop(&mut self) {
        let _ = self.terminate();
        let _ = self.child.wait();
        let _ = self.wait_empty(Instant::now() + Duration::from_secs(2));
        let _ = self.workspace.root();
    }
}

pub(crate) fn spawn(command: ProcessCommand) -> Result<ChildProcess, ProcessError> {
    validate_command(&command)?;
    let mut process = Command::new(command.program);
    process.args(command.arguments);
    if command.clear_environment {
        process.env_clear();
    }
    process.envs(command.environment);
    if let Some(directory) = command.current_directory {
        process.current_dir(directory);
    }
    process
        .stdin(stdio(command.stdin))
        .stdout(stdio(command.stdout))
        .stderr(stdio(command.stderr));
    process
        .spawn()
        .map(|child| ChildProcess { child })
        .map_err(|source| ProcessError::io("create child", source))
}

pub(crate) fn spawn_in_job(
    _command: ProcessCommand,
    _job: &KillOnCloseJob,
) -> Result<ChildProcess, ProcessError> {
    Err(ProcessError::UnsupportedPlatform)
}

pub(crate) fn probe_windows_adapter_isolation(
    _command: ProcessCommand,
    _limits: AdapterSandboxLimits,
) -> Result<AdapterIsolationReport, ProcessError> {
    Err(ProcessError::UnsupportedPlatform)
}

pub(crate) fn spawn_isolated_adapter(
    command: AdapterProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<(IsolatedAdapterProcess, AdapterIsolationReport), ProcessError> {
    let workspace =
        ImmutableWorkspace::stage(&command.program, command.expected_executable_digest)?;
    let mut process = isolated_command(&workspace, &command, limits)?;
    let mut child = retry_executable_busy(|| process.spawn())
        .map_err(|error| ProcessError::io("create isolated adapter launcher", error))?;
    let process_group = Pid::from_raw(i32::try_from(child.id()).map_err(|_| {
        ProcessError::InvalidInput("adapter process identifier is not representable".to_owned())
    })?);
    let mut stdout = child.stdout.take().ok_or_else(|| {
        ProcessError::InvalidInput("isolated adapter output pipe is missing".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessError::InvalidInput("isolated adapter diagnostics pipe is missing".to_owned())
    })?;
    #[cfg(target_os = "macos")]
    let mut stderr = stderr;
    #[cfg(target_os = "linux")]
    let verification = verify_record(&mut stdout, &mut child, HANDSHAKE);
    #[cfg(target_os = "macos")]
    let verification = verify_macos_unlink_handshake(&workspace, &mut stdout, &mut child);
    if let Err(error) = verification {
        let _ = killpg(process_group, Signal::SIGKILL);
        let _ = child.wait();
        #[cfg(target_os = "macos")]
        return Err(macos_verification_failure(error, &mut stderr));
        #[cfg(target_os = "linux")]
        return Err(error);
    }

    let report = platform_report();
    Ok((
        IsolatedAdapterProcess {
            child,
            stdout: Some(stdout),
            stderr: Some(stderr),
            workspace,
            input_limit: command.input_limit,
            output_limit: command.output_limit,
            diagnostic_limit: command.diagnostic_limit,
            process_group,
        },
        report,
    ))
}

fn retry_executable_busy<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    for attempt in 0..=EXECUTABLE_BUSY_RETRY_LIMIT {
        match operation() {
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY)
                    && attempt < EXECUTABLE_BUSY_RETRY_LIMIT =>
            {
                // Some Unix filesystems briefly retain the writer lease after
                // a sealed staged executable is closed and synced.
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
            result => return result,
        }
    }
    unreachable!("the bounded executable retry loop always returns")
}

fn isolated_command(
    workspace: &ImmutableWorkspace,
    command: &AdapterProcessCommand,
    limits: AdapterSandboxLimits,
) -> Result<Command, ProcessError> {
    let mut arguments = vec![
        OsString::from(LAUNCHER_ARGUMENT),
        #[cfg(target_os = "macos")]
        OsString::from(MACOS_RESOURCE_STAGE),
        OsString::from(limits.memory_bytes().to_string()),
        OsString::from(limits.cpu_seconds().to_string()),
        OsString::from(ADAPTER_DESCRIPTOR_LIMIT.to_string()),
        OsString::from(LAUNCHER_SEPARATOR),
    ];
    arguments.extend(command.arguments.iter().cloned());

    #[cfg(target_os = "linux")]
    let mut process = {
        let mut process = Command::new(&workspace.executable);
        process.args(arguments);
        process
    };
    #[cfg(target_os = "macos")]
    let mut process = {
        let mut process = Command::new(&workspace.executable);
        process.args(arguments);
        // SETEXEC preserves this operation-owned process group across both
        // Darwin replacement stages and the final adapter entry.
        process.process_group(0);
        process
    };

    process
        .env_clear()
        .current_dir(workspace.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(process)
}

#[cfg(target_os = "macos")]
fn verify_macos_unlink_handshake(
    workspace: &ImmutableWorkspace,
    stdout: &mut ChildStdout,
    child: &mut StdChild,
) -> Result<(), ProcessError> {
    verify_record(stdout, child, MACOS_UNLINK_READY)?;
    workspace.unlink_executable()?;
    let stdin = child.stdin.as_mut().ok_or_else(|| {
        ProcessError::InvalidInput("isolated adapter input pipe is missing".to_owned())
    })?;
    stdin
        .write_all(MACOS_UNLINK_ACK)
        .and_then(|()| stdin.write_all(&workspace.executable_digest().as_bytes()))
        .and_then(|()| stdin.flush())
        .map_err(|error| ProcessError::io("acknowledge staged adapter unlink", error))?;
    verify_record(stdout, child, HANDSHAKE)
}

fn verify_record(
    stdout: &mut ChildStdout,
    child: &mut StdChild,
    expected: &[u8],
) -> Result<(), ProcessError> {
    use std::os::fd::AsFd as _;

    let original = nix::fcntl::fcntl(stdout.as_fd(), nix::fcntl::FcntlArg::F_GETFL)
        .map_err(|error| nix_error("query launcher handshake flags", error))?;
    let flags = nix::fcntl::OFlag::from_bits_truncate(original);
    nix::fcntl::fcntl(
        stdout.as_fd(),
        nix::fcntl::FcntlArg::F_SETFL(flags | nix::fcntl::OFlag::O_NONBLOCK),
    )
    .map_err(|error| nix_error("set launcher handshake nonblocking", error))?;

    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut observed = vec![0_u8; expected.len()];
    let mut offset = 0;
    while offset < observed.len() {
        match stdout.read(&mut observed[offset..]) {
            Ok(0) => {
                return Err(ProcessError::InvalidInput(
                    "isolated adapter launcher closed before verification".to_owned(),
                ));
            }
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if child
                    .try_wait()
                    .map_err(|source| ProcessError::io("query adapter launcher", source))?
                    .is_some()
                {
                    return Err(ProcessError::InvalidInput(
                        "isolated adapter launcher exited before verification".to_owned(),
                    ));
                }
                if Instant::now() >= deadline {
                    return Err(ProcessError::Deadline {
                        operation: "verify isolated adapter launcher",
                    });
                }
                thread::sleep(PROCESS_POLL_INTERVAL);
            }
            Err(error) => return Err(ProcessError::io("read isolation handshake", error)),
        }
    }
    if observed != expected {
        return Err(ProcessError::InvalidInput(
            "isolated adapter launcher verification failed".to_owned(),
        ));
    }
    nix::fcntl::fcntl(stdout.as_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))
        .map_err(|error| nix_error("restore launcher handshake flags", error))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_verification_failure(fallback: ProcessError, stderr: &mut ChildStderr) -> ProcessError {
    // The child has been reaped, so this bounded read cannot wait for more
    // bytes. Only closed stage codes are promoted into the parent error.
    let mut diagnostics = Vec::new();
    if stderr
        .take(MACOS_DIAGNOSTIC_LIMIT)
        .read_to_end(&mut diagnostics)
        .is_err()
    {
        return fallback;
    }
    let Ok(diagnostics) = std::str::from_utf8(&diagnostics) else {
        return fallback;
    };
    if let Some(code) = diagnostics
        .lines()
        .find_map(|line| line.strip_prefix(MACOS_FAILURE_PREFIX))
        .and_then(known_macos_failure_code)
    {
        return ProcessError::InvalidInput(format!("isolated adapter launcher failed at {code}"));
    }
    if diagnostics.contains("sandbox-exec") {
        return ProcessError::InvalidInput(
            "isolated adapter launcher failed at sandbox-entry".to_owned(),
        );
    }
    fallback
}

#[cfg(target_os = "macos")]
fn known_macos_failure_code(code: &str) -> Option<&'static str> {
    [
        "hard-limit-replacement",
        "sandbox-entry",
        "cpu-limit",
        "descriptor-limit",
        "core-limit",
        "file-output-limit",
        "resolve-executable",
        "resolve-workspace",
        "resolve-sandboxed-executable",
        "resolve-sandboxed-workspace",
        "descriptor-closure",
        "unlink-acknowledgement",
        "executable-identity",
        "verify-unlink",
        "handshake-write",
        "launcher-io",
        "launcher-contract",
        "unsupported-platform",
        "launcher-deadline",
    ]
    .into_iter()
    .find(|known| *known == code)
}

#[cfg(target_os = "linux")]
pub(crate) fn enter_isolated_adapter_launcher(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<IsolatedAdapterEntry, ProcessError> {
    let (memory_bytes, cpu_seconds, descriptor_limit, adapter_arguments) =
        parse_launcher_contract(&mut arguments)?;
    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::io("resolve staged adapter executable", error))?;
    close_inherited_descriptors()?;
    establish_linux_resource_limits(memory_bytes, cpu_seconds, descriptor_limit)?;
    setsid().map_err(|error| nix_error("create adapter process session", error))?;
    establish_linux_platform_isolation()?;
    let never = enter_verified_adapter(&executable, adapter_arguments)?;
    match never {}
}

#[cfg(target_os = "macos")]
pub(crate) fn enter_isolated_adapter_launcher(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<IsolatedAdapterEntry, ProcessError> {
    match arguments.next().as_deref() {
        Some(stage) if stage == std::ffi::OsStr::new(MACOS_RESOURCE_STAGE) => {
            enter_macos_resource_stage(arguments)
        }
        Some(stage) if stage == std::ffi::OsStr::new(MACOS_SANDBOX_STAGE) => {
            enter_macos_sandbox_stage(arguments)
        }
        _ => Err(ProcessError::InvalidInput(
            "isolated adapter Darwin launcher stage is invalid".to_owned(),
        )),
    }
}

#[cfg(target_os = "macos")]
fn enter_macos_resource_stage(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<IsolatedAdapterEntry, ProcessError> {
    let (memory_bytes, cpu_seconds, descriptor_limit, adapter_arguments) =
        parse_launcher_contract(&mut arguments)?;
    close_inherited_descriptors()?;
    establish_macos_resource_limits(cpu_seconds, descriptor_limit)?;
    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::io("resolve staged adapter executable", error))?;
    let workspace = std::env::current_dir()
        .map_err(|error| ProcessError::io("resolve staged adapter workspace", error))?;
    if executable.parent() != Some(workspace.as_path()) {
        return Err(ProcessError::InvalidInput(
            "staged adapter executable does not belong to its workspace".to_owned(),
        ));
    }
    let mut sandbox_arguments = vec![
        OsString::from(LAUNCHER_ARGUMENT),
        OsString::from(MACOS_SANDBOX_STAGE),
        OsString::from(LAUNCHER_SEPARATOR),
    ];
    sandbox_arguments.extend(adapter_arguments);
    // SETEXEC applies the fatal ledger without creating a descendant. The next
    // trusted stage enters Seatbelt in-process, completes the unlink protocol,
    // and returns directly into dispatch with both fork and exec still denied.
    let never =
        macos::replace_process_with_memory_limit(&executable, &sandbox_arguments, memory_bytes)
            .map_err(|error| ProcessError::io("enter hard-limited Darwin adapter stage", error))?;
    match never {}
}

#[cfg(target_os = "macos")]
fn enter_macos_sandbox_stage(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<IsolatedAdapterEntry, ProcessError> {
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(LAUNCHER_SEPARATOR)) {
        return Err(ProcessError::InvalidInput(
            "isolated adapter sandbox stage separator is invalid".to_owned(),
        ));
    }
    close_inherited_descriptors()?;
    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::io("resolve sandboxed adapter executable", error))?;
    let workspace = std::env::current_dir()
        .map_err(|error| ProcessError::io("resolve sandboxed adapter workspace", error))?;
    if executable.parent() != Some(workspace.as_path()) {
        return Err(ProcessError::InvalidInput(
            "sandboxed adapter executable does not belong to its workspace".to_owned(),
        ));
    }
    macos::enter_sandbox(&macos_profile(&executable, &workspace))
        .map_err(|error| ProcessError::io("enter Darwin sandbox", error))?;
    write_record(MACOS_UNLINK_READY)?;
    let mut acknowledgement = [0_u8; MACOS_UNLINK_ACK.len()];
    io::stdin()
        .lock()
        .read_exact(&mut acknowledgement)
        .map_err(|error| ProcessError::io("read staged adapter unlink acknowledgement", error))?;
    if acknowledgement != MACOS_UNLINK_ACK {
        return Err(ProcessError::InvalidInput(
            "staged adapter unlink acknowledgement is invalid".to_owned(),
        ));
    }
    let mut executable_digest = [0_u8; blake3::OUT_LEN];
    io::stdin()
        .lock()
        .read_exact(&mut executable_digest)
        .map_err(|error| ProcessError::io("read staged adapter executable identity", error))?;
    match fs::symlink_metadata(&executable) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(ProcessError::InvalidInput(
                "staged adapter executable remains reachable after unlink".to_owned(),
            ));
        }
        Err(error) => {
            return Err(ProcessError::io(
                "verify staged adapter executable unlink",
                error,
            ));
        }
    }
    write_handshake()?;
    Ok(IsolatedAdapterEntry::new(
        arguments.collect(),
        AdapterExecutableDigest::from_bytes(executable_digest),
    ))
}

#[cfg(target_os = "linux")]
fn enter_verified_adapter(
    executable: &Path,
    adapter_arguments: Vec<OsString>,
) -> Result<std::convert::Infallible, ProcessError> {
    write_handshake()?;
    let error = Command::new(executable)
        .args(adapter_arguments)
        .env_clear()
        .exec();
    Err(ProcessError::io("enter isolated adapter executable", error))
}

fn write_handshake() -> Result<(), ProcessError> {
    write_record(HANDSHAKE)
}

fn write_record(record: &[u8]) -> Result<(), ProcessError> {
    // Keep the private verification record separate from sandbox-exec and
    // adapter diagnostics, which both intentionally use standard error.
    io::stdout()
        .lock()
        .write_all(record)
        .and_then(|()| io::stdout().lock().flush())
        .map_err(|error| ProcessError::io("write isolation handshake", error))
}

fn close_inherited_descriptors() -> Result<(), ProcessError> {
    #[cfg(target_os = "linux")]
    const DESCRIPTOR_DIRECTORY: &str = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    const DESCRIPTOR_DIRECTORY: &str = "/dev/fd";

    let mut entries = fs::read_dir(DESCRIPTOR_DIRECTORY)
        .map_err(|error| ProcessError::io("enumerate inherited descriptors", error))?;
    let mut descriptors = Vec::new();
    for entry in entries.by_ref() {
        let entry =
            entry.map_err(|error| ProcessError::io("inspect inherited descriptor", error))?;
        let Some(descriptor) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if descriptor > 2 {
            descriptors.push(descriptor);
        }
    }
    drop(entries);
    for descriptor in descriptors {
        match nix::unistd::close(descriptor) {
            Ok(()) | Err(Errno::EBADF) => {}
            Err(error) => return Err(nix_error("close inherited descriptor", error)),
        }
    }
    Ok(())
}

fn parse_launcher_limit(value: Option<OsString>, label: &str) -> Result<u64, ProcessError> {
    value
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            ProcessError::InvalidInput(format!("isolated adapter {label} limit is invalid"))
        })
}

fn parse_launcher_contract(
    arguments: &mut impl Iterator<Item = OsString>,
) -> Result<(u64, u64, u64, Vec<OsString>), ProcessError> {
    let memory_bytes = parse_launcher_limit(arguments.next(), "memory")?;
    let cpu_seconds = parse_launcher_limit(arguments.next(), "CPU")?;
    let descriptor_limit = parse_launcher_limit(arguments.next(), "descriptor")?;
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(LAUNCHER_SEPARATOR)) {
        return Err(ProcessError::InvalidInput(
            "isolated adapter launcher separator is invalid".to_owned(),
        ));
    }
    Ok((
        memory_bytes,
        cpu_seconds,
        descriptor_limit,
        arguments.collect(),
    ))
}

#[cfg(target_os = "linux")]
fn establish_linux_resource_limits(
    memory_bytes: u64,
    cpu_seconds: u64,
    descriptor_limit: u64,
) -> Result<(), ProcessError> {
    let memory = rlim_t::try_from(memory_bytes).map_err(|_| {
        ProcessError::InvalidInput("adapter memory limit is not representable".to_owned())
    })?;
    let cpu = rlim_t::try_from(cpu_seconds).map_err(|_| {
        ProcessError::InvalidInput("adapter CPU limit is not representable".to_owned())
    })?;
    let descriptors = rlim_t::try_from(descriptor_limit).map_err(|_| {
        ProcessError::InvalidInput("adapter descriptor limit is not representable".to_owned())
    })?;
    setrlimit(Resource::RLIMIT_AS, memory, memory)
        .map_err(|error| nix_error("set adapter address-space limit", error))?;
    setrlimit(Resource::RLIMIT_CPU, cpu, cpu)
        .map_err(|error| nix_error("set adapter CPU limit", error))?;
    setrlimit(Resource::RLIMIT_NOFILE, descriptors, descriptors)
        .map_err(|error| nix_error("set adapter descriptor limit", error))?;
    setrlimit(Resource::RLIMIT_CORE, 0, 0)
        .map_err(|error| nix_error("disable adapter core dumps", error))?;
    setrlimit(Resource::RLIMIT_FSIZE, 0, 0)
        .map_err(|error| nix_error("disable adapter file output", error))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn establish_linux_platform_isolation() -> Result<(), ProcessError> {
    establish_linux_landlock()?;
    establish_linux_seccomp()
}

#[cfg(target_os = "macos")]
fn establish_macos_resource_limits(
    cpu_seconds: u64,
    descriptor_limit: u64,
) -> Result<(), ProcessError> {
    let cpu = rlim_t::try_from(cpu_seconds).map_err(|_| {
        ProcessError::InvalidInput("adapter CPU limit is not representable".to_owned())
    })?;
    let descriptors = rlim_t::try_from(descriptor_limit).map_err(|_| {
        ProcessError::InvalidInput("adapter descriptor limit is not representable".to_owned())
    })?;
    setrlimit(Resource::RLIMIT_CPU, cpu, cpu)
        .map_err(|error| nix_error("set adapter CPU limit", error))?;
    setrlimit(Resource::RLIMIT_NOFILE, descriptors, descriptors)
        .map_err(|error| nix_error("set adapter descriptor limit", error))?;
    setrlimit(Resource::RLIMIT_CORE, 0, 0)
        .map_err(|error| nix_error("disable adapter core dumps", error))?;
    setrlimit(Resource::RLIMIT_FSIZE, 0, 0)
        .map_err(|error| nix_error("disable adapter file output", error))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn establish_linux_landlock() -> Result<(), ProcessError> {
    use landlock::{
        ABI, Access as _, AccessFs, CompatLevel, Compatible as _, PathBeneath, PathFd, Ruleset,
        RulesetAttr as _, RulesetCreatedAttr as _, RulesetStatus,
    };

    let abi = ABI::V3;
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|error| invalid_native_policy("configure Landlock access", error))?
        .create()
        .map_err(|error| invalid_native_policy("create Landlock ruleset", error))?;
    for path in linux_read_only_paths()? {
        let access = if path.is_dir() {
            AccessFs::from_read(abi)
        } else {
            AccessFs::Execute | AccessFs::ReadFile
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(
                PathFd::new(&path)
                    .map_err(|error| invalid_native_policy("open Landlock path", error))?,
                access,
            ))
            .map_err(|error| invalid_native_policy("add Landlock path rule", error))?;
    }
    let status = ruleset
        .restrict_self()
        .map_err(|error| invalid_native_policy("enforce Landlock ruleset", error))?;
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(ProcessError::InvalidInput(
            "Landlock did not fully enforce the adapter profile".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_read_only_paths() -> Result<Vec<PathBuf>, ProcessError> {
    let executable = std::env::current_exe()
        .map_err(|error| ProcessError::io("resolve staged adapter executable", error))?;
    let directory = executable
        .parent()
        .ok_or_else(|| {
            ProcessError::InvalidInput("staged adapter executable has no parent".to_owned())
        })?
        .to_path_buf();
    let mut paths = vec![directory, executable];
    for candidate in [
        "/usr",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/dev/null",
        "/dev/urandom",
    ] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(target_os = "linux")]
fn establish_linux_seccomp() -> Result<(), ProcessError> {
    use std::collections::BTreeMap;

    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch, apply_filter_all_threads,
    };

    let mut rules = allowed_linux_syscalls()
        .into_iter()
        .map(|syscall| (syscall, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let self_process = SeccompCondition::new(0, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, 0)
        .map_err(|error| invalid_native_policy("compile self-process seccomp condition", error))?;
    let self_process_rule = SeccompRule::new(vec![self_process])
        .map_err(|error| invalid_native_policy("compile self-process seccomp rule", error))?;
    rules.insert(libc::SYS_prlimit64, vec![self_process_rule.clone()]);
    rules.insert(libc::SYS_sched_getaffinity, vec![self_process_rule]);
    let target = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|error| invalid_native_policy("select seccomp architecture", error))?;
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM.cast_unsigned()),
        SeccompAction::Allow,
        target,
    )
    .map_err(|error| invalid_native_policy("compile seccomp policy", error))?;
    let program = BpfProgram::try_from(filter)
        .map_err(|error| invalid_native_policy("compile seccomp BPF", error))?;
    apply_filter_all_threads(&program)
        .map_err(|error| invalid_native_policy("install seccomp policy", error))
}

#[cfg(target_os = "linux")]
fn allowed_linux_syscalls() -> Vec<i64> {
    let syscalls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_lseek,
        libc::SYS_pread64,
        libc::SYS_fcntl,
        libc::SYS_ioctl,
        libc::SYS_dup,
        libc::SYS_dup3,
        libc::SYS_pipe2,
        libc::SYS_openat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_getdents64,
        libc::SYS_readlinkat,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_getcwd,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_futex,
        libc::SYS_membarrier,
        libc::SYS_ppoll,
        libc::SYS_sched_yield,
        libc::SYS_nanosleep,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_restart_syscall,
        libc::SYS_getrandom,
        libc::SYS_uname,
        libc::SYS_sysinfo,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_getrlimit,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    #[cfg(target_arch = "x86_64")]
    {
        let mut extended = syscalls;
        extended.extend([
            libc::SYS_arch_prctl,
            libc::SYS_access,
            libc::SYS_poll,
            libc::SYS_readlink,
        ]);
        extended
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        syscalls
    }
}

#[cfg(target_os = "macos")]
const MACOS_PROFILE_PREAMBLE: &str = r#"(version 1)
(deny default)
"#;

#[cfg(target_os = "macos")]
fn macos_profile(executable: &Path, workspace: &Path) -> String {
    let executable = sandbox_literal(executable);
    let workspace = sandbox_literal(workspace);
    format!(
        r#"{MACOS_PROFILE_PREAMBLE}(allow process-info* (target self))
(allow signal (target self))
(allow sysctl-read)
(allow file-read*
    (literal "{executable}")
    (subpath "{workspace}")
    (subpath "/usr/lib")
    (subpath "/System/Library")
    (subpath "/private/var/db/dyld")
    (literal "/dev/null")
    (literal "/dev/urandom"))
(deny network*)
(deny process-fork)
"#
    )
}

#[cfg(target_os = "macos")]
fn sandbox_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(target_os = "linux")]
fn platform_report() -> AdapterIsolationReport {
    AdapterIsolationReport::linux_isolated_process()
}

#[cfg(target_os = "macos")]
fn platform_report() -> AdapterIsolationReport {
    AdapterIsolationReport::macos_isolated_process()
}

#[cfg(target_os = "linux")]
fn invalid_native_policy(operation: &'static str, error: impl std::fmt::Display) -> ProcessError {
    ProcessError::InvalidInput(format!("{operation} failed: {error}"))
}

fn nix_error(operation: &'static str, error: Errno) -> ProcessError {
    ProcessError::io(operation, io::Error::from_raw_os_error(error as i32))
}

fn validate_command(command: &ProcessCommand) -> Result<(), ProcessError> {
    if !command.program.is_absolute() {
        return Err(ProcessError::InvalidInput(
            "the executable path must be absolute".to_owned(),
        ));
    }
    if let Some(directory) = &command.current_directory
        && !directory.is_absolute()
    {
        return Err(ProcessError::InvalidInput(
            "the working directory must be absolute".to_owned(),
        ));
    }
    Ok(())
}

fn stdio(mode: StdioMode) -> Stdio {
    match mode {
        StdioMode::Null => Stdio::null(),
        StdioMode::Piped => Stdio::piped(),
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Read as _, os::unix::fs::symlink};

    #[cfg(target_os = "macos")]
    use std::io::Write as _;

    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;

    use super::*;

    #[test]
    fn executable_busy_spawn_is_retried_with_a_hard_limit() {
        let mut attempts = 0_usize;
        let value = retry_executable_busy(|| {
            attempts += 1;
            if attempts <= 2 {
                Err(io::Error::from_raw_os_error(libc::ETXTBSY))
            } else {
                Ok(42_u8)
            }
        })
        .expect("transient executable lease clears");

        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn unrelated_spawn_error_is_not_retried() {
        let mut attempts = 0_usize;
        let error = retry_executable_busy(|| {
            attempts += 1;
            Err::<(), _>(io::Error::from_raw_os_error(libc::ENOENT))
        })
        .expect_err("unrelated spawn error remains terminal");

        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        assert_eq!(attempts, 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_grants_no_filesystem_writes() {
        let profile = macos_profile(
            Path::new("/private/tmp/rootlight-adapter/adapter"),
            Path::new("/private/tmp/rootlight-adapter"),
        );

        assert!(profile.starts_with("(version 1)\n(deny default)\n"));
        assert!(!profile.contains("(import "));
        assert!(!profile.contains("(allow file-write"));
        assert!(!profile.contains("(allow process-exec"));
        assert!(profile.contains("(deny network*)"));
        assert!(profile.contains("(deny process-fork)"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_compiles_in_fresh_process() {
        const PROBE_ENVIRONMENT: &str = "ROOTLIGHT_TEST_MACOS_PROFILE_STAGE";
        const TEST_NAME: &str = "platform::unix::tests::macos_profile_compiles_in_fresh_process";
        const STAGES: [&str; 7] = [
            "baseline",
            "process-info",
            "self-signal",
            "sysctl-read",
            "filesystem-read",
            "network-deny",
            "process-fork-deny",
        ];

        if let Some(stage) = std::env::var_os(PROBE_ENVIRONMENT) {
            let stage = stage
                .into_string()
                .ok()
                .and_then(|stage| stage.parse::<usize>().ok())
                .filter(|stage| *stage < STAGES.len())
                .expect("profile probe stage is valid");
            let marker = format!("rootlight-seatbelt-profile-probe/{stage}");
            eprintln!("{marker}");
            io::stderr().flush().expect("profile probe marker flushes");
            match macos::enter_sandbox(&macos_profile_prefix(stage)) {
                Ok(()) => {
                    // Anonymous standard streams remain usable without granting
                    // the sandbox access to any filesystem write operation.
                    let mut input = [0_u8; 1];
                    io::stdin()
                        .read_exact(&mut input)
                        .expect("sandboxed stdin remains readable");
                    assert_eq!(input, [b'R']);
                    println!("{marker}/stdout");
                    io::stdout()
                        .flush()
                        .expect("sandboxed stdout remains writable");
                    eprintln!("{marker}/stderr");
                    io::stderr()
                        .flush()
                        .expect("sandboxed stderr remains writable");
                    std::process::exit(0);
                }
                Err(_) => std::process::exit(70),
            }
        }

        // Seatbelt is process-wide and irreversible, so each cumulative
        // profile must be compiled in a separate test process.
        let executable = std::env::current_exe().expect("current test executable resolves");
        for (stage, label) in STAGES.into_iter().enumerate() {
            let marker = format!("rootlight-seatbelt-profile-probe/{stage}");
            let mut child = Command::new(&executable)
                .env_clear()
                .env(PROBE_ENVIRONMENT, stage.to_string())
                .arg("--exact")
                .arg(TEST_NAME)
                .arg("--nocapture")
                .arg("--test-threads=1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("profile probe process starts");
            child
                .stdin
                .take()
                .expect("profile probe stdin is piped")
                .write_all(b"R")
                .expect("profile probe input is written");
            let output = child
                .wait_with_output()
                .expect("profile probe process completes");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains(&format!("{marker}/stdout"))
                    && stderr.contains(&format!("{marker}/stderr")),
                "Darwin Seatbelt profile stage `{label}` failed validation \
                 (status={}, stdout={stdout}, stderr={stderr})",
                output.status
            );
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_profile_prefix(stage: usize) -> String {
        const FRAGMENTS: [&str; 6] = [
            "(allow process-info* (target self))\n",
            "(allow signal (target self))\n",
            "(allow sysctl-read)\n",
            r#"(allow file-read*
    (literal "/private/tmp/rootlight-adapter/adapter")
    (subpath "/private/tmp/rootlight-adapter")
    (subpath "/usr/lib")
    (subpath "/System/Library")
    (subpath "/private/var/db/dyld")
    (literal "/dev/null")
    (literal "/dev/urandom"))
"#,
            "(deny network*)\n",
            "(deny process-fork)\n",
        ];

        let mut profile = String::from(MACOS_PROFILE_PREAMBLE);
        for fragment in &FRAGMENTS[..stage] {
            profile.push_str(fragment);
        }
        profile
    }

    #[test]
    fn persistent_executable_busy_error_exhausts_the_retry_budget() {
        let mut attempts = 0_usize;
        let error = retry_executable_busy(|| {
            attempts += 1;
            Err::<(), _>(io::Error::from_raw_os_error(libc::ETXTBSY))
        })
        .expect_err("persistent executable lease remains terminal");

        assert_eq!(error.raw_os_error(), Some(libc::ETXTBSY));
        assert_eq!(attempts, EXECUTABLE_BUSY_RETRY_LIMIT + 1);
    }

    #[test]
    fn opened_handle_remains_bound_when_path_is_replaced() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter");
        let moved = directory.path().join("adapter-opened");
        let original = b"authenticated executable";
        fs::write(&source, original).expect("fixture executable writes");
        let expected = executable_digest(original);
        let (mut opened, declared_bytes) =
            open_adapter_executable(&source).expect("fixture executable opens");

        fs::rename(&source, &moved).expect("opened fixture path renames");
        fs::write(&source, b"replacement executable").expect("replacement executable writes");
        let mut staged = Vec::new();
        let observed =
            copy_authenticated_executable(&mut opened, declared_bytes, Some(expected), &mut staged)
                .expect("opened executable stages");

        assert_eq!(observed, expected);
        assert_eq!(staged, original);
    }

    #[test]
    fn executable_symlink_is_rejected_without_following_it() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let target = directory.path().join("target");
        let link = directory.path().join("adapter");
        fs::write(&target, b"executable").expect("fixture target writes");
        symlink(&target, &link).expect("fixture symlink creates");

        assert!(open_adapter_executable(&link).is_err());
    }

    #[test]
    fn executable_fifo_is_rejected_without_blocking_for_a_writer() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let fifo = directory.path().join("adapter");
        mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).expect("fixture FIFO creates");

        assert!(matches!(
            open_adapter_executable(&fifo),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn sparse_oversize_executable_is_rejected_before_copying() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter");
        let file = File::create(&source).expect("fixture executable creates");
        file.set_len(MAX_ADAPTER_EXECUTABLE_BYTES + 1)
            .expect("fixture executable becomes sparse");

        assert!(matches!(
            open_adapter_executable(&source),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn negotiated_digest_mismatch_is_rejected_before_spawn() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter");
        fs::write(&source, b"unexpected executable").expect("fixture executable writes");
        let expected = executable_digest(b"negotiated executable");

        assert!(matches!(
            ImmutableWorkspace::stage(&source, Some(expected)),
            Err(ProcessError::InvalidInput(_))
        ));
    }

    #[test]
    fn staged_workspace_uses_one_canonical_path_identity() {
        let directory = tempfile::tempdir().expect("fixture directory opens");
        let source = directory.path().join("adapter");
        fs::write(&source, b"authenticated executable").expect("fixture executable writes");

        let workspace =
            ImmutableWorkspace::stage(&source, None).expect("fixture executable stages");

        assert_eq!(
            workspace.root,
            fs::canonicalize(workspace._directory.path()).expect("workspace canonicalizes")
        );
        assert_eq!(workspace.executable, workspace.root.join(STAGED_EXECUTABLE));
        assert_eq!(
            workspace.executable,
            fs::canonicalize(&workspace.executable).expect("executable canonicalizes")
        );
    }

    #[test]
    fn launcher_handshake_is_separate_from_diagnostics_and_adapter_output() {
        let script = format!(
            "printf wrapper-diagnostic >&2; printf '{}'; printf adapter-output",
            String::from_utf8_lossy(HANDSHAKE)
        );
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("fixture launcher starts");
        let mut stdout = child.stdout.take().expect("fixture stdout exists");
        let mut stderr = child.stderr.take().expect("fixture stderr exists");

        verify_record(&mut stdout, &mut child, HANDSHAKE).expect("handshake verifies");

        let mut adapter_output = Vec::new();
        stdout
            .read_to_end(&mut adapter_output)
            .expect("adapter output reads");
        let mut diagnostics = Vec::new();
        stderr
            .read_to_end(&mut diagnostics)
            .expect("diagnostics read");
        assert!(child.wait().expect("fixture launcher exits").success());
        assert_eq!(adapter_output, b"adapter-output");
        assert_eq!(diagnostics, b"wrapper-diagnostic");
    }

    fn executable_digest(bytes: &[u8]) -> AdapterExecutableDigest {
        AdapterExecutableDigest::from_bytes(*blake3::hash(bytes).as_bytes())
    }
}
