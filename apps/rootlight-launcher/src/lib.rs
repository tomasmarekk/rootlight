//! Stable routing from public Rootlight commands to side-by-side payloads.
//!
//! The launcher reads only bounded, strict installation state. Incomplete or
//! malformed update transactions always route to the retained last-good payload.

#![forbid(unsafe_code)]

#[cfg(windows)]
mod mcp_bootstrap;

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File},
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

#[cfg(any(test, windows))]
use std::fs::OpenOptions;
#[cfg(windows)]
use std::{
    ffi::OsString,
    io::{BufRead as _, BufReader, Write as _},
    process::{Child, ChildStdin, ChildStdout},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use mcp_bootstrap::{
    DEFAULT_MAX_FRAME_BYTES, DEFAULT_MAX_RESPONSE_BYTES, bootstrap_initialize,
    exposure_profile_policy_is_valid,
};
use semver::Version;
use serde::Deserialize;

const INSTALL_MANIFEST_SCHEMA_V1: &str = "rootlight.install-ownership/1";
const INSTALL_MANIFEST_SCHEMA_V2: &str = "rootlight.install-ownership/2";
const UPDATE_TRANSACTION_SCHEMA: &str = "rootlight.update-transaction/1";
#[cfg(windows)]
const DEFERRED_UNINSTALL_SCHEMA: &str = "rootlight.deferred-uninstall/1";
#[cfg(windows)]
const DEFERRED_UNINSTALL_FILE: &str = "deferred-uninstall.json";
#[cfg(windows)]
const DEFERRED_UNINSTALL_TOKEN_ENV: &str = "ROOTLIGHT_DEFERRED_UNINSTALL_TOKEN";
#[cfg(windows)]
const DEFERRED_UNINSTALL_LAUNCHER_PID_ENV: &str = "ROOTLIGHT_DEFERRED_UNINSTALL_LAUNCHER_PID";
#[cfg(windows)]
const INTERNAL_UNINSTALL_HELPER: &str = "--rootlight-internal-uninstall-helper";
#[cfg(windows)]
const MCP_PROFILE_CEILING_ENV: &str = "ROOTLIGHT_MCP_PROFILE_CEILING";
#[cfg(windows)]
const MCP_PROFILE_ENV: &str = "ROOTLIGHT_MCP_PROFILE";
#[cfg(windows)]
const MCP_INTERNAL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(windows)]
const MCP_INTERNAL_POLL_INTERVAL: Duration = Duration::from_millis(2);
const MAX_ACTIVE_VERSION_BYTES: u64 = 128;
const MAX_INSTALL_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_UPDATE_TRANSACTION_BYTES: u64 = 1024 * 1024;
const MAX_OWNED_PATHS: usize = 256;
const MAX_PATH_BYTES: usize = 512;
#[cfg(windows)]
const UNINSTALL_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(windows)]
const UNINSTALL_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EXPECTED_BINARIES: [&str; 5] = [
    "rootlight",
    "rootlight-adapter-host",
    "rootlight-daemon",
    "rootlight-mcp",
    "rootlight-semantic-host",
];

