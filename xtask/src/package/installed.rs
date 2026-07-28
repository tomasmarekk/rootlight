//! Installed-package health and MCP release gates.

use std::{
    io::{BufRead as _, BufReader, Read, Write as _},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_client::{Client, ClientError, ConnectPolicy};
use rootlight_runtime::RuntimePaths;
#[cfg(windows)]
use rootlight_sandbox::KillOnCloseJob;
use rootlight_sandbox::{ChildProcess, ChildStdin, ProcessCommand, ProcessError, StdioMode};
use serde::Serialize;
use serde_json::Value;

use super::PackageError;

#[cfg(windows)]
const WINDOWS_COLD_HEALTH_ATTEMPTS: usize = 10;
#[cfg(windows)]
const WINDOWS_COLD_HEALTH_LIMIT_MICROS: u64 = 3_000_000;
const MCP_WARMUP_SAMPLES: usize = 5;
const MCP_MEASURED_SAMPLES: usize = 100;
const MCP_P50_LIMIT_MICROS: u64 = 80_000;
const MCP_P95_LIMIT_MICROS: u64 = 150_000;
const MCP_P99_LIMIT_MICROS: u64 = 300_000;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(2);
const PROCESS_COMPLETION_TIMEOUT: Duration = Duration::from_secs(3);
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(windows)]
const PROCESS_TERMINATION_EXIT_CODE: u32 = 1;
const CLIENT_INSTANCE_ID: [u8; 16] = *b"rootlight-xtask1";
const INITIALIZE_REQUEST: &[u8] = br#"{"jsonrpc":"2.0","id":"initialize","method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"rootlight-startup-regression","version":"1.0"}}}
"#;

#[derive(Debug, Serialize)]
pub(super) struct InstalledReleaseEvidence {
    windows_first_health: Option<WindowsFirstHealthEvidence>,
    mcp_initialize: McpInitializeEvidence,
}

#[derive(Debug, Serialize)]
struct WindowsFirstHealthEvidence {
    limit_micros: u64,
    samples_micros: Vec<u64>,
    successful_attempts: usize,
    launcher_exit_count: usize,
    stdout_eof_count: usize,
    stderr_eof_count: usize,
    pre_cleanup_active_processes: Vec<u32>,
    post_cleanup_active_processes: u32,
}

#[derive(Debug, Serialize)]
struct McpInitializeEvidence {
    warmup_samples_micros: Vec<u64>,
    measured_samples_micros: Vec<u64>,
    p50_micros: u64,
    p95_micros: u64,
    p99_micros: u64,
    p50_limit_micros: u64,
    p95_limit_micros: u64,
    p99_limit_micros: u64,
    successful_warmups: usize,
    successful_measurements: usize,
    launcher_exit_count: usize,
    daemon_exit_count: usize,
    steady_state_active_processes: Option<u32>,
    post_cleanup_active_processes: Option<u32>,
}

pub(super) fn exercise(
    install_root: &Path,
    candidate_version: &str,
) -> Result<InstalledReleaseEvidence, PackageError> {
    Ok(InstalledReleaseEvidence {
        windows_first_health: exercise_windows_first_health(install_root)?,
        mcp_initialize: exercise_mcp_initialize(install_root, candidate_version)?,
    })
}

