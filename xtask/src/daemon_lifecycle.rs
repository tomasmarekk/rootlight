//! Portable real-process evidence for daemon startup, control, and cleanup.
//!
//! The harness uses validated discovery and negotiated health as readiness events,
//! then asks a supervised daemon to shut down through stdin rather than sleeping.

use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rootlight_client::{Client, ConnectPolicy};
use rootlight_ids::OperationId;
use rootlight_ipc::connect;
use rootlight_runtime::{RuntimeError, RuntimePaths};
use serde_json::Value;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_COUNT: usize = 100;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) fn check(bin_dir: &Path) -> Result<(), LifecycleError> {
    let rootlight = binary_path(bin_dir, "rootlight")?;
    let daemon = binary_path(bin_dir, "rootlight-daemon")?;
    let temporary = tempfile::tempdir().map_err(LifecycleError::TemporaryDirectory)?;
    let paths = RuntimePaths::new(
        temporary.path().join("state"),
        temporary.path().join("runtime"),
    )
    .map_err(LifecycleError::Runtime)?;
    let environment = Environment::new(&paths);
    exercise_simultaneous_autostart(&paths, &rootlight, &environment)?;
    let mut process = SupervisedDaemon::spawn(&daemon, &environment)?;

    wait_until_ready(&paths, &rootlight, &environment)?;
    let health = run_json(&rootlight, &["health"], &environment, COMMAND_TIMEOUT)?;
    assert_success_type(&health, "health")?;
    exercise_concurrent_clients(&rootlight, &environment)?;

    let operation = OperationId::from_bytes([42; 16]).to_string();
    let submitted = run_json(
        &rootlight,
        &["operation-submit", &operation],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&submitted, "operation_submit")?;
    let retried = run_json(
        &rootlight,
        &["operation-submit", &operation],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&retried, "operation_submit")?;
    assert_same_operation(&submitted, &retried)?;
    wait_for_terminal(&rootlight, &operation, &environment)?;

    let cancelled_operation = OperationId::from_bytes([44; 16]).to_string();
    let cancelled_submit = run_json(
        &rootlight,
        &["operation-submit", &cancelled_operation],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&cancelled_submit, "operation_submit")?;
    let cancelled = run_json(
        &rootlight,
        &["operation-cancel", &cancelled_operation],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&cancelled, "operation_cancel")?;
    assert_cancel_accepted(&cancelled)?;
    let cancelled_terminal = wait_for_terminal(&rootlight, &cancelled_operation, &environment)?;
    assert_operation_state(&cancelled_terminal, "cancelled")?;

    let deadline_operation = OperationId::from_bytes([45; 16]).to_string();
    let deadline_submit = run_json(
        &rootlight,
        &[
            "operation-submit",
            &deadline_operation,
            "--timeout-ms",
            "25",
        ],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&deadline_submit, "operation_submit")?;
    let deadline_terminal = wait_for_terminal(&rootlight, &deadline_operation, &environment)?;
    assert_operation_state(&deadline_terminal, "interrupted")?;
    assert_recovery_class(&deadline_terminal, "deadline_elapsed")?;

    let stable_retry_operation = OperationId::from_bytes([47; 16]).to_string();
    let stable_deadline = unix_time_ms()?
        .checked_add(4_000)
        .ok_or(LifecycleError::Clock)?
        .to_string();
    let stable_submit = run_json(
        &rootlight,
        &[
            "operation-submit",
            &stable_retry_operation,
            "--deadline-unix-ms",
            &stable_deadline,
        ],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    let stable_retry = run_json(
        &rootlight,
        &[
            "operation-submit",
            &stable_retry_operation,
            "--deadline-unix-ms",
            &stable_deadline,
        ],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_same_operation(&stable_submit, &stable_retry)?;
    assert_timing_fields(&stable_retry, Some(stable_deadline.as_str()), None)?;
    wait_for_terminal(&rootlight, &stable_retry_operation, &environment)?;

    let lease_operation = OperationId::from_bytes([48; 16]);
    let initial_lease = unix_time_ms()?
        .checked_add(60_000)
        .ok_or(LifecycleError::Clock)?;
    let renewed_lease = initial_lease
        .checked_add(60_000)
        .ok_or(LifecycleError::Clock)?;
    let lease_client = Client::connect_or_start(&paths, [48; 16], ConnectPolicy::ExistingOnly)
        .map_err(LifecycleError::Client)?;
    lease_client
        .operation_submit_attached(lease_operation, None, initial_lease)
        .map_err(LifecycleError::Client)?;
    let renewed = lease_client
        .operation_renew_lease(lease_operation, renewed_lease)
        .map_err(LifecycleError::Client)?;
    if renewed.lease_expires_unix_ms != Some(renewed_lease) {
        return Err(LifecycleError::UnexpectedEnvelope);
    }
    let lease_operation = lease_operation.to_string();
    let lease_terminal = wait_for_terminal(&rootlight, &lease_operation, &environment)?;
    assert_operation_state(&lease_terminal, "succeeded")?;

    let expired_lease_operation = OperationId::from_bytes([49; 16]).to_string();
    let expired_lease = unix_time_ms()?
        .checked_add(1_500)
        .ok_or(LifecycleError::Clock)?
        .to_string();
    let expired_submit = run_json(
        &rootlight,
        &[
            "operation-submit",
            &expired_lease_operation,
            "--lease-expires-unix-ms",
            &expired_lease,
        ],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&expired_submit, "operation_submit")?;
    let expired_terminal = wait_for_terminal(&rootlight, &expired_lease_operation, &environment)?;
    assert_operation_state(&expired_terminal, "interrupted")?;
    assert_recovery_class(&expired_terminal, "lease_expired")?;

    let writer_conflict = run_json_allow_failure(
        &rootlight,
        &["--standalone", "health"],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_error_code(&writer_conflict, "BUSY")?;

    exercise_stalled_peer_shutdown(&paths, &mut process)?;
    wait_until_absent(&paths)?;

    exercise_crash_restart(&paths, &daemon, &rootlight, &environment)?;

    let standalone = run_json(
        &rootlight,
        &["--standalone", "operation-status", &operation],
        &environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&standalone, "operation_status")?;

    let parity_operation = OperationId::from_bytes([43; 16]).to_string();
    let standalone_environment = isolated_environment(temporary.path(), "standalone-parity")?;
    let standalone_submit = run_json(
        &rootlight,
        &["--standalone", "operation-submit", &parity_operation],
        &standalone_environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&standalone_submit, "operation_submit")?;
    let daemon_paths = isolated_paths(temporary.path(), "daemon-parity")?;
    let daemon_environment = Environment::new(&daemon_paths);
    let mut parity_daemon = SupervisedDaemon::spawn(&daemon, &daemon_environment)?;
    wait_until_ready(&daemon_paths, &rootlight, &daemon_environment)?;
    let daemon_submit = run_json(
        &rootlight,
        &["operation-submit", &parity_operation],
        &daemon_environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&daemon_submit, "operation_submit")?;
    assert_operation_submit_equivalent(&standalone_submit, &daemon_submit)?;
    parity_daemon.shutdown()?;
    wait_until_absent(&daemon_paths)?;

    println!(
        "daemon lifecycle check passed: startup, 100 concurrent clients, health, retry-safe submission, cancellation, stable deadlines, attached lease renewal and expiry, crash recovery, daemon/standalone submit parity, writer exclusion, stalled-peer shutdown, graceful cleanup, and durable standalone status"
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct Environment {
    state: PathBuf,
    runtime: PathBuf,
}

impl Environment {
    fn new(paths: &RuntimePaths) -> Self {
        Self {
            state: paths.state_dir().to_path_buf(),
            runtime: paths.runtime_dir().to_path_buf(),
        }
    }

    fn apply(&self, command: &mut Command) {
        command
            .env("ROOTLIGHT_STATE_DIR", &self.state)
            .env("ROOTLIGHT_RUNTIME_DIR", &self.runtime);
    }
}

fn isolated_paths(root: &Path, label: &str) -> Result<RuntimePaths, LifecycleError> {
    RuntimePaths::new(
        root.join(label).join("state"),
        root.join(label).join("runtime"),
    )
    .map_err(LifecycleError::Runtime)
}

fn isolated_environment(root: &Path, label: &str) -> Result<Environment, LifecycleError> {
    isolated_paths(root, label).map(|paths| Environment::new(&paths))
}

struct SupervisedDaemon {
    child: Child,
}

impl SupervisedDaemon {
    fn spawn(binary: &Path, environment: &Environment) -> Result<Self, LifecycleError> {
        let mut command = Command::new(binary);
        environment.apply(&mut command);
        command
            .arg("--supervised-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(LifecycleError::SpawnDaemon)?;
        Ok(Self { child })
    }

    fn terminate(&mut self) -> Result<(), LifecycleError> {
        self.child.kill().map_err(LifecycleError::TerminateChild)?;
        let status = wait_child(&mut self.child, STOP_TIMEOUT)?;
        if status.success() {
            return Err(LifecycleError::CrashExitSucceeded);
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), LifecycleError> {
        if let Some(mut input) = self.child.stdin.take() {
            input
                .write_all(b"shutdown\n")
                .map_err(LifecycleError::WriteShutdown)?;
            input.flush().map_err(LifecycleError::WriteShutdown)?;
        }
        let status = wait_child(&mut self.child, STOP_TIMEOUT)?;
        if !status.success() {
            let stderr = read_child_stderr(&mut self.child)?;
            return Err(LifecycleError::DaemonFailed { status, stderr });
        }
        Ok(())
    }
}

impl Drop for SupervisedDaemon {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

fn wait_until_ready(
    paths: &RuntimePaths,
    rootlight: &Path,
    environment: &Environment,
) -> Result<(), LifecycleError> {
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    loop {
        match paths.discover() {
            Ok(_) => {
                if let Ok(value) = run_json(rootlight, &["health"], environment, COMMAND_TIMEOUT)
                    && value["ok"] == true
                    && value["result"]["data"]["ready"] == true
                {
                    return Ok(());
                }
            }
            Err(error) if runtime_absence(&error) => {}
            Err(error) => return Err(LifecycleError::Runtime(error)),
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::ReadyTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn exercise_simultaneous_autostart(
    paths: &RuntimePaths,
    rootlight: &Path,
    environment: &Environment,
) -> Result<(), LifecycleError> {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(CLIENT_COUNT));
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for index in 0..CLIENT_COUNT {
        let rootlight = rootlight.to_path_buf();
        let environment = environment.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        clients.push(thread::spawn(move || {
            barrier.wait();
            let mut command = Command::new(rootlight);
            environment.apply(&mut command);
            command
                .arg("health")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
                .spawn()
                .map_err(LifecycleError::SpawnCommand)
                .map(|child| (index, child))
        }));
    }
    let mut children = Vec::with_capacity(CLIENT_COUNT);
    for client in clients {
        children.push(
            client
                .join()
                .map_err(|_| LifecycleError::ClientThreadPanicked)??,
        );
    }
    let deadline = Instant::now()
        .checked_add(START_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    while !children.is_empty() {
        let mut index = 0_usize;
        while index < children.len() {
            let status = children[index]
                .1
                .try_wait()
                .map_err(LifecycleError::WaitChild)?;
            if let Some(status) = status {
                let (client_index, child) = children.swap_remove(index);
                drop(child);
                if !status.success() {
                    return Err(LifecycleError::AutostartClient {
                        index: client_index,
                        source: Box::new(LifecycleError::CommandFailed {
                            status,
                            stderr: String::new(),
                        }),
                    });
                }
            } else {
                index = index.saturating_add(1);
            }
        }
        if !children.is_empty() && Instant::now() >= deadline {
            for (_, mut child) in children {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(LifecycleError::CommandTimedOut);
        }
        if !children.is_empty() {
            thread::sleep(POLL_INTERVAL);
        }
    }
    let discovery = paths.discover().map_err(LifecycleError::Runtime)?;
    terminate_process(discovery.pid())?;
    wait_until_process_exit(discovery.pid())?;
    paths
        .remove_stale_endpoint(discovery.instance_nonce())
        .map_err(LifecycleError::Runtime)?;
    paths
        .remove_discovery_if_matches(discovery.instance_nonce())
        .map_err(LifecycleError::Runtime)?;
    wait_until_absent(paths)
}

fn exercise_crash_restart(
    paths: &RuntimePaths,
    daemon: &Path,
    rootlight: &Path,
    environment: &Environment,
) -> Result<(), LifecycleError> {
    let mut process = SupervisedDaemon::spawn(daemon, environment)?;
    wait_until_ready(paths, rootlight, environment)?;
    let operation = OperationId::from_bytes([46; 16]).to_string();
    let submitted = run_json(
        rootlight,
        &["operation-submit", &operation],
        environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&submitted, "operation_submit")?;
    assert_operation_state(&submitted, "running")?;

    let discovery = paths.discover().map_err(LifecycleError::Runtime)?;
    process.terminate()?;
    paths
        .remove_stale_endpoint(discovery.instance_nonce())
        .map_err(LifecycleError::Runtime)?;
    paths
        .remove_discovery_if_matches(discovery.instance_nonce())
        .map_err(LifecycleError::Runtime)?;
    wait_until_absent(paths)?;

    let mut restarted = SupervisedDaemon::spawn(daemon, environment)?;
    wait_until_ready(paths, rootlight, environment)?;
    let recovered = run_json(
        rootlight,
        &["operation-status", &operation],
        environment,
        COMMAND_TIMEOUT,
    )?;
    assert_success_type(&recovered, "operation_status")?;
    assert_operation_state(&recovered, "interrupted")?;
    assert_recovery_class(&recovered, "interrupted_by_restart")?;
    restarted.shutdown()?;
    wait_until_absent(paths)
}

fn exercise_stalled_peer_shutdown(
    paths: &RuntimePaths,
    process: &mut SupervisedDaemon,
) -> Result<(), LifecycleError> {
    let discovery = paths.discover().map_err(LifecycleError::Runtime)?;
    let endpoint = discovery.endpoint(paths).map_err(LifecycleError::Runtime)?;
    let stalled = connect(&endpoint).map_err(LifecycleError::Ipc)?;

    process.shutdown()?;
    drop(stalled);
    wait_until_absent(paths)
}

fn exercise_concurrent_clients(
    rootlight: &Path,
    environment: &Environment,
) -> Result<(), LifecycleError> {
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for _ in 0..CLIENT_COUNT {
        let rootlight = rootlight.to_path_buf();
        let environment = environment.clone();
        clients.push(thread::spawn(move || {
            run_json(&rootlight, &["health"], &environment, COMMAND_TIMEOUT)
        }));
    }
    for client in clients {
        let value = client
            .join()
            .map_err(|_| LifecycleError::ClientThreadPanicked)??;
        assert_success_type(&value, "health")?;
    }
    Ok(())
}

fn wait_for_terminal(
    rootlight: &Path,
    operation: &str,
    environment: &Environment,
) -> Result<Value, LifecycleError> {
    let deadline = Instant::now()
        .checked_add(COMMAND_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    loop {
        let value = run_json(
            rootlight,
            &["operation-status", operation],
            environment,
            COMMAND_TIMEOUT,
        )?;
        assert_success_type(&value, "operation_status")?;
        if matches!(
            value["result"]["data"]["state"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "interrupted")
        ) {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::OperationTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_until_absent(paths: &RuntimePaths) -> Result<(), LifecycleError> {
    let deadline = Instant::now()
        .checked_add(STOP_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    loop {
        match paths.discover() {
            Err(error) if runtime_absence(&error) => return Ok(()),
            Err(error) => return Err(LifecycleError::Runtime(error)),
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::CleanupTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn run_json(
    binary: &Path,
    arguments: &[&str],
    environment: &Environment,
    timeout: Duration,
) -> Result<Value, LifecycleError> {
    let output = run_command(binary, arguments, environment, timeout)?;
    if !output.status.success() {
        return Err(LifecycleError::CommandFailed {
            status: output.status,
            stderr: format!(
                "arguments={arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    parse_single_json(&output.stdout)
}

fn run_json_allow_failure(
    binary: &Path,
    arguments: &[&str],
    environment: &Environment,
    timeout: Duration,
) -> Result<Value, LifecycleError> {
    let output = run_command(binary, arguments, environment, timeout)?;
    let bytes = if output.status.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    parse_single_json(bytes)
}

fn run_command(
    binary: &Path,
    arguments: &[&str],
    environment: &Environment,
    timeout: Duration,
) -> Result<Output, LifecycleError> {
    let mut command = Command::new(binary);
    environment.apply(&mut command);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(LifecycleError::SpawnCommand)?;
    wait_output(child, timeout)
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<(), LifecycleError> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(LifecycleError::TerminateProcess)?;
    if status.success() {
        Ok(())
    } else {
        Err(LifecycleError::TerminateProcessFailed(status))
    }
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<(), LifecycleError> {
    let status = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(LifecycleError::TerminateProcess)?;
    if status.success() {
        Ok(())
    } else {
        Err(LifecycleError::TerminateProcessFailed(status))
    }
}

fn wait_until_process_exit(pid: u32) -> Result<(), LifecycleError> {
    let deadline = Instant::now()
        .checked_add(STOP_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    loop {
        if !process_is_running(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::ProcessExitTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> Result<bool, LifecycleError> {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(LifecycleError::ProcessProbe)?;
    if !output.status.success() {
        return Err(LifecycleError::ProcessProbeFailed(output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(!text.contains("No tasks are running") && text.contains(&pid.to_string()))
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> Result<bool, LifecycleError> {
    let status = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(LifecycleError::ProcessProbe)?;
    Ok(status.success())
}

fn wait_output(mut child: Child, timeout: Duration) -> Result<Output, LifecycleError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(LifecycleError::Clock)?;
    loop {
        match child.try_wait().map_err(LifecycleError::WaitChild)? {
            Some(status) => return read_completed_output(&mut child, status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LifecycleError::CommandTimedOut);
            }
            None => thread::sleep(POLL_INTERVAL),
        }
    }
}

fn read_completed_output(child: &mut Child, status: ExitStatus) -> Result<Output, LifecycleError> {
    let stdout = child
        .stdout
        .take()
        .map(read_stream)
        .transpose()?
        .unwrap_or_default();
    let stderr = child
        .stderr
        .take()
        .map(read_stream)
        .transpose()?
        .unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn wait_child(child: &mut Child, timeout: Duration) -> Result<ExitStatus, LifecycleError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(LifecycleError::Clock)?;
    loop {
        if let Some(status) = child.try_wait().map_err(LifecycleError::WaitChild)? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LifecycleError::CommandTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_stream(mut stream: impl io::Read) -> Result<Vec<u8>, LifecycleError> {
    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(LifecycleError::ReadOutput)?;
    Ok(bytes)
}

fn read_child_stderr(child: &mut Child) -> Result<String, LifecycleError> {
    child
        .stderr
        .take()
        .map(read_stream)
        .transpose()
        .map(|value| String::from_utf8_lossy(&value.unwrap_or_default()).into_owned())
}

fn unix_time_ms() -> Result<u64, LifecycleError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LifecycleError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| LifecycleError::Clock)
}

fn parse_single_json(bytes: &[u8]) -> Result<Value, LifecycleError> {
    let text = std::str::from_utf8(bytes).map_err(LifecycleError::OutputUtf8)?;
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let value = values
        .next()
        .transpose()
        .map_err(LifecycleError::OutputJson)?
        .ok_or(LifecycleError::MissingOutput)?;
    if values
        .next()
        .transpose()
        .map_err(LifecycleError::OutputJson)?
        .is_some()
    {
        return Err(LifecycleError::MultipleOutputs);
    }
    Ok(value)
}

fn assert_success_type(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if value["contract_version"] == "1.0"
        && value["ok"] == true
        && value["result"]["type"] == expected
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_error_code(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if value["contract_version"] == "1.0"
        && value["ok"] == false
        && value["error"]["code"] == expected
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_cancel_accepted(value: &Value) -> Result<(), LifecycleError> {
    if value["result"]["data"]["accepted"] == true {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_operation_state(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if value["result"]["data"]["state"] == expected {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_recovery_class(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if value["result"]["data"]["recovery_class"] == expected {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_timing_fields(
    value: &Value,
    deadline_unix_ms: Option<&str>,
    lease_expires_unix_ms: Option<&str>,
) -> Result<(), LifecycleError> {
    let observed_deadline = value["result"]["data"]["deadline_unix_ms"]
        .as_u64()
        .map(|value| value.to_string());
    let observed_lease = value["result"]["data"]["lease_expires_unix_ms"]
        .as_u64()
        .map(|value| value.to_string());
    if observed_deadline.as_deref() == deadline_unix_ms
        && observed_lease.as_deref() == lease_expires_unix_ms
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_same_operation(left: &Value, right: &Value) -> Result<(), LifecycleError> {
    if left["result"]["data"]["operation"] == right["result"]["data"]["operation"]
        && left["result"]["data"]["kind"] == right["result"]["data"]["kind"]
        && left["result"]["data"]["plan_hash"] == right["result"]["data"]["plan_hash"]
        && left["result"]["data"]["detached"] == right["result"]["data"]["detached"]
        && left["result"]["data"]["deadline_unix_ms"] == right["result"]["data"]["deadline_unix_ms"]
        && left["result"]["data"]["lease_expires_unix_ms"]
            == right["result"]["data"]["lease_expires_unix_ms"]
    {
        Ok(())
    } else {
        Err(LifecycleError::OperationSubmitMismatch)
    }
}

fn assert_operation_submit_equivalent(left: &Value, right: &Value) -> Result<(), LifecycleError> {
    if left == right {
        Ok(())
    } else {
        Err(LifecycleError::OperationSubmitMismatch)
    }
}

fn runtime_absence(error: &RuntimeError) -> bool {
    matches!(
        error,
        RuntimeError::Io(source) if source.kind() == io::ErrorKind::NotFound
    )
}

fn binary_path(directory: &Path, name: &str) -> Result<PathBuf, LifecycleError> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = directory.join(file);
    if path.is_file() {
        Ok(path)
    } else {
        Err(LifecycleError::MissingBinary(path))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LifecycleError {
    #[error("daemon lifecycle binary is missing: {0}")]
    MissingBinary(PathBuf),
    #[error("failed to create daemon lifecycle temporary directory")]
    TemporaryDirectory(#[source] io::Error),
    #[error("daemon lifecycle runtime setup failed")]
    Runtime(#[source] RuntimeError),
    #[error("daemon lifecycle IPC setup failed")]
    Ipc(#[source] rootlight_ipc::IpcError),
    #[error("daemon lifecycle client failed")]
    Client(#[source] rootlight_client::ClientError),
    #[error("failed to spawn supervised daemon")]
    SpawnDaemon(#[source] io::Error),
    #[error("failed to spawn lifecycle command")]
    SpawnCommand(#[source] io::Error),
    #[error("failed to send supervised shutdown")]
    WriteShutdown(#[source] io::Error),
    #[error("failed to wait for lifecycle child")]
    WaitChild(#[source] io::Error),
    #[error("failed to terminate supervised daemon")]
    TerminateChild(#[source] io::Error),
    #[error("forced daemon termination unexpectedly returned success")]
    CrashExitSucceeded,
    #[error("failed to terminate autostarted daemon")]
    TerminateProcess(#[source] io::Error),
    #[error("autostarted daemon termination failed with {0}")]
    TerminateProcessFailed(ExitStatus),
    #[error("failed to probe autostarted daemon liveness")]
    ProcessProbe(#[source] io::Error),
    #[error("autostarted daemon liveness probe failed with {0}")]
    ProcessProbeFailed(ExitStatus),
    #[error("autostarted daemon did not exit within the cleanup deadline")]
    ProcessExitTimedOut,
    #[error("failed to read lifecycle command output")]
    ReadOutput(#[source] io::Error),
    #[error("daemon lifecycle command timed out")]
    CommandTimedOut,
    #[error("daemon readiness timed out")]
    ReadyTimedOut,
    #[error("daemon operation did not reach a terminal state")]
    OperationTimedOut,
    #[error("daemon lifecycle client thread panicked")]
    ClientThreadPanicked,
    #[error("simultaneous autostart client {index} failed: {source}")]
    AutostartClient {
        index: usize,
        #[source]
        source: Box<LifecycleError>,
    },
    #[error("daemon discovery cleanup timed out")]
    CleanupTimedOut,
    #[error("monotonic lifecycle deadline overflowed")]
    Clock,
    #[error("lifecycle command failed with {status}: {stderr}")]
    CommandFailed { status: ExitStatus, stderr: String },
    #[error("supervised daemon failed with {status}: {stderr}")]
    DaemonFailed { status: ExitStatus, stderr: String },
    #[error("lifecycle output was not UTF-8")]
    OutputUtf8(#[source] std::str::Utf8Error),
    #[error("lifecycle output was not valid JSON")]
    OutputJson(#[source] serde_json::Error),
    #[error("lifecycle command produced no JSON envelope")]
    MissingOutput,
    #[error("lifecycle command produced multiple JSON envelopes")]
    MultipleOutputs,
    #[error("lifecycle command returned an unexpected envelope")]
    UnexpectedEnvelope,
    #[error("daemon operation-submit retry or standalone parity check failed")]
    OperationSubmitMismatch,
}