/// Runs the launcher for the public command name used to invoke this process.
///
/// On Unix, success replaces the launcher process with the selected payload.
/// On Windows, the launcher waits for the payload and returns its bounded exit
/// status.
///
/// # Errors
///
/// Returns [`LauncherError`] when the launcher layout, bounded state, retained
/// fallback, payload identity, process creation, or process wait fails.
pub fn run() -> Result<ExitCode, LauncherError> {
    let executable = std::env::current_exe().map_err(LauncherError::CurrentExecutable)?;
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    #[cfg(windows)]
    if let Some((root, token, launcher_pid)) = internal_uninstall_arguments(&arguments)? {
        return run_uninstall_helper(&executable, &root, &token, launcher_pid);
    }
    let binary = invocation_binary_name(executable.as_os_str())?;
    let install_root = install_root(&executable)?;
    let resolved = resolve_payload(&install_root, binary)?;
    let target = &resolved.executable;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        let error = Command::new(target)
            .args(arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .exec();
        Err(LauncherError::Spawn(error))
    }
    #[cfg(windows)]
    {
        if binary == "rootlight-mcp" && arguments.is_empty() {
            return run_lazy_mcp_payload(target, &resolved.version);
        }
        let mut command = Command::new(target);
        command
            .args(&arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut helper = if installed_uninstall_root(binary, &arguments, &install_root)? {
            let helper = DeferredUninstallHelper::spawn(&executable, &install_root)?;
            command
                .env(DEFERRED_UNINSTALL_TOKEN_ENV, &helper.token)
                .env(
                    DEFERRED_UNINSTALL_LAUNCHER_PID_ENV,
                    std::process::id().to_string(),
                );
            Some(helper)
        } else {
            None
        };
        let status = match command.status() {
            Ok(status) => status,
            Err(error) => {
                if let Some(helper) = helper.as_mut() {
                    helper.cancel();
                }
                return Err(LauncherError::Spawn(error));
            }
        };
        if let Some(helper) = helper.as_mut() {
            if status.success() {
                helper.ensure_running()?;
            } else {
                helper.cancel();
            }
        }
        let code = status
            .code()
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(1);
        Ok(ExitCode::from(code))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, arguments);
        Err(LauncherError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
fn run_lazy_mcp_payload(target: &Path, server_version: &str) -> Result<ExitCode, LauncherError> {
    let mut input = BufReader::new(io::stdin());
    let first_input = read_mcp_input_prefix(&mut input)?;
    if first_input.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    let Some(frame) = first_input.strip_suffix(b"\n") else {
        return proxy_mcp_payload(target, input, first_input, Vec::new(), None);
    };
    let ceiling = std::env::var_os(MCP_PROFILE_CEILING_ENV);
    let requested = std::env::var_os(MCP_PROFILE_ENV);
    if !exposure_profile_policy_is_valid(ceiling.as_deref(), requested.as_deref()) {
        return proxy_mcp_payload(target, input, first_input, Vec::new(), None);
    }
    let Some(bootstrap) = bootstrap_initialize(frame, server_version)
        .map_err(|_error| LauncherError::McpBootstrap)?
    else {
        return proxy_mcp_payload(target, input, first_input, Vec::new(), None);
    };
    let response = bootstrap.response();
    let expected = response
        .strip_suffix(b"\n")
        .filter(|frame| !frame.is_empty() && !frame.contains(&b'\n'))
        .ok_or(LauncherError::McpProxyInvariant)?
        .to_vec();

    {
        let mut output = io::stdout().lock();
        output
            .write_all(response)
            .and_then(|()| output.flush())
            .map_err(LauncherError::McpProxy)?;
    }

    // A session that closes after `initialize` has no operating work, so the
    // versioned payload never needs to enter the image loader or security scan.
    let pending_input = read_mcp_input_prefix(&mut input)?;
    if pending_input.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    // The full payload must reproduce the frozen initialize ABI exactly before
    // buffered operating input is released, preventing silent launcher/payload
    // protocol drift across side-by-side updates.
    proxy_mcp_payload(target, input, first_input, pending_input, Some(expected))
}

#[cfg(windows)]
fn read_mcp_input_prefix(reader: &mut impl io::BufRead) -> Result<Vec<u8>, LauncherError> {
    let maximum = u64::try_from(DEFAULT_MAX_FRAME_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut frame = Vec::new();
    reader
        .take(maximum)
        .read_until(b'\n', &mut frame)
        .map_err(LauncherError::McpProxy)?;
    Ok(frame)
}

#[cfg(windows)]
fn proxy_mcp_payload(
    target: &Path,
    input: BufReader<io::Stdin>,
    first_input: Vec<u8>,
    pending_input: Vec<u8>,
    expected_initialize: Option<Vec<u8>>,
) -> Result<ExitCode, LauncherError> {
    let mut child = Command::new(target)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(LauncherError::Spawn)?;
    let Some(mut child_input) = child.stdin.take() else {
        terminate_mcp_child(&mut child);
        return Err(LauncherError::McpProxyInvariant);
    };
    let Some(child_output) = child.stdout.take() else {
        terminate_mcp_child(&mut child);
        return Err(LauncherError::McpProxyInvariant);
    };
    let mut child_output = BufReader::new(child_output);

    if let Err(error) = child_input
        .write_all(&first_input)
        .and_then(|()| child_input.flush())
    {
        terminate_mcp_child(&mut child);
        return Err(LauncherError::McpProxy(error));
    }

    if let Some(expected) = expected_initialize {
        let (observed, returned_output) =
            match read_mcp_output_frame_timed(&mut child, child_output) {
                Ok(observed) => observed,
                Err(error) => {
                    terminate_mcp_child(&mut child);
                    return Err(error);
                }
            };
        child_output = returned_output;
        if observed.strip_suffix(b"\n") != Some(expected.as_slice()) {
            terminate_mcp_child(&mut child);
            return Err(LauncherError::McpProxyInvariant);
        }
        if let Err(error) = child_input
            .write_all(&pending_input)
            .and_then(|()| child_input.flush())
        {
            terminate_mcp_child(&mut child);
            return Err(LauncherError::McpProxy(error));
        }
    }

    let input_worker = match spawn_mcp_input_proxy(input, child_input) {
        Ok(worker) => worker,
        Err(error) => {
            terminate_mcp_child(&mut child);
            return Err(error);
        }
    };
    let output_worker = match spawn_mcp_output_proxy(child_output) {
        Ok(worker) => worker,
        Err(error) => {
            terminate_mcp_child(&mut child);
            return Err(error);
        }
    };
    let mut input_worker = Some(input_worker);
    let mut output_worker = Some(output_worker);
    let mut settlement_deadline = None;
    let status = loop {
        if input_worker
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            input_worker
                .take()
                .ok_or(LauncherError::McpProxyInvariant)?
                .join()
                .map_err(|_| LauncherError::McpProxyThread)?
                .map_err(LauncherError::McpProxy)?;
            settlement_deadline.get_or_insert_with(|| {
                Instant::now()
                    .checked_add(MCP_INTERNAL_HANDSHAKE_TIMEOUT)
                    .unwrap_or_else(Instant::now)
            });
        }
        if output_worker
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            let result = output_worker
                .take()
                .ok_or(LauncherError::McpProxyInvariant)?
                .join()
                .map_err(|_| LauncherError::McpProxyThread)?;
            if let Err(error) = result {
                terminate_mcp_child(&mut child);
                return Err(LauncherError::McpProxy(error));
            }
            settlement_deadline.get_or_insert_with(|| {
                Instant::now()
                    .checked_add(MCP_INTERNAL_HANDSHAKE_TIMEOUT)
                    .unwrap_or_else(Instant::now)
            });
        }
        if let Some(status) = child.try_wait().map_err(LauncherError::McpProxy)? {
            break status;
        }
        if settlement_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            terminate_mcp_child(&mut child);
            if let Some(worker) = output_worker.take() {
                let _ = worker.join();
            }
            return Err(LauncherError::McpProxyTimeout);
        }
        thread::sleep(MCP_INTERNAL_POLL_INTERVAL);
    };
    if let Some(worker) = output_worker {
        worker
            .join()
            .map_err(|_| LauncherError::McpProxyThread)?
            .map_err(LauncherError::McpProxy)?;
    }
    if input_worker
        .as_ref()
        .is_some_and(thread::JoinHandle::is_finished)
    {
        input_worker
            .take()
            .ok_or(LauncherError::McpProxyInvariant)?
            .join()
            .map_err(|_| LauncherError::McpProxyThread)?
            .map_err(LauncherError::McpProxy)?;
    }
    let code = status
        .code()
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(1);
    Ok(ExitCode::from(code))
}

#[cfg(windows)]
fn read_mcp_output_frame_timed(
    child: &mut Child,
    mut reader: BufReader<ChildStdout>,
) -> Result<(Vec<u8>, BufReader<ChildStdout>), LauncherError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("rootlight-mcp-initialize-output".to_owned())
        .spawn(move || {
            let result = read_mcp_output_frame(&mut reader);
            let _ = sender.send((result, reader));
        })
        .map_err(LauncherError::McpProxy)?;
    let received = receiver.recv_timeout(MCP_INTERNAL_HANDSHAKE_TIMEOUT);
    match received {
        Ok((result, reader)) => {
            worker.join().map_err(|_| LauncherError::McpProxyThread)?;
            result.map(|frame| (frame, reader))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            terminate_mcp_child(child);
            let _ = worker.join();
            Err(LauncherError::McpProxyTimeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            terminate_mcp_child(child);
            let _ = worker.join();
            Err(LauncherError::McpProxyThread)
        }
    }
}

#[cfg(windows)]
fn read_mcp_output_frame(reader: &mut impl io::BufRead) -> Result<Vec<u8>, LauncherError> {
    let maximum = u64::try_from(DEFAULT_MAX_RESPONSE_BYTES).unwrap_or(u64::MAX);
    let mut frame = Vec::new();
    reader
        .take(maximum.saturating_add(1))
        .read_until(b'\n', &mut frame)
        .map_err(LauncherError::McpProxy)?;
    if frame.is_empty() || frame.len() > DEFAULT_MAX_RESPONSE_BYTES || frame.last() != Some(&b'\n')
    {
        return Err(LauncherError::McpProxyInvariant);
    }
    Ok(frame)
}

#[cfg(windows)]
fn spawn_mcp_input_proxy(
    mut input: BufReader<io::Stdin>,
    mut child_input: ChildStdin,
) -> Result<thread::JoinHandle<io::Result<()>>, LauncherError> {
    thread::Builder::new()
        .name("rootlight-mcp-input".to_owned())
        .spawn(move || match io::copy(&mut input, &mut child_input) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error),
        })
        .map_err(LauncherError::McpProxy)
}