#[cfg(windows)]
fn exercise_windows_first_health(
    install_root: &Path,
) -> Result<Option<WindowsFirstHealthEvidence>, PackageError> {
    let launcher = installed_binary(install_root, "rootlight");
    let sandbox = private_tempdir("rootlight-installed-health-")?;
    let mut samples = Vec::with_capacity(WINDOWS_COLD_HEALTH_ATTEMPTS);
    let mut active_processes = Vec::with_capacity(WINDOWS_COLD_HEALTH_ATTEMPTS);

    for attempt in 0..WINDOWS_COLD_HEALTH_ATTEMPTS {
        let attempt_root = sandbox.path().join(format!("attempt-{attempt}"));
        let state = attempt_root.join("state");
        let runtime = attempt_root.join("runtime");
        if state.exists() || runtime.exists() {
            return invalid("cold health roots were used before the installed launcher ran");
        }
        let mut tree = ProcessTree::new()?;
        let started = Instant::now();
        let command = ProcessCommand::new(&launcher)
            .arg("health")
            .env("ROOTLIGHT_STATE_DIR", &state)
            .env("ROOTLIGHT_RUNTIME_DIR", &runtime)
            .stdin(StdioMode::Null)
            .stdout(StdioMode::Piped)
            .stderr(StdioMode::Piped);
        let mut child = ExactChild::new(tree.spawn(command)?);
        let stdout = child
            .take_stdout()
            .ok_or_else(|| invalid_error("installed health stdout was not retained"))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| invalid_error("installed health stderr was not retained"))?;
        let stdout_reader = bounded_reader(stdout, "installed-health-stdout")?;
        let stderr_reader = bounded_reader(stderr, "installed-health-stderr")?;
        let deadline = started
            .checked_add(Duration::from_micros(WINDOWS_COLD_HEALTH_LIMIT_MICROS))
            .ok_or(PackageError::Clock)?;
        let status = wait_for_exit_and_eof(
            &mut child,
            &stdout_reader,
            &stderr_reader,
            deadline,
            "installed Windows health",
        )?;
        let elapsed = duration_micros(started.elapsed())?;
        child.mark_reaped();
        let stdout = join_reader(stdout_reader, "installed Windows health stdout")?;
        let stderr = join_reader(stderr_reader, "installed Windows health stderr")?;
        if !status.success() {
            return invalid("installed Windows health returned a non-success status");
        }
        if !stderr.is_empty() {
            return invalid("installed Windows health wrote to stderr");
        }
        validate_health_response(&stdout)?;
        if elapsed > WINDOWS_COLD_HEALTH_LIMIT_MICROS {
            return invalid("installed Windows health exceeded its release latency limit");
        }
        let active_before_cleanup =
            wait_for_bounded_active_processes(&tree, 1, 2, "installed Windows health")?;
        let post_cleanup = tree.terminate_and_wait()?;
        if post_cleanup != Some(0) {
            return invalid("installed Windows health retained an owned process after cleanup");
        }
        samples.push(elapsed);
        active_processes.push(active_before_cleanup);
    }

    Ok(Some(WindowsFirstHealthEvidence {
        limit_micros: WINDOWS_COLD_HEALTH_LIMIT_MICROS,
        samples_micros: samples,
        successful_attempts: WINDOWS_COLD_HEALTH_ATTEMPTS,
        launcher_exit_count: WINDOWS_COLD_HEALTH_ATTEMPTS,
        stdout_eof_count: WINDOWS_COLD_HEALTH_ATTEMPTS,
        stderr_eof_count: WINDOWS_COLD_HEALTH_ATTEMPTS,
        pre_cleanup_active_processes: active_processes,
        post_cleanup_active_processes: 0,
    }))
}

#[cfg(not(windows))]
fn exercise_windows_first_health(
    _install_root: &Path,
) -> Result<Option<WindowsFirstHealthEvidence>, PackageError> {
    Ok(None)
}