#[cfg(windows)]
fn spawn_mcp_output_proxy(
    mut child_output: BufReader<ChildStdout>,
) -> Result<thread::JoinHandle<io::Result<()>>, LauncherError> {
    thread::Builder::new()
        .name("rootlight-mcp-output".to_owned())
        .spawn(move || {
            let mut output = io::stdout().lock();
            if let Err(error) = io::copy(&mut child_output, &mut output) {
                // Keep draining the trusted child so it cannot deadlock while
                // the launcher reports a closed or failed client output pipe.
                let _ = io::copy(&mut child_output, &mut io::sink());
                return Err(error);
            }
            output.flush()
        })
        .map_err(LauncherError::McpProxy)
}

#[cfg(windows)]
fn terminate_mcp_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(windows)]
fn internal_uninstall_arguments(
    arguments: &[OsString],
) -> Result<Option<(PathBuf, String, u32)>, LauncherError> {
    let Some(flag) = arguments.first().and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    if flag != INTERNAL_UNINSTALL_HELPER {
        return Ok(None);
    }
    let [_, root, token, launcher_pid] = arguments else {
        return Err(LauncherError::InvalidInvocation);
    };
    let token = token.to_str().ok_or(LauncherError::InvalidInvocation)?;
    let launcher_pid = launcher_pid
        .to_str()
        .ok_or(LauncherError::InvalidInvocation)?
        .parse()
        .map_err(|_| LauncherError::InvalidInvocation)?;
    if !lower_hex(token, 32) || launcher_pid == 0 {
        return Err(LauncherError::InvalidInvocation);
    }
    Ok(Some((PathBuf::from(root), token.to_owned(), launcher_pid)))
}

#[cfg(windows)]
fn installed_uninstall_root(
    binary: &str,
    arguments: &[OsString],
    install_root: &Path,
) -> Result<bool, LauncherError> {
    let [update, uninstall, root_flag, requested_root] = arguments else {
        return Ok(false);
    };
    if binary != "rootlight"
        || update != "update"
        || uninstall != "uninstall"
        || root_flag != "--root"
    {
        return Ok(false);
    }
    let requested =
        fs::canonicalize(requested_root).map_err(LauncherError::UninstallPreparation)?;
    let actual = fs::canonicalize(install_root).map_err(LauncherError::UninstallPreparation)?;
    Ok(requested == actual)
}

#[cfg(windows)]
struct DeferredUninstallHelper {
    child: Child,
    directory: PathBuf,
    token: String,
}

#[cfg(windows)]
impl DeferredUninstallHelper {
    fn spawn(executable: &Path, install_root: &Path) -> Result<Self, LauncherError> {
        use std::os::windows::process::CommandExt as _;
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;

        let token = uninstall_token()?;
        let directory = create_private_helper_directory(&token)?;
        let helper_path = directory.join("rootlight-uninstall-helper.exe");
        if let Err(error) = copy_private_helper(executable, &helper_path) {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        let child = Command::new(&helper_path)
            .arg(INTERNAL_UNINSTALL_HELPER)
            .arg(install_root)
            .arg(&token)
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .map_err(|source| {
                let _ = fs::remove_file(&helper_path);
                let _ = fs::remove_dir(&directory);
                LauncherError::UninstallPreparation(source)
            })?;
        Ok(Self {
            child,
            directory,
            token,
        })
    }

    fn ensure_running(&mut self) -> Result<(), LauncherError> {
        if self
            .child
            .try_wait()
            .map_err(LauncherError::UninstallPreparation)?
            .is_some()
        {
            return Err(LauncherError::UninstallHelperExited);
        }
        Ok(())
    }

    fn cancel(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(windows)]
fn uninstall_token() -> Result<String, LauncherError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LauncherError::UninstallClock)?;
    let time_bits = elapsed.as_nanos() & ((1_u128 << 96) - 1);
    Ok(format!("{:08x}{time_bits:024x}", std::process::id()))
}

#[cfg(windows)]
fn create_private_helper_directory(token: &str) -> Result<PathBuf, LauncherError> {
    let temporary =
        fs::canonicalize(std::env::temp_dir()).map_err(LauncherError::UninstallPreparation)?;
    validate_directory_no_reparse(&temporary)?;
    let directory = temporary.join(format!("rootlight-uninstall-{token}"));
    fs::create_dir(&directory).map_err(LauncherError::UninstallPreparation)?;
    if let Err(error) = apply_private_windows_dacl(&directory, true)
        .and_then(|()| verify_private_windows_dacl(&directory))
    {
        let _ = fs::remove_dir(&directory);
        return Err(error);
    }
    let canonical = fs::canonicalize(&directory).map_err(LauncherError::UninstallPreparation)?;
    if canonical.parent() != Some(temporary.as_path()) {
        let _ = fs::remove_dir(&canonical);
        return Err(LauncherError::InvalidHelperLocation);
    }
    Ok(canonical)
}

#[cfg(windows)]
fn copy_private_helper(source: &Path, destination: &Path) -> Result<(), LauncherError> {
    validate_payload(source)?;
    let mut source = open_regular_no_follow(source).map_err(LauncherError::UninstallPreparation)?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(LauncherError::UninstallPreparation)?;
    io::copy(&mut source, &mut destination_file).map_err(LauncherError::UninstallPreparation)?;
    destination_file
        .sync_all()
        .map_err(LauncherError::UninstallPreparation)?;
    drop(destination_file);
    apply_private_windows_dacl(destination, false)?;
    verify_private_windows_dacl(destination)?;
    validate_payload(destination)
}

#[cfg(windows)]
fn run_uninstall_helper(
    executable: &Path,
    install_root: &Path,
    token: &str,
    launcher_pid: u32,
) -> Result<ExitCode, LauncherError> {
    validate_helper_location(executable, token)?;
    let root = fs::canonicalize(install_root).map_err(LauncherError::UninstallCleanup)?;
    validate_private_install_root(&root)?;
    let request = wait_for_uninstall_request(&root, token, launcher_pid)?;
    wait_for_process_exit(request.launcher_pid, request.payload_pid)?;
    let cleanup_result = cleanup_deferred_installation(&root, &request);
    let self_cleanup_result = schedule_helper_self_cleanup(executable);
    cleanup_result?;
    self_cleanup_result?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(windows)]
fn validate_helper_location(executable: &Path, token: &str) -> Result<(), LauncherError> {
    let temporary =
        fs::canonicalize(std::env::temp_dir()).map_err(LauncherError::UninstallPreparation)?;
    let executable = fs::canonicalize(executable).map_err(LauncherError::UninstallPreparation)?;
    let directory = executable
        .parent()
        .ok_or(LauncherError::InvalidHelperLocation)?;
    if directory.parent() != Some(temporary.as_path())
        || directory.file_name()
            != Some(OsStr::new(format!("rootlight-uninstall-{token}").as_str()))
        || executable.file_name() != Some(OsStr::new("rootlight-uninstall-helper.exe"))
    {
        return Err(LauncherError::InvalidHelperLocation);
    }
    validate_directory_no_reparse(directory)?;
    validate_payload(&executable)?;
    verify_private_windows_dacl(directory)?;
    verify_private_windows_dacl(&executable)
}