fn exercise_mcp_initialize(
    install_root: &Path,
    candidate_version: &str,
) -> Result<McpInitializeEvidence, PackageError> {
    let mcp_launcher = installed_binary(install_root, "rootlight-mcp");
    let daemon = candidate_binary(install_root, candidate_version, "rootlight-daemon");
    let sandbox = private_tempdir("rootlight-installed-mcp-")?;
    let state = sandbox.path().join("state");
    let runtime = sandbox.path().join("runtime");
    if state.exists() || runtime.exists() {
        return invalid("installed MCP roots were used before the candidate daemon ran");
    }
    let paths = RuntimePaths::new(state.clone(), runtime.clone()).map_err(|source| {
        PackageError::InstalledRuntime {
            operation: "construct installed MCP runtime paths",
            source,
        }
    })?;
    let mut tree = ProcessTree::new()?;
    let daemon_command = ProcessCommand::new(daemon)
        .arg("--coordinated-stdio")
        .env("ROOTLIGHT_STATE_DIR", &state)
        .env("ROOTLIGHT_RUNTIME_DIR", &runtime)
        .stdin(StdioMode::Piped)
        .stdout(StdioMode::Null)
        .stderr(StdioMode::Null);
    let mut daemon = ExactChild::new(tree.spawn(daemon_command)?);
    let mut daemon_stdin = daemon
        .take_stdin()
        .ok_or_else(|| invalid_error("candidate daemon stdin was not retained"))?;
    wait_for_daemon_ready(&paths, &mut daemon)?;
    // Windows Job accounting includes the console host paired with this
    // console-subsystem daemon. Both processes remain in the same owned tree.
    let daemon_processes = if cfg!(windows) {
        Some(wait_for_bounded_active_processes(
            &tree,
            1,
            2,
            "installed MCP candidate daemon",
        )?)
    } else {
        None
    };

    let mut warmups = Vec::with_capacity(MCP_WARMUP_SAMPLES);
    for _ in 0..MCP_WARMUP_SAMPLES {
        warmups.push(measure_initialize(
            &tree,
            &mcp_launcher,
            &state,
            &runtime,
            daemon_processes,
        )?);
    }
    let mut measurements = Vec::with_capacity(MCP_MEASURED_SAMPLES);
    for _ in 0..MCP_MEASURED_SAMPLES {
        measurements.push(measure_initialize(
            &tree,
            &mcp_launcher,
            &state,
            &runtime,
            daemon_processes,
        )?);
    }
    let p50 = nearest_rank(&measurements, 50)?;
    let p95 = nearest_rank(&measurements, 95)?;
    let p99 = nearest_rank(&measurements, 99)?;
    if p50 > MCP_P50_LIMIT_MICROS || p95 > MCP_P95_LIMIT_MICROS || p99 > MCP_P99_LIMIT_MICROS {
        return invalid("installed stable MCP launcher exceeded its release latency limits");
    }

    daemon_stdin
        .write_all(b"shutdown\n")
        .and_then(|()| daemon_stdin.flush())
        .map_err(|source| PackageError::InstalledIo {
            operation: "request candidate daemon shutdown",
            source,
        })?;
    drop(daemon_stdin);
    let shutdown_deadline = Instant::now()
        .checked_add(PROCESS_CLEANUP_TIMEOUT)
        .ok_or(PackageError::Clock)?;
    let daemon_status = wait_for_exit(
        &mut daemon,
        shutdown_deadline,
        "installed MCP candidate daemon",
    )?;
    daemon.mark_reaped();
    if !daemon_status.success() {
        return invalid("installed MCP candidate daemon shutdown was not successful");
    }
    let post_cleanup = tree.wait_empty()?;
    if post_cleanup.is_some_and(|count| count != 0) {
        return invalid("installed MCP gate retained an owned process after shutdown");
    }

    Ok(McpInitializeEvidence {
        warmup_samples_micros: warmups,
        measured_samples_micros: measurements,
        p50_micros: p50,
        p95_micros: p95,
        p99_micros: p99,
        p50_limit_micros: MCP_P50_LIMIT_MICROS,
        p95_limit_micros: MCP_P95_LIMIT_MICROS,
        p99_limit_micros: MCP_P99_LIMIT_MICROS,
        successful_warmups: MCP_WARMUP_SAMPLES,
        successful_measurements: MCP_MEASURED_SAMPLES,
        launcher_exit_count: MCP_WARMUP_SAMPLES + MCP_MEASURED_SAMPLES,
        daemon_exit_count: 1,
        steady_state_active_processes: daemon_processes,
        post_cleanup_active_processes: post_cleanup,
    })
}