#[cfg(windows)]
fn validate_private_install_root(root: &Path) -> Result<(), LauncherError> {
    validate_directory_no_reparse(root)?;
    verify_private_windows_dacl(root)?;
    for name in ["state", "versions", "current", "current/bin"] {
        let path = root.join(name);
        validate_directory_no_reparse(&path)?;
        verify_private_windows_dacl(&path)?;
    }
    Ok(())
}

#[cfg(windows)]
fn wait_for_uninstall_request(
    root: &Path,
    token: &str,
    launcher_pid: u32,
) -> Result<DeferredUninstallRequest, LauncherError> {
    let deadline = Instant::now()
        .checked_add(UNINSTALL_TIMEOUT)
        .ok_or(LauncherError::UninstallClock)?;
    let request_path = root.join("state").join(DEFERRED_UNINSTALL_FILE);
    loop {
        match read_optional_regular_bounded(&request_path, MAX_UPDATE_TRANSACTION_BYTES)? {
            Some(bytes) => {
                verify_private_windows_dacl(&request_path)?;
                let request: DeferredUninstallRequest =
                    serde_json::from_slice(&bytes).map_err(|_| LauncherError::InvalidState)?;
                request.validate(root, token, launcher_pid)?;
                return Ok(request);
            }
            None if Instant::now() < deadline => thread::sleep(UNINSTALL_POLL_INTERVAL),
            None => return Err(LauncherError::UninstallTimedOut),
        }
    }
}

#[cfg(windows)]
fn wait_for_process_exit(launcher_pid: u32, payload_pid: u32) -> Result<(), LauncherError> {
    use std::os::windows::process::CommandExt as _;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    const WAIT_SCRIPT: &str = "$ErrorActionPreference='Stop';$ids=@([uint32]$env:ROOTLIGHT_LAUNCHER_PID,[uint32]$env:ROOTLIGHT_PAYLOAD_PID);$deadline=[DateTime]::UtcNow.AddSeconds(120);while([DateTime]::UtcNow -lt $deadline){$alive=$false;foreach($processId in $ids){if(Get-Process -Id $processId -ErrorAction SilentlyContinue){$alive=$true}};if(-not $alive){exit 0};Start-Sleep -Milliseconds 50};exit 1";

    let status = Command::new(powershell_path()?)
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(WAIT_SCRIPT)
        .env("ROOTLIGHT_LAUNCHER_PID", launcher_pid.to_string())
        .env("ROOTLIGHT_PAYLOAD_PID", payload_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .status()
        .map_err(LauncherError::UninstallCleanup)?;
    if status.success() {
        Ok(())
    } else {
        Err(LauncherError::UninstallTimedOut)
    }
}

#[cfg(windows)]
fn powershell_path() -> Result<PathBuf, LauncherError> {
    let system_root = std::env::var_os("SystemRoot").ok_or(LauncherError::InvalidHelperLocation)?;
    let system_root = fs::canonicalize(system_root).map_err(LauncherError::UninstallPreparation)?;
    validate_directory_no_reparse(&system_root)?;
    let executable = system_root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let executable = fs::canonicalize(executable).map_err(LauncherError::UninstallPreparation)?;
    validate_payload(&executable)?;
    Ok(executable)
}

#[cfg(windows)]
fn schedule_helper_self_cleanup(executable: &Path) -> Result<(), LauncherError> {
    use std::os::windows::process::CommandExt as _;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;

    const CLEANUP_SCRIPT: &str = "$ErrorActionPreference='SilentlyContinue';Wait-Process -Id ([uint32]$env:ROOTLIGHT_HELPER_PID);for($attempt=0;$attempt -lt 100;$attempt++){Remove-Item -LiteralPath $env:ROOTLIGHT_HELPER_EXE -Force;if(-not (Test-Path -LiteralPath $env:ROOTLIGHT_HELPER_EXE)){break};Start-Sleep -Milliseconds 50};Remove-Item -LiteralPath $env:ROOTLIGHT_HELPER_DIR -Force";

    let executable = fs::canonicalize(executable).map_err(LauncherError::UninstallPreparation)?;
    let directory = executable
        .parent()
        .ok_or(LauncherError::InvalidHelperLocation)?
        .to_path_buf();
    Command::new(powershell_path()?)
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(CLEANUP_SCRIPT)
        .env("ROOTLIGHT_HELPER_PID", std::process::id().to_string())
        .env("ROOTLIGHT_HELPER_EXE", &executable)
        .env("ROOTLIGHT_HELPER_DIR", &directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .map(|_| ())
        .map_err(LauncherError::UninstallCleanup)
}

#[cfg(windows)]
fn cleanup_deferred_installation(
    root: &Path,
    expected_request: &DeferredUninstallRequest,
) -> Result<(), LauncherError> {
    use fs2::FileExt as _;

    let state = root.join("state");
    let lock_path = state.join("update.lock");
    verify_private_windows_dacl(&lock_path)?;
    let lock = open_regular_no_follow(&lock_path).map_err(LauncherError::UninstallCleanup)?;
    lock.try_lock_exclusive()
        .map_err(|_| LauncherError::UninstallBusy)?;

    validate_private_install_root(root)?;
    let request_path = state.join(DEFERRED_UNINSTALL_FILE);
    verify_private_windows_dacl(&request_path)?;
    let request_bytes = read_regular_bounded(&request_path, MAX_UPDATE_TRANSACTION_BYTES)?;
    let request: DeferredUninstallRequest =
        serde_json::from_slice(&request_bytes).map_err(|_| LauncherError::InvalidState)?;
    if &request != expected_request {
        return Err(LauncherError::InvalidState);
    }
    request.validate(root, &request.token, request.launcher_pid)?;

    let manifest = read_install_manifest(root)?;
    for version in &request.versions {
        if !manifest.owns_payload(version, "rootlight") {
            return Err(LauncherError::InvalidState);
        }
        let version_root = root.join("versions").join(version);
        validate_directory_no_reparse(&version_root)?;
        verify_private_windows_dacl(&version_root)?;
        fs::remove_dir_all(&version_root).map_err(LauncherError::UninstallCleanup)?;
    }
    fs::remove_dir(root.join("versions")).map_err(LauncherError::UninstallCleanup)?;

    let current_bin = root.join("current/bin");
    for binary in EXPECTED_BINARIES {
        remove_owned_file(&current_bin.join(format!("{binary}.exe")))?;
    }
    fs::remove_dir(&current_bin).map_err(LauncherError::UninstallCleanup)?;
    fs::remove_dir(root.join("current")).map_err(LauncherError::UninstallCleanup)?;

    for name in [
        "update-policy.json",
        "active-version",
        "update-transaction.json",
        ".update-artifact.copy",
        ".bootstrap-artifact.copy",
    ] {
        remove_optional_owned_file(&state.join(name))?;
    }
    remove_owned_file(&state.join("install-manifest.json"))?;
    remove_owned_file(&request_path)?;
    drop(lock);
    remove_owned_file(&lock_path)?;
    fs::remove_dir(&state).map_err(LauncherError::UninstallCleanup)?;
    Ok(())
}

#[cfg(windows)]
fn remove_optional_owned_file(path: &Path) -> Result<(), LauncherError> {
    match fs::symlink_metadata(path) {
        Ok(_) => remove_owned_file(path),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(LauncherError::UninstallCleanup(source)),
    }
}

#[cfg(windows)]
fn remove_owned_file(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(LauncherError::UninstallCleanup)?;
    if !metadata.file_type().is_file() {
        return Err(LauncherError::InvalidState);
    }
    reject_reparse(&metadata)?;
    verify_private_windows_dacl(path)?;
    fs::remove_file(path).map_err(LauncherError::UninstallCleanup)
}

#[cfg(windows)]
fn apply_private_windows_dacl(path: &Path, inheritable: bool) -> Result<(), LauncherError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows_permissions::{
        LocalBox, SecurityDescriptor,
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetNamedSecurityInfo,
    };

    let token =
        OwnedToken::from_current_process(TOKEN_QUERY).map_err(|_| LauncherError::InsecureHelper)?;
    let sid = token
        .user()
        .and_then(|value| value.to_string())
        .map_err(|_| LauncherError::InsecureHelper)?;
    let inheritance = if inheritable { "OICI" } else { "" };
    let descriptor_text = format!("D:P(A;{inheritance};FA;;;{sid})");
    let descriptor: LocalBox<SecurityDescriptor> = descriptor_text
        .parse()
        .map_err(|_| LauncherError::InsecureHelper)?;
    let dacl = descriptor.dacl().ok_or(LauncherError::InsecureHelper)?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(LauncherError::UninstallPreparation)
}

#[cfg(windows)]
fn verify_private_windows_dacl(path: &Path) -> Result<(), LauncherError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows_permissions::{
        constants::{AccessRights, AceType, SeObjectType, SecurityInformation},
        wrappers::{ConvertSecurityDescriptorToStringSecurityDescriptor, GetNamedSecurityInfo},
    };

    let token =
        OwnedToken::from_current_process(TOKEN_QUERY).map_err(|_| LauncherError::InsecureHelper)?;
    let expected_sid = token
        .user()
        .and_then(|value| value.to_string())
        .map_err(|_| LauncherError::InsecureHelper)?;
    let descriptor = GetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
    )
    .map_err(LauncherError::UninstallPreparation)?;
    let sddl =
        ConvertSecurityDescriptorToStringSecurityDescriptor(&descriptor, SecurityInformation::Dacl)
            .map_err(LauncherError::UninstallPreparation)?;
    let dacl = descriptor.dacl().ok_or(LauncherError::InsecureHelper)?;
    let ace = dacl.get_ace(0).ok_or(LauncherError::InsecureHelper)?;
    if !sddl.to_string_lossy().starts_with("D:P")
        || dacl.len() != 1
        || ace.ace_type() != AceType::ACCESS_ALLOWED_ACE_TYPE
        || ace.mask() != AccessRights::FileAllAccess
        || ace.sid().ok_or(LauncherError::InsecureHelper)?.to_string() != expected_sid
    {
        return Err(LauncherError::InsecureHelper);
    }
    Ok(())
}