fn wait_for_daemon_ready(
    paths: &RuntimePaths,
    daemon: &mut ExactChild,
) -> Result<(), PackageError> {
    let deadline = Instant::now()
        .checked_add(PROCESS_COMPLETION_TIMEOUT)
        .ok_or(PackageError::Clock)?;
    loop {
        if daemon.try_wait()?.is_some() {
            daemon.mark_reaped();
            return invalid("installed MCP candidate daemon exited before readiness");
        }
        match Client::connect_or_start(paths, CLIENT_INSTANCE_ID, ConnectPolicy::ExistingOnly) {
            Ok(client) => {
                drop(client);
                return Ok(());
            }
            Err(source) if daemon_is_starting(&source) => {}
            Err(source) => {
                return Err(PackageError::InstalledClient {
                    operation: "probe installed MCP candidate daemon",
                    source,
                });
            }
        }
        if Instant::now() >= deadline {
            return invalid("installed MCP candidate daemon did not become ready");
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn daemon_is_starting(error: &ClientError) -> bool {
    match error {
        ClientError::DaemonUnavailable => true,
        ClientError::Runtime(
            rootlight_runtime::RuntimeError::OwnerSetupIncomplete
            | rootlight_runtime::RuntimeError::WindowsSecurityPolicy,
        ) => true,
        ClientError::Runtime(rootlight_runtime::RuntimeError::Io(source)) => {
            source.kind() == std::io::ErrorKind::NotFound
        }
        _ => false,
    }
}

fn measure_initialize(
    tree: &ProcessTree,
    launcher: &Path,
    state: &Path,
    runtime: &Path,
    expected_active_processes: Option<u32>,
) -> Result<u64, PackageError> {
    let started = Instant::now();
    let command = ProcessCommand::new(launcher)
        .env("ROOTLIGHT_STATE_DIR", state)
        .env("ROOTLIGHT_RUNTIME_DIR", runtime)
        .stdin(StdioMode::Piped)
        .stdout(StdioMode::Piped)
        .stderr(StdioMode::Piped);
    let mut child = ExactChild::new(tree.spawn(command)?);
    let mut stdin = child
        .take_stdin()
        .ok_or_else(|| invalid_error("installed MCP stdin was not retained"))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| invalid_error("installed MCP stdout was not retained"))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| invalid_error("installed MCP stderr was not retained"))?;
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let stdout_reader = timed_response_reader(stdout, started, response_sender)?;
    let stderr_reader = bounded_reader(stderr, "installed-mcp-stderr")?;

    stdin
        .write_all(INITIALIZE_REQUEST)
        .and_then(|()| stdin.flush())
        .map_err(|source| PackageError::InstalledIo {
            operation: "write installed MCP initialize request",
            source,
        })?;
    let (elapsed, response) = response_receiver
        .recv_timeout(PROCESS_COMPLETION_TIMEOUT)
        .map_err(|_| {
            invalid_error("installed MCP initialize response did not meet its process deadline")
        })?
        .map_err(invalid_error)?;
    validate_initialize_response(&response)?;
    drop(stdin);

    let exit_deadline = Instant::now()
        .checked_add(PROCESS_CLEANUP_TIMEOUT)
        .ok_or(PackageError::Clock)?;
    let status = wait_for_exit_and_eof(
        &mut child,
        &stdout_reader,
        &stderr_reader,
        exit_deadline,
        "installed MCP launcher",
    )?;
    child.mark_reaped();
    let trailing_stdout = join_reader(stdout_reader, "installed MCP stdout")?;
    let stderr = join_reader(stderr_reader, "installed MCP stderr")?;
    if !status.success() {
        return invalid("installed MCP launcher returned a non-success status");
    }
    if !trailing_stdout.is_empty() {
        return invalid("installed MCP launcher emitted more than one response");
    }
    if !stderr.is_empty() {
        return invalid("installed MCP launcher wrote to stderr");
    }
    wait_for_active_processes(
        tree,
        expected_active_processes,
        "installed MCP launcher process settlement",
    )?;
    Ok(elapsed)
}

fn wait_for_active_processes(
    tree: &ProcessTree,
    expected: Option<u32>,
    operation: &'static str,
) -> Result<(), PackageError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let deadline = Instant::now()
        .checked_add(PROCESS_CLEANUP_TIMEOUT)
        .ok_or(PackageError::Clock)?;
    loop {
        let observed = tree.active_processes()?;
        if observed == Some(expected) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return invalid(format!(
                "{operation} expected {expected} active processes, observed {observed:?}"
            ));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn wait_for_bounded_active_processes(
    tree: &ProcessTree,
    minimum: u32,
    maximum: u32,
    operation: &'static str,
) -> Result<u32, PackageError> {
    // A Windows console executable can retain one conhost process inside the
    // same Job Object; the release boundary therefore proves a bounded pair
    // before proving that cleanup reaches zero.
    let deadline = Instant::now()
        .checked_add(PROCESS_CLEANUP_TIMEOUT)
        .ok_or(PackageError::Clock)?;
    loop {
        let observed = tree.active_processes()?.ok_or_else(|| {
            invalid_error(format!("{operation} process accounting is unavailable"))
        })?;
        if (minimum..=maximum).contains(&observed) {
            return Ok(observed);
        }
        if Instant::now() >= deadline {
            return invalid(format!(
                "{operation} expected {minimum}..={maximum} active processes, observed {observed}"
            ));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

#[cfg(any(windows, test))]
fn validate_health_response(bytes: &[u8]) -> Result<(), PackageError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid_error("health output is not JSON"))?;
    if value.get("ok") != Some(&Value::Bool(true))
        || value.get("exit_family").and_then(Value::as_str) != Some("success")
        || value.pointer("/result/type").and_then(Value::as_str) != Some("health")
        || value.pointer("/result/data/ready").and_then(Value::as_bool) != Some(true)
    {
        return invalid("installed Windows health response is not healthy");
    }
    Ok(())
}

fn validate_initialize_response(bytes: &[u8]) -> Result<(), PackageError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| invalid_error("installed MCP initialize response is not JSON"))?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_str) != Some("initialize")
        || value.get("error").is_some()
        || value
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            != Some("2025-11-25")
        || value
            .pointer("/result/serverInfo/name")
            .and_then(Value::as_str)
            != Some("rootlight")
        || value
            .pointer("/result/capabilities/tools/listChanged")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return invalid("installed MCP initialize response violates the release contract");
    }
    Ok(())
}

fn nearest_rank(samples: &[u64], percentile: usize) -> Result<u64, PackageError> {
    if samples.is_empty() || !(1..=100).contains(&percentile) {
        return invalid("installed MCP percentile input is invalid");
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = ordered
        .len()
        .checked_mul(percentile)
        .and_then(|value| value.checked_add(99))
        .map(|value| value / 100)
        .ok_or(PackageError::Clock)?;
    ordered
        .get(rank.saturating_sub(1))
        .copied()
        .ok_or_else(|| invalid_error("installed MCP percentile rank is unavailable"))
}

fn wait_for_exit_and_eof(
    child: &mut ExactChild,
    stdout: &JoinHandle<Result<Vec<u8>, std::io::Error>>,
    stderr: &JoinHandle<Result<Vec<u8>, std::io::Error>>,
    deadline: Instant,
    operation: &'static str,
) -> Result<ExitStatus, PackageError> {
    let mut status = None;
    loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        if let Some(status) = status
            && stdout.is_finished()
            && stderr.is_finished()
        {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return invalid(format!(
                "{operation} did not exit and close stdout/stderr before its deadline"
            ));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn wait_for_exit(
    child: &mut ExactChild,
    deadline: Instant,
    operation: &'static str,
) -> Result<ExitStatus, PackageError> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return invalid(format!("{operation} did not exit before its deadline"));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    }
}

fn bounded_reader(
    reader: impl Read + Send + 'static,
    name: &'static str,
) -> Result<JoinHandle<Result<Vec<u8>, std::io::Error>>, PackageError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || read_bounded(reader))
        .map_err(|source| PackageError::InstalledIo {
            operation: "spawn installed process output reader",
            source,
        })
}