fn invocation_binary_name(invocation: &OsStr) -> Result<&'static str, LauncherError> {
    let file_name = Path::new(invocation)
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or(LauncherError::InvalidInvocation)?;
    #[cfg(windows)]
    let file_name = file_name
        .strip_suffix(".exe")
        .ok_or(LauncherError::InvalidInvocation)?;
    EXPECTED_BINARIES
        .iter()
        .copied()
        .find(|candidate| *candidate == file_name)
        .ok_or(LauncherError::InvalidInvocation)
}

fn install_root(executable: &Path) -> Result<PathBuf, LauncherError> {
    let bin = executable.parent().ok_or(LauncherError::InvalidLayout)?;
    let current = bin.parent().ok_or(LauncherError::InvalidLayout)?;
    let root = current.parent().ok_or(LauncherError::InvalidLayout)?;
    if bin.file_name() != Some(OsStr::new("bin"))
        || current.file_name() != Some(OsStr::new("current"))
        || !root.is_absolute()
    {
        return Err(LauncherError::InvalidLayout);
    }
    Ok(root.to_path_buf())
}

struct ResolvedPayload {
    executable: PathBuf,
    #[cfg(any(test, windows))]
    version: String,
}

fn resolve_payload(root: &Path, binary: &str) -> Result<ResolvedPayload, LauncherError> {
    validate_install_directory(root)?;
    let state = root.join("state");
    let manifest_bytes = read_regular_bounded(
        &state.join("install-manifest.json"),
        MAX_INSTALL_MANIFEST_BYTES,
    )?;
    let manifest: InstallManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| LauncherError::InvalidState)?;
    manifest.validate()?;

    let active = read_active_version(&state.join("active-version")).ok();
    let transaction_path = state.join("update-transaction.json");
    let transaction = load_transaction(&transaction_path);
    let mut candidates = Vec::with_capacity(4);
    match transaction {
        TransactionRead::Absent => {
            candidates.extend(active);
            candidates.push(manifest.active_version.clone());
            candidates.push(manifest.last_good_version().to_owned());
        }
        TransactionRead::Valid(transaction) if transaction.phase == UpdatePhase::Committed => {
            candidates.extend(active);
            candidates.push(transaction.candidate_version);
            candidates.push(transaction.previous_version);
            candidates.push(manifest.last_good_version().to_owned());
        }
        TransactionRead::Valid(transaction) => {
            candidates.push(transaction.previous_version);
            candidates.push(manifest.last_good_version().to_owned());
        }
        TransactionRead::Malformed => {
            candidates.push(manifest.last_good_version().to_owned());
        }
    }

    let mut observed = BTreeSet::new();
    for version in candidates {
        if !observed.insert(version.clone())
            || canonical_version(&version).is_err()
            || !manifest.owns_payload(&version, binary)
        {
            continue;
        }
        let candidate = payload_path(root, &version, binary);
        if validate_payload(&candidate).is_ok() {
            return Ok(ResolvedPayload {
                executable: candidate,
                #[cfg(any(test, windows))]
                version,
            });
        }
    }
    Err(LauncherError::NoTrustedPayload)
}

fn load_transaction(path: &Path) -> TransactionRead {
    let bytes = match read_optional_regular_bounded(path, MAX_UPDATE_TRANSACTION_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return TransactionRead::Absent,
        Err(_) => return TransactionRead::Malformed,
    };
    let transaction: UpdateTransaction = match serde_json::from_slice(&bytes) {
        Ok(transaction) => transaction,
        Err(_) => return TransactionRead::Malformed,
    };
    if transaction.validate().is_err() {
        TransactionRead::Malformed
    } else {
        TransactionRead::Valid(transaction)
    }
}