fn timed_response_reader(
    stdout: impl Read + Send + 'static,
    started: Instant,
    sender: mpsc::SyncSender<Result<(u64, Vec<u8>), String>>,
) -> Result<JoinHandle<Result<Vec<u8>, std::io::Error>>, PackageError> {
    thread::Builder::new()
        .name("installed-mcp-stdout".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut response = Vec::new();
            match reader.read_until(b'\n', &mut response) {
                Ok(0) => {
                    let source = std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "installed MCP stdout reached EOF before initialize response",
                    );
                    let _ = sender.send(Err(source.to_string()));
                    Err(source)
                }
                Ok(_) if response.len() > MAX_PROCESS_OUTPUT_BYTES => {
                    let source = std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "installed MCP initialize response exceeds its byte limit",
                    );
                    let _ = sender.send(Err(source.to_string()));
                    Err(source)
                }
                Ok(_) => {
                    let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                    let _ = sender.send(Ok((elapsed, response)));
                    read_bounded(reader)
                }
                Err(source) => {
                    let _ = sender.send(Err(source.to_string()));
                    Err(source)
                }
            }
        })
        .map_err(|source| PackageError::InstalledIo {
            operation: "spawn installed MCP response reader",
            source,
        })
}

fn read_bounded(mut reader: impl Read) -> Result<Vec<u8>, std::io::Error> {
    let limit = u64::try_from(MAX_PROCESS_OUTPUT_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    reader.by_ref().take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROCESS_OUTPUT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "installed process output exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

fn join_reader(
    reader: JoinHandle<Result<Vec<u8>, std::io::Error>>,
    operation: &'static str,
) -> Result<Vec<u8>, PackageError> {
    reader
        .join()
        .map_err(|_| invalid_error(format!("{operation} reader panicked")))?
        .map_err(|source| PackageError::InstalledIo { operation, source })
}

fn installed_binary(install_root: &Path, name: &str) -> PathBuf {
    install_root
        .join("current/bin")
        .join(format!("{name}{}", executable_suffix()))
}

fn candidate_binary(install_root: &Path, version: &str, name: &str) -> PathBuf {
    install_root
        .join("versions")
        .join(version)
        .join("bin")
        .join(format!("{name}{}", executable_suffix()))
}

const fn executable_suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

fn private_tempdir(prefix: &str) -> Result<tempfile::TempDir, PackageError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(target_os = "macos")]
    {
        builder
            .tempdir_in("/private/tmp")
            .map_err(PackageError::WorkingDir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder.tempdir().map_err(PackageError::WorkingDir)
    }
}

#[cfg(windows)]
fn duration_micros(duration: Duration) -> Result<u64, PackageError> {
    u64::try_from(duration.as_micros()).map_err(|_| PackageError::Clock)
}

fn invalid<T>(detail: impl Into<String>) -> Result<T, PackageError> {
    Err(invalid_error(detail))
}

fn invalid_error(detail: impl Into<String>) -> PackageError {
    PackageError::InvalidInstall(detail.into())
}

struct ExactChild {
    child: ChildProcess,
    reaped: bool,
}

impl ExactChild {
    const fn new(child: ChildProcess) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.take_stdin()
    }

    fn take_stdout(&mut self) -> Option<rootlight_sandbox::ChildStdout> {
        self.child.take_stdout()
    }

    fn take_stderr(&mut self) -> Option<rootlight_sandbox::ChildStderr> {
        self.child.take_stderr()
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, PackageError> {
        self.child
            .try_wait()
            .map_err(|source| process_error("query installed child status", source))
    }

    const fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for ExactChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let deadline = Instant::now()
            .checked_add(PROCESS_CLEANUP_TIMEOUT)
            .unwrap_or_else(Instant::now);
        // Error paths cannot return a second failure, so keep exact process
        // authority long enough to terminate and reap the child boundedly.
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            let _ = self.child.terminate();
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            thread::sleep(PROCESS_POLL_INTERVAL);
        }
    }
}