fn read_active_version(path: &Path) -> Result<String, LauncherError> {
    let bytes = read_regular_bounded(path, MAX_ACTIVE_VERSION_BYTES)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| LauncherError::InvalidState)?;
    let version = value
        .strip_suffix('\n')
        .ok_or(LauncherError::InvalidState)?;
    if version.contains(['\r', '\n']) {
        return Err(LauncherError::InvalidState);
    }
    canonical_version(version)?;
    Ok(version.to_owned())
}

fn canonical_version(value: &str) -> Result<Version, LauncherError> {
    let version = Version::parse(value).map_err(|_| LauncherError::InvalidState)?;
    if version.to_string() != value || value.len() > 128 {
        return Err(LauncherError::InvalidState);
    }
    Ok(version)
}

fn payload_path(root: &Path, version: &str, binary: &str) -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    root.join("versions")
        .join(version)
        .join("bin")
        .join(format!("{binary}{suffix}"))
}

fn validate_payload(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(LauncherError::ReadState)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        return Err(LauncherError::NoTrustedPayload);
    }
    reject_reparse(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        if metadata.nlink() != 1 || metadata.permissions().mode() & 0o111 == 0 {
            return Err(LauncherError::NoTrustedPayload);
        }
    }
    Ok(())
}

fn validate_install_directory(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(LauncherError::ReadState)?;
    if !metadata.is_dir() {
        return Err(LauncherError::InvalidLayout);
    }
    reject_reparse(&metadata)
}

#[cfg(windows)]
fn validate_directory_no_reparse(path: &Path) -> Result<(), LauncherError> {
    let metadata = fs::symlink_metadata(path).map_err(LauncherError::UninstallPreparation)?;
    if !metadata.is_dir() {
        return Err(LauncherError::InvalidHelperLocation);
    }
    reject_reparse(&metadata)
}

fn read_optional_regular_bounded(
    path: &Path,
    maximum: u64,
) -> Result<Option<Vec<u8>>, LauncherError> {
    match open_regular_no_follow(path) {
        Ok(file) => read_open_file_bounded(file, maximum).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(LauncherError::ReadState(source)),
    }
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, LauncherError> {
    let file = open_regular_no_follow(path).map_err(LauncherError::ReadState)?;
    read_open_file_bounded(file, maximum)
}

fn read_open_file_bounded(file: File, maximum: u64) -> Result<Vec<u8>, LauncherError> {
    let metadata = file.metadata().map_err(LauncherError::ReadState)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(LauncherError::InvalidState);
    }
    reject_reparse(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(LauncherError::InvalidState);
        }
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| LauncherError::InvalidState)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(maximum.checked_add(1).ok_or(LauncherError::InvalidState)?)
        .read_to_end(&mut bytes)
        .map_err(LauncherError::ReadState)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(LauncherError::InvalidState);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_regular_no_follow(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn open_regular_no_follow(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };

    OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_regular_no_follow(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "launcher platform is unsupported",
    ))
}

fn reject_reparse(metadata: &fs::Metadata) -> Result<(), LauncherError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(LauncherError::InvalidState);
        }
    }
    #[cfg(not(windows))]
    let _ = metadata;
    Ok(())
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !value.contains('\\')
        && !value.contains(':')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallManifest {
    schema: String,
    target: String,
    active_version: String,
    #[serde(default)]
    last_good_version: Option<String>,
    owned_paths: Vec<String>,
    platform_resources: Vec<PlatformResource>,
}

impl InstallManifest {
    fn validate(&self) -> Result<(), LauncherError> {
        if !matches!(
            self.schema.as_str(),
            INSTALL_MANIFEST_SCHEMA_V1 | INSTALL_MANIFEST_SCHEMA_V2
        ) || self.target.is_empty()
            || self.target.len() > 128
            || self.owned_paths.len() > MAX_OWNED_PATHS
            || self.platform_resources.len() > 16
        {
            return Err(LauncherError::InvalidState);
        }
        canonical_version(&self.active_version)?;
        canonical_version(self.last_good_version())?;
        let mut previous = None;
        for path in &self.owned_paths {
            if !valid_relative_path(path) || previous.is_some_and(|value| value >= path) {
                return Err(LauncherError::InvalidState);
            }
            previous = Some(path);
        }
        if self
            .platform_resources
            .iter()
            .any(|resource| !resource.is_valid())
        {
            return Err(LauncherError::InvalidState);
        }
        Ok(())
    }

    fn last_good_version(&self) -> &str {
        self.last_good_version
            .as_deref()
            .unwrap_or(&self.active_version)
    }

    fn owns_payload(&self, version: &str, binary: &str) -> bool {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let expected = format!("versions/{version}/bin/{binary}{suffix}");
        self.owned_paths
            .binary_search_by(|candidate| candidate.as_str().cmp(&expected))
            .is_ok()
    }

    #[cfg(windows)]
    fn installed_versions(&self) -> Result<Vec<String>, LauncherError> {
        let mut versions = BTreeSet::new();
        for path in &self.owned_paths {
            let mut components = Path::new(path).components();
            if components.next() != Some(Component::Normal(OsStr::new("versions"))) {
                continue;
            }
            let Some(Component::Normal(version)) = components.next() else {
                return Err(LauncherError::InvalidState);
            };
            let version = version.to_str().ok_or(LauncherError::InvalidState)?;
            canonical_version(version)?;
            versions.insert(version.to_owned());
        }
        if versions.is_empty() {
            return Err(LauncherError::InvalidState);
        }
        Ok(versions.into_iter().collect())
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredUninstallRequest {
    schema: String,
    token: String,
    target: String,
    launcher_pid: u32,
    payload_pid: u32,
    versions: Vec<String>,
}

#[cfg(windows)]
impl DeferredUninstallRequest {
    fn validate(&self, root: &Path, token: &str, launcher_pid: u32) -> Result<(), LauncherError> {
        if self.schema != DEFERRED_UNINSTALL_SCHEMA
            || self.token != token
            || !lower_hex(&self.token, 32)
            || self.launcher_pid != launcher_pid
            || self.launcher_pid == 0
            || self.payload_pid == 0
            || self.launcher_pid == self.payload_pid
            || self.versions.is_empty()
            || self.versions.len() > MAX_OWNED_PATHS
            || self.target != current_target()?
        {
            return Err(LauncherError::InvalidState);
        }
        let manifest = read_install_manifest(root)?;
        let active = read_active_version(&root.join("state/active-version"))?;
        if manifest.target != self.target
            || manifest.active_version != active
            || manifest.installed_versions()? != self.versions
        {
            return Err(LauncherError::InvalidState);
        }
        Ok(())
    }
}

#[cfg(windows)]
fn current_target() -> Result<&'static str, LauncherError> {
    #[cfg(target_arch = "x86_64")]
    return Ok("x86_64-pc-windows-msvc");
    #[allow(unreachable_code)]
    Err(LauncherError::UnsupportedPlatform)
}

#[cfg(windows)]
fn read_install_manifest(root: &Path) -> Result<InstallManifest, LauncherError> {
    let path = root.join("state/install-manifest.json");
    verify_private_windows_dacl(&path)?;
    let bytes = read_regular_bounded(&path, MAX_INSTALL_MANIFEST_BYTES)?;
    let manifest: InstallManifest =
        serde_json::from_slice(&bytes).map_err(|_| LauncherError::InvalidState)?;
    manifest.validate()?;
    Ok(manifest)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformResource {
    kind: String,
    id: String,
}

impl PlatformResource {
    fn is_valid(&self) -> bool {
        [&self.kind, &self.id].into_iter().all(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UpdatePhase {
    Staged,
    HealthChecking,
    CommitPrepared,
    Committed,
    RollbackPrepared,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateTransaction {
    schema: String,
    phase: UpdatePhase,
    previous_version: String,
    candidate_version: String,
    target: String,
    staging_name: String,
    metadata_sha256: String,
    artifact_sha256: String,
    candidate_owned_paths: Vec<String>,
}

impl UpdateTransaction {
    fn validate(&self) -> Result<(), LauncherError> {
        canonical_version(&self.previous_version)?;
        canonical_version(&self.candidate_version)?;
        if self.schema != UPDATE_TRANSACTION_SCHEMA
            || self.previous_version == self.candidate_version
            || self.target.is_empty()
            || self.target.len() > 128
            || self.staging_name.is_empty()
            || self.staging_name.len() > 255
            || !self.staging_name.starts_with(".update-")
            || !lower_hex(&self.metadata_sha256, 64)
            || !lower_hex(&self.artifact_sha256, 64)
            || self.candidate_owned_paths.len() > MAX_OWNED_PATHS
            || self
                .candidate_owned_paths
                .iter()
                .any(|path| !valid_relative_path(path))
        {
            return Err(LauncherError::InvalidState);
        }
        Ok(())
    }
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

enum TransactionRead {
    Absent,
    Valid(UpdateTransaction),
    Malformed,
}

/// Stable launcher failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LauncherError {
    /// The process executable could not be resolved.
    #[error("current executable is unavailable")]
    CurrentExecutable(#[source] io::Error),
    /// The invocation basename is not one supported public Rootlight command.
    #[error("launcher invocation is invalid")]
    InvalidInvocation,
    /// The executable is outside the fixed `current/bin` layout.
    #[error("launcher installation layout is invalid")]
    InvalidLayout,
    /// Installation state could not be read.
    #[error("launcher state could not be read")]
    ReadState(#[source] io::Error),
    /// Installation state is oversized, malformed, insecure, or inconsistent.
    #[error("launcher state is invalid")]
    InvalidState,
    /// Neither selected nor retained fallback payload is trusted and executable.
    #[error("no trusted Rootlight payload is available")]
    NoTrustedPayload,
    /// The selected payload could not replace or start from the launcher.
    #[error("Rootlight payload could not be started")]
    Spawn(#[source] io::Error),
    /// The Windows MCP launcher could not forward bounded standard streams.
    #[error("Rootlight MCP stream proxy failed")]
    McpProxy(#[source] io::Error),
    /// The conservative MCP bootstrap could not prepare its bounded response.
    #[error("Rootlight MCP initialization bootstrap failed")]
    McpBootstrap,
    /// The selected MCP payload disagreed with the shared bootstrap protocol.
    #[error("Rootlight MCP bootstrap invariant failed")]
    McpProxyInvariant,
    /// A bounded MCP stream worker terminated unexpectedly.
    #[error("Rootlight MCP stream proxy worker failed")]
    McpProxyThread,
    /// An internal MCP bootstrap or handoff missed its fixed deadline.
    #[error("Rootlight MCP bootstrap timed out")]
    McpProxyTimeout,
    /// A private helper copy or its fixed supervisor process could not be prepared.
    #[error("Windows uninstall helper could not be prepared")]
    UninstallPreparation(#[source] io::Error),
    /// Deferred owned-path removal failed.
    #[error("Windows uninstall cleanup failed")]
    UninstallCleanup(#[source] io::Error),
    /// The helper temporary tree or installation state is not private.
    #[error("Windows uninstall helper security policy failed")]
    InsecureHelper,
    /// The helper copy is outside its one-time private temporary directory.
    #[error("Windows uninstall helper location is invalid")]
    InvalidHelperLocation,
    /// Another update process still owns the installation lock.
    #[error("Windows uninstall cleanup is busy")]
    UninstallBusy,
    /// The trusted system clock cannot provide a one-time helper identity.
    #[error("Windows uninstall clock is invalid")]
    UninstallClock,
    /// The deferred request or target processes did not finish in time.
    #[error("Windows uninstall cleanup timed out")]
    UninstallTimedOut,
    /// The deferred helper exited before the installed payload scheduled cleanup.
    #[error("Windows uninstall helper exited before cleanup was scheduled")]
    UninstallHelperExited,
    /// The operating system has no supported launcher implementation.
    #[error("launcher platform is unsupported")]
    UnsupportedPlatform,
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn private_tempdir() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
                .expect("temporary directory becomes private");
        }
        temporary
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options.mode(0o600);
        }
        let mut file = options.open(path).expect("private fixture creates");
        file.write_all(bytes).expect("private fixture writes");
        file.sync_all().expect("private fixture synchronizes");
    }

    fn install_fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = private_tempdir();
        let root = temporary.path().join("install");
        fs::create_dir_all(root.join("state")).expect("state directory creates");
        fs::create_dir_all(root.join("versions/1.0.0/bin")).expect("old payload directory creates");
        fs::create_dir_all(root.join("versions/2.0.0/bin")).expect("new payload directory creates");
        #[cfg(unix)]
        for directory in [
            root.clone(),
            root.join("state"),
            root.join("versions"),
            root.join("versions/1.0.0"),
            root.join("versions/1.0.0/bin"),
            root.join("versions/2.0.0"),
            root.join("versions/2.0.0/bin"),
        ] {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .expect("fixture directory becomes private");
        }
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        for version in ["1.0.0", "2.0.0"] {
            let payload = root
                .join("versions")
                .join(version)
                .join("bin")
                .join(format!("rootlight{suffix}"));
            write_private(&payload, b"payload");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;

                fs::set_permissions(&payload, fs::Permissions::from_mode(0o700))
                    .expect("payload becomes executable");
            }
        }
        let manifest = serde_json::json!({
            "schema": INSTALL_MANIFEST_SCHEMA_V2,
            "target": current_target(),
            "active_version": "2.0.0",
            "last_good_version": "1.0.0",
            "owned_paths": [
                format!("versions/1.0.0/bin/rootlight{suffix}"),
                format!("versions/2.0.0/bin/rootlight{suffix}")
            ],
            "platform_resources": []
        });
        write_private(
            &root.join("state/install-manifest.json"),
            &serde_json::to_vec(&manifest).expect("manifest serializes"),
        );
        write_private(&root.join("state/active-version"), b"2.0.0\n");
        (temporary, root)
    }

    fn current_target() -> &'static str {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        return "x86_64-pc-windows-msvc";
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        return "x86_64-unknown-linux-gnu";
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        return "aarch64-unknown-linux-gnu";
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return "x86_64-apple-darwin";
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return "aarch64-apple-darwin";
        #[allow(unreachable_code)]
        "unsupported"
    }

    #[cfg(windows)]
    fn deferred_uninstall_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        PathBuf,
        DeferredUninstallRequest,
    ) {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        for directory in [
            root.clone(),
            root.join("state"),
            root.join("versions"),
            root.join("versions/1.0.0"),
            root.join("versions/1.0.0/bin"),
            root.join("versions/2.0.0"),
            root.join("versions/2.0.0/bin"),
            root.join("current"),
            root.join("current/bin"),
            root.join("user"),
        ] {
            fs::create_dir(&directory).expect("owned directory creates");
            apply_private_windows_dacl(&directory, true).expect("owned directory becomes private");
            verify_private_windows_dacl(&directory).expect("owned directory stays private");
        }
        let mut owned_paths = Vec::new();
        for version in ["1.0.0", "2.0.0"] {
            for binary in EXPECTED_BINARIES {
                let relative = format!("versions/{version}/bin/{binary}.exe");
                let path = root.join(&relative);
                write_private(&path, b"payload");
                apply_private_windows_dacl(&path, false).expect("payload becomes private");
                owned_paths.push(relative);
            }
        }
        for binary in EXPECTED_BINARIES {
            let path = root.join("current/bin").join(format!("{binary}.exe"));
            write_private(&path, b"launcher");
            apply_private_windows_dacl(&path, false).expect("launcher becomes private");
        }
        owned_paths.sort();
        let manifest = serde_json::json!({
            "schema": INSTALL_MANIFEST_SCHEMA_V2,
            "target": current_target(),
            "active_version": "2.0.0",
            "last_good_version": "1.0.0",
            "owned_paths": owned_paths,
            "platform_resources": []
        });
        for (name, bytes) in [
            (
                "install-manifest.json",
                serde_json::to_vec(&manifest).expect("manifest serializes"),
            ),
            ("active-version", b"2.0.0\n".to_vec()),
            ("update.lock", Vec::new()),
        ] {
            let path = root.join("state").join(name);
            write_private(&path, &bytes);
            apply_private_windows_dacl(&path, false).expect("state file becomes private");
        }
        write_private(&root.join("user/sentinel"), b"user");
        let unrelated = temporary.path().join("unrelated");
        write_private(&unrelated, b"unrelated");
        let request = DeferredUninstallRequest {
            schema: DEFERRED_UNINSTALL_SCHEMA.to_owned(),
            token: "00000000000000000000000000000000".to_owned(),
            target: current_target().to_owned(),
            launcher_pid: 100,
            payload_pid: 101,
            versions: vec!["1.0.0".to_owned(), "2.0.0".to_owned()],
        };
        write_deferred_request(&root, &request);
        (temporary, root, unrelated, request)
    }

    #[cfg(windows)]
    fn write_deferred_request(root: &Path, request: &DeferredUninstallRequest) {
        let path = root.join("state").join(DEFERRED_UNINSTALL_FILE);
        if path.exists() {
            fs::remove_file(&path).expect("old request removes");
        }
        write_private(
            &path,
            &serde_json::to_vec(request).expect("request serializes"),
        );
        apply_private_windows_dacl(&path, false).expect("request becomes private");
    }

    #[test]
    fn incomplete_transaction_routes_the_retained_payload() {
        let (_temporary, root) = install_fixture();
        let transaction = serde_json::json!({
            "schema": UPDATE_TRANSACTION_SCHEMA,
            "phase": "commit_prepared",
            "previous_version": "1.0.0",
            "candidate_version": "2.0.0",
            "target": current_target(),
            "staging_name": ".update-2.0.0",
            "metadata_sha256": "a".repeat(64),
            "artifact_sha256": "b".repeat(64),
            "candidate_owned_paths": []
        });
        write_private(
            &root.join("state/update-transaction.json"),
            &serde_json::to_vec(&transaction).expect("transaction serializes"),
        );

        let target = resolve_payload(&root, "rootlight").expect("fallback resolves");

        assert!(target.executable.starts_with(root.join("versions/1.0.0")));
        assert_eq!(target.version, "1.0.0");
    }

    #[test]
    fn malformed_transaction_never_routes_the_active_candidate() {
        let (_temporary, root) = install_fixture();
        write_private(&root.join("state/update-transaction.json"), b"{");

        let target = resolve_payload(&root, "rootlight").expect("fallback resolves");

        assert!(target.executable.starts_with(root.join("versions/1.0.0")));
        assert_eq!(target.version, "1.0.0");
    }

    #[cfg(windows)]
    #[test]
    fn deferred_uninstall_removes_only_owned_installation_trees() {
        let (_temporary, root, unrelated, request) = deferred_uninstall_fixture();

        cleanup_deferred_installation(&root, &request).expect("deferred uninstall completes");

        assert!(!root.join("state").exists());
        assert!(!root.join("versions").exists());
        assert!(!root.join("current").exists());
        assert_eq!(
            fs::read(root.join("user/sentinel")).expect("user data remains"),
            b"user"
        );
        assert_eq!(
            fs::read(unrelated).expect("unrelated data remains"),
            b"unrelated"
        );
    }

    #[cfg(windows)]
    #[test]
    fn forged_uninstall_token_cannot_remove_owned_or_unrelated_files() {
        let (_temporary, root, unrelated, request) = deferred_uninstall_fixture();
        let mut forged = request.clone();
        forged.token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        write_deferred_request(&root, &forged);

        assert!(matches!(
            cleanup_deferred_installation(&root, &request),
            Err(LauncherError::InvalidState)
        ));
        assert!(root.join("current/bin/rootlight.exe").is_file());
        assert_eq!(
            fs::read(unrelated).expect("unrelated data remains"),
            b"unrelated"
        );
    }

    #[cfg(windows)]
    #[test]
    fn forged_uninstall_version_path_cannot_escape_install_root() {
        let (_temporary, root, unrelated, mut request) = deferred_uninstall_fixture();
        request.versions = vec!["../unrelated".to_owned()];
        write_deferred_request(&root, &request);

        assert!(matches!(
            cleanup_deferred_installation(&root, &request),
            Err(LauncherError::InvalidState)
        ));
        assert!(root.join("current/bin/rootlight.exe").is_file());
        assert_eq!(
            fs::read(unrelated).expect("unrelated data remains"),
            b"unrelated"
        );
    }
}