struct ProcessTree {
    #[cfg(windows)]
    job: KillOnCloseJob,
    #[cfg(windows)]
    disarmed: bool,
}

impl ProcessTree {
    fn new() -> Result<Self, PackageError> {
        #[cfg(windows)]
        {
            let job = KillOnCloseJob::new()
                .map_err(|source| process_error("create installed process Job Object", source))?;
            Ok(Self {
                job,
                disarmed: false,
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    fn spawn(&self, command: ProcessCommand) -> Result<ChildProcess, PackageError> {
        #[cfg(windows)]
        let result = self.job.spawn(command);
        #[cfg(not(windows))]
        let result = command.spawn();
        result.map_err(|source| process_error("spawn installed package process", source))
    }

    fn active_processes(&self) -> Result<Option<u32>, PackageError> {
        #[cfg(windows)]
        {
            self.job
                .active_processes()
                .map(Some)
                .map_err(|source| process_error("query installed process accounting", source))
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }

    #[cfg(windows)]
    fn terminate_and_wait(&mut self) -> Result<Option<u32>, PackageError> {
        self.job
            .terminate(PROCESS_TERMINATION_EXIT_CODE)
            .map_err(|source| process_error("terminate installed process tree", source))?;
        self.wait_empty()
    }

    fn wait_empty(&mut self) -> Result<Option<u32>, PackageError> {
        #[cfg(windows)]
        {
            let deadline = Instant::now()
                .checked_add(PROCESS_CLEANUP_TIMEOUT)
                .ok_or(PackageError::Clock)?;
            self.job.wait_empty(deadline).map_err(|source| {
                process_error("wait for installed process tree cleanup", source)
            })?;
            let active = self
                .job
                .active_processes()
                .map_err(|source| process_error("verify installed process tree cleanup", source))?;
            self.disarmed = active == 0;
            Ok(Some(active))
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = self.job.terminate(PROCESS_TERMINATION_EXIT_CODE);
            let _ = self.job.wait_empty(
                Instant::now()
                    .checked_add(PROCESS_CLEANUP_TIMEOUT)
                    .unwrap_or_else(Instant::now),
            );
        }
    }
}

fn process_error(operation: &'static str, source: ProcessError) -> PackageError {
    PackageError::InstalledProcess { operation, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_uses_the_release_contract_definition() {
        let samples = (1..=100).collect::<Vec<_>>();

        assert_eq!(nearest_rank(&samples, 50).expect("p50 exists"), 50);
        assert_eq!(nearest_rank(&samples, 95).expect("p95 exists"), 95);
        assert_eq!(nearest_rank(&samples, 99).expect("p99 exists"), 99);
    }

    #[test]
    fn initialize_validation_requires_protocol_identity_and_tools_contract() {
        let valid = br#"{"jsonrpc":"2.0","id":"initialize","result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"rootlight","version":"0.1.0"}}}"#;
        validate_initialize_response(valid).expect("valid initialize response passes");

        let invalid = br#"{"jsonrpc":"2.0","id":"initialize","result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"rootlight"}}}"#;
        assert!(validate_initialize_response(invalid).is_err());
    }

    #[test]
    fn health_validation_requires_a_ready_success_envelope() {
        let valid = br#"{"contract_version":"1.0","ok":true,"exit_family":"success","result":{"type":"health","data":{"ready":true}}}"#;
        validate_health_response(valid).expect("ready health response passes");

        let invalid = br#"{"contract_version":"1.0","ok":false,"exit_family":"internal","result":{"type":"health","data":{"ready":false}}}"#;
        assert!(validate_health_response(invalid).is_err());
    }

    #[test]
    fn direct_daemon_readiness_retries_only_startup_absence() {
        assert!(daemon_is_starting(&ClientError::DaemonUnavailable));
        assert!(daemon_is_starting(&ClientError::Runtime(
            rootlight_runtime::RuntimeError::OwnerSetupIncomplete,
        )));
        assert!(!daemon_is_starting(&ClientError::Runtime(
            rootlight_runtime::RuntimeError::InvalidDiscovery,
        )));
    }
}
