//! Portable real-process evidence for daemon startup, control, and cleanup.
//!
//! The harness uses validated discovery and negotiated health as readiness events,
//! then asks a supervised daemon to shut down through stdin rather than sleeping.

use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{Arc, Barrier},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rootlight_client::{
    Client, ClientError, ConnectPolicy, DaemonLifecycle, Health, OperationState, OwnedDaemon,
};
use rootlight_error::{ErrorCode, PublicValue};
use rootlight_ids::OperationId;
use rootlight_ipc::{IpcError, connect};
use rootlight_observability::{
    CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION, ControlMethod,
    DaemonLifecycle as TelemetryDaemonLifecycle, LogEvent, MAX_STRUCTURED_LOG_LINE_BYTES, SpanKind,
    StructuredLogRecord, TELEMETRY_SCHEMA_VERSION, TelemetryOutcome,
};
use rootlight_runtime::{RuntimeError, RuntimePaths};
use serde_json::Value;

const START_TIMEOUT: Duration = Duration::from_secs(10);
const CLIENT_COUNT: usize = 100;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const EXPECTED_CLIENT_CONNECTION_LIMIT: u32 = 8;
const EXPECTED_CLIENT_OPERATION_LIMIT: u32 = 32;
const EXPECTED_OPERATION_WORKERS: usize = 4;
const QUOTA_GUARD_IDENTITIES: [u8; 8] = [72, 73, 74, 75, 76, 77, 78, 79];
const QUOTA_GUARD_MIN_QUEUED: u32 = 48;
const QUOTA_WINDOW_TIMEOUT: Duration = Duration::from_secs(15);
const QUOTA_EXIT_CONFIRM_TIMEOUT: Duration = Duration::from_millis(500);
const REFERENCE_CONTROL_P95_TARGET: Duration = Duration::from_millis(50);
const MAX_DAEMON_STDERR_BYTES: usize = 1024 * 1024;

pub(crate) fn check(bin_dir: &Path) -> Result<(), LifecycleError> {
    let rootlight = binary_path(bin_dir, "rootlight")?;
    let daemon = binary_path(bin_dir, "rootlight-daemon")?;
    let temporary = lifecycle_tempdir().map_err(LifecycleError::TemporaryDirectory)?;
    let paths = RuntimePaths::new(
        temporary.path().join("state"),
        temporary.path().join("runtime"),
    )
    .map_err(LifecycleError::Runtime)?;
    let environment = Environment::new(&paths);
    exercise_simultaneous_autostart(&paths)?;
    let mut process = SupervisedDaemon::spawn(&daemon, &environment)?;

    wait_until_ready(&paths, &rootlight, &environment)?;
    let health = run_json(&rootlight, &["health"], &environment, COMMAND_TIMEOUT)?;
    assert_success_type(&health, "health")?;
    exercise_concurrent_clients(&paths)?;
    if let Err(error) = exercise_operation_quota_isolation(&paths) {
        return Err(process.contextualize_quota_failure(error, &environment.private_values()));
    }
    let control_latency = exercise_saturated_control_responsiveness(&paths)?;

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
    let renewal = match lease_client.operation_renew_lease(lease_operation, renewed_lease) {
        Ok(_) => return Err(LifecycleError::UnexpectedEnvelope),
        Err(error) => error,
    };
    if renewal
        .as_public_error()
        .map(rootlight_error::PublicError::code)
        != Some(ErrorCode::UnsupportedCapability)
    {
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

    let evidence_client = Client::connect_or_start(&paths, [62; 16], ConnectPolicy::ExistingOnly)
        .map_err(LifecycleError::Client)?;
    let telemetry_health = evidence_client.health().map_err(LifecycleError::Client)?;
    let first_support = evidence_client
        .support_bundle()
        .map_err(LifecycleError::Client)?;
    let support = evidence_client
        .support_bundle()
        .map_err(LifecycleError::Client)?;
    assert_support_telemetry(&telemetry_health, &first_support, &support)?;
    assert_privacy_sentinels_absent(
        "support archive",
        &support.archive,
        &environment.private_values(),
    )?;

    let daemon_stderr = exercise_stalled_peer_shutdown(&paths, &mut process)?;
    validate_daemon_stderr(&daemon_stderr, &environment.private_values())?;
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
    let parity_stderr = parity_daemon.shutdown()?;
    assert_privacy_sentinels_absent(
        "parity daemon stderr",
        &parity_stderr,
        &daemon_environment.private_values(),
    )?;
    wait_until_absent(&daemon_paths)?;

    println!(
        "daemon lifecycle check passed: startup, 100 deterministic concurrent clients, per-client operation quota isolation, saturated-worker control responsiveness, health, retry-safe submission, cancellation, stable deadlines, explicit lease-renewal rejection and attached lease expiry, schema-v3 support telemetry, bounded structured JSONL, privacy sentinels, crash recovery, daemon/standalone submit parity, writer exclusion, stalled-peer shutdown, graceful cleanup, and durable standalone status"
    );
    control_latency.report();
    Ok(())
}

fn lifecycle_tempdir() -> Result<tempfile::TempDir, std::io::Error> {
    #[cfg(target_os = "macos")]
    {
        // The default macOS TMPDIR is long enough to overflow `sun_path` once
        // Rootlight appends its authenticated endpoint name.
        tempfile::Builder::new()
            .prefix("rl-life-")
            .tempdir_in("/private/tmp")
    }
    #[cfg(not(target_os = "macos"))]
    {
        tempfile::tempdir()
    }
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

    fn private_values(&self) -> Vec<Vec<u8>> {
        [self.state.as_path(), self.runtime.as_path()]
            .into_iter()
            .flat_map(path_privacy_encodings)
            .collect()
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
    stderr_reader: Option<JoinHandle<Result<Vec<u8>, LifecycleError>>>,
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
        let mut child = command.spawn().map_err(LifecycleError::SpawnDaemon)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(LifecycleError::MissingDaemonStderr)?;
        let stderr_reader = match thread::Builder::new()
            .name("rootlight-daemon-stderr".to_owned())
            .spawn(move || read_bounded_stream(stderr))
        {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(LifecycleError::SpawnDaemonStderrReader(error));
            }
        };
        Ok(Self {
            child,
            stderr_reader: Some(stderr_reader),
        })
    }

    fn terminate(&mut self) -> Result<Vec<u8>, LifecycleError> {
        self.child.kill().map_err(LifecycleError::TerminateChild)?;
        let status = wait_child(&mut self.child, STOP_TIMEOUT)?;
        let stderr = self.take_stderr()?;
        if status.success() {
            return Err(LifecycleError::CrashExitSucceeded);
        }
        Ok(stderr)
    }

    fn shutdown(&mut self) -> Result<Vec<u8>, LifecycleError> {
        if let Some(mut input) = self.child.stdin.take() {
            input
                .write_all(b"shutdown\n")
                .map_err(LifecycleError::WriteShutdown)?;
            input.flush().map_err(LifecycleError::WriteShutdown)?;
        }
        let status = wait_child(&mut self.child, STOP_TIMEOUT)?;
        let stderr = self.take_stderr()?;
        if !status.success() {
            return Err(LifecycleError::DaemonFailed {
                status,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
        Ok(stderr)
    }

    fn contextualize_quota_failure(
        &mut self,
        source: LifecycleError,
        private_values: &[Vec<u8>],
    ) -> LifecycleError {
        let Some(deadline) = Instant::now().checked_add(QUOTA_EXIT_CONFIRM_TIMEOUT) else {
            return LifecycleError::OperationQuotaProcessStateUnavailable {
                source: Box::new(source),
            };
        };
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    let stderr = self
                        .take_stderr()
                        .map_or(QuotaDaemonStderr::Unavailable, |stderr| {
                            privacy_checked_quota_stderr(&stderr, private_values)
                        });
                    return LifecycleError::DaemonExitedDuringQuota {
                        status,
                        stderr,
                        source: Box::new(source),
                    };
                }
                Ok(None) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return LifecycleError::OperationQuotaFailed {
                            source: Box::new(source),
                        };
                    }
                    thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
                }
                Err(_) => {
                    return LifecycleError::OperationQuotaProcessStateUnavailable {
                        source: Box::new(source),
                    };
                }
            }
        }
    }

    fn take_stderr(&mut self) -> Result<Vec<u8>, LifecycleError> {
        self.stderr_reader
            .take()
            .ok_or(LifecycleError::MissingDaemonStderr)?
            .join()
            .map_err(|_| LifecycleError::DaemonStderrThreadPanicked)?
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
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
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

fn exercise_simultaneous_autostart(paths: &RuntimePaths) -> Result<(), LifecycleError> {
    // Provision the shared security boundary once. The race under test starts
    // without a daemon or discovery record and targets launch authority.
    paths.prepare_owner().map_err(LifecycleError::Runtime)?;
    let barrier = Arc::new(Barrier::new(CLIENT_COUNT));
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for index in 0..CLIENT_COUNT {
        let paths = paths.clone();
        let barrier = Arc::clone(&barrier);
        clients.push(thread::spawn(move || {
            barrier.wait();
            let identity = deterministic_client_identity(index)?;
            Client::connect_or_start_owned(&paths, identity, ConnectPolicy::StartIfMissing)
                .map_err(LifecycleError::Client)
        }));
    }
    let mut connected = Vec::with_capacity(CLIENT_COUNT);
    let mut owner: Option<OwnedDaemon> = None;
    for client in clients {
        let (client, owned) = client
            .join()
            .map_err(|_| LifecycleError::ClientThreadPanicked)??;
        if !client.health().map_err(LifecycleError::Client)?.ready {
            return Err(LifecycleError::UnexpectedClientHealth);
        }
        if let Some(owned) = owned
            && owner.replace(owned).is_some()
        {
            return Err(LifecycleError::MultipleAutostartOwners);
        }
        connected.push(client);
    }
    let owner = owner.ok_or(LifecycleError::MissingAutostartOwner)?;
    owner.shutdown().map_err(LifecycleError::Client)?;
    drop(connected);
    wait_until_autostart_quiescent(paths)
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
    if !matches!(
        operation_data(&submitted)["state"].as_str(),
        Some("queued" | "running")
    ) {
        return Err(LifecycleError::UnexpectedEnvelope);
    }

    let discovery = paths.discover().map_err(LifecycleError::Runtime)?;
    let crash_stderr = process.terminate()?;
    assert_privacy_sentinels_absent(
        "crashed daemon stderr",
        &crash_stderr,
        &environment.private_values(),
    )?;
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
    let restarted_stderr = restarted.shutdown()?;
    assert_privacy_sentinels_absent(
        "restarted daemon stderr",
        &restarted_stderr,
        &environment.private_values(),
    )?;
    wait_until_absent(paths)
}

fn exercise_stalled_peer_shutdown(
    paths: &RuntimePaths,
    process: &mut SupervisedDaemon,
) -> Result<Vec<u8>, LifecycleError> {
    let discovery = paths.discover().map_err(LifecycleError::Runtime)?;
    let endpoint = discovery.endpoint(paths).map_err(LifecycleError::Runtime)?;
    let stalled = connect(&endpoint).map_err(LifecycleError::Ipc)?;

    let stderr = process.shutdown()?;
    drop(stalled);
    wait_until_absent(paths)?;
    Ok(stderr)
}

fn exercise_concurrent_clients(paths: &RuntimePaths) -> Result<(), LifecycleError> {
    let barrier = Arc::new(Barrier::new(CLIENT_COUNT));
    let mut clients = Vec::with_capacity(CLIENT_COUNT);
    for index in 0..CLIENT_COUNT {
        let paths = paths.clone();
        let barrier = Arc::clone(&barrier);
        clients.push(thread::spawn(move || {
            let identity = deterministic_client_identity(index)?;
            let client = Client::connect_or_start(&paths, identity, ConnectPolicy::ExistingOnly)
                .map_err(LifecycleError::Client);
            barrier.wait();
            let client = client?;
            client.health().map_err(LifecycleError::Client)
        }));
    }
    for client in clients {
        let health = client
            .join()
            .map_err(|_| LifecycleError::ClientThreadPanicked)??;
        if !health.ready
            || health.lifecycle != DaemonLifecycle::Ready
            || !health.accepting_operations
        {
            return Err(LifecycleError::UnexpectedClientHealth);
        }
    }
    let observer = Client::connect_or_start(paths, [59; 16], ConnectPolicy::ExistingOnly)
        .map_err(LifecycleError::Client)?;
    // The observer itself is the only connection allowed across the scenario boundary.
    wait_for_health(&observer, |health| health.active_connections == 1).map_err(
        |error| match error {
            LifecycleError::HealthStateTimedOut => LifecycleError::ConcurrentClientDrainTimedOut,
            other => other,
        },
    )?;
    Ok(())
}

fn exercise_operation_quota_isolation(paths: &RuntimePaths) -> Result<(), LifecycleError> {
    let noisy = Arc::new(
        Client::connect_or_start(paths, [70; 16], ConnectPolicy::ExistingOnly)
            .map_err(LifecycleError::Client)?,
    );
    let peer = Client::connect_or_start(paths, [71; 16], ConnectPolicy::ExistingOnly)
        .map_err(LifecycleError::Client)?;
    let worker_count = usize::try_from(EXPECTED_CLIENT_CONNECTION_LIMIT)
        .map_err(|_| LifecycleError::InvalidWorkerConfiguration)?;

    // A bounded guard backlog prevents finite probes from releasing noisy-client
    // permits while their submission burst is still crossing a slower IPC host.
    let mut guards = Vec::with_capacity(QUOTA_GUARD_IDENTITIES.len());
    for identity in QUOTA_GUARD_IDENTITIES {
        let guard = Arc::new(
            Client::connect_or_start(paths, [identity; 16], ConnectPolicy::ExistingOnly)
                .map_err(LifecycleError::Client)?,
        );
        let operations = (0..EXPECTED_CLIENT_CONNECTION_LIMIT)
            .map(|ordinal| quota_operation(identity, ordinal))
            .collect::<Result<Vec<_>, _>>()?;
        guards.push((guard, operations));
    }
    let guard_deadline = quota_deadline(COMMAND_TIMEOUT)?;
    submit_guard_operations(&guards, guard_deadline).map_err(|error| match error {
        LifecycleError::QuotaGuardWindowExpired => LifecycleError::QuotaGuardBacklogUnavailable,
        other => other,
    })?;

    let expected_running = default_worker_slots()?;
    let guard_ready_deadline = quota_deadline(COMMAND_TIMEOUT)?;
    let guard_health = wait_for_health_until(
        &peer,
        |health| {
            health.ready
                && health.lifecycle == DaemonLifecycle::Ready
                && health.accepting_operations
                && health.running_operations == expected_running
                && health.queued_operations >= QUOTA_GUARD_MIN_QUEUED
                && health
                    .running_operations
                    .checked_add(health.queued_operations)
                    == Some(health.admitted_operations)
        },
        guard_ready_deadline,
    )
    .map_err(|error| match error {
        LifecycleError::HealthStateTimedOut => LifecycleError::QuotaGuardBacklogUnavailable,
        other => other,
    })?;
    require_expected_default_limits(&guard_health)?;

    let quota_window = quota_deadline(QUOTA_WINDOW_TIMEOUT)?;
    let noisy_operations = (0..EXPECTED_CLIENT_OPERATION_LIMIT)
        .map(|ordinal| quota_operation(70, ordinal))
        .collect::<Result<Vec<_>, _>>()?;
    let noisy_operations = submit_operation_batch(
        Arc::clone(&noisy),
        noisy_operations,
        worker_count,
        quota_window,
    )?;

    let rejected = quota_operation(70, EXPECTED_CLIENT_OPERATION_LIMIT)?;
    let rejection = noisy.operation_submit(rejected);
    require_quota_window(quota_window)?;
    match rejection {
        Err(error) if is_client_operation_quota(&error) => {}
        Err(error) => return Err(LifecycleError::Client(error)),
        Ok(_) => return Err(LifecycleError::ClientOperationQuotaNotEnforced),
    }

    let saturated = wait_for_health_until(
        &peer,
        |health| {
            health.ready
                && health.lifecycle == DaemonLifecycle::Ready
                && health.accepting_operations
                && health.admitted_operations >= EXPECTED_CLIENT_OPERATION_LIMIT
                && health.running_operations == expected_running
                && health
                    .running_operations
                    .checked_add(health.queued_operations)
                    == Some(health.admitted_operations)
        },
        quota_window,
    )
    .map_err(|error| match error {
        LifecycleError::HealthStateTimedOut => LifecycleError::QuotaGuardWindowExpired,
        other => other,
    })?;
    require_expected_default_limits(&saturated)?;
    let peer_operation = quota_operation(71, 0)?;
    let peer_submission = peer.operation_submit(peer_operation);
    require_quota_window(quota_window)?;
    let peer_status = peer_submission.map_err(LifecycleError::Client)?;
    if !matches!(
        peer_status.state,
        OperationState::Running | OperationState::Queued
    ) {
        return Err(LifecycleError::UnexpectedQuotaOperationState(
            peer_status.state,
        ));
    }

    for operation in noisy_operations {
        cancel_and_wait(&noisy, operation)?;
    }
    cancel_and_wait(&peer, peer_operation)?;
    for (guard, operations) in guards {
        for operation in operations {
            cancel_and_wait(&guard, operation)?;
        }
    }
    wait_for_health(&peer, |health| {
        health.admitted_operations == 0
            && health.running_operations == 0
            && health.queued_operations == 0
    })?;
    Ok(())
}

fn exercise_saturated_control_responsiveness(
    paths: &RuntimePaths,
) -> Result<ControlLatencyEvidence, LifecycleError> {
    let workload = Arc::new(
        Client::connect_or_start(paths, [60; 16], ConnectPolicy::ExistingOnly)
            .map_err(LifecycleError::Client)?,
    );
    let sampler = Client::connect_or_start(paths, [61; 16], ConnectPolicy::ExistingOnly)
        .map_err(LifecycleError::Client)?;
    let operations = worker_operations()?;
    for operation in operations {
        let status = workload
            .operation_submit(operation)
            .map_err(LifecycleError::Client)?;
        if !matches!(
            status.state,
            OperationState::Running | OperationState::Queued
        ) {
            return Err(LifecycleError::UnexpectedSaturationState);
        }
    }
    let expected_worker_slots = default_worker_slots()?;
    let saturated = wait_for_health(&sampler, |health| {
        health.ready
            && health.lifecycle == DaemonLifecycle::Ready
            && health.accepting_operations
            && health.admitted_operations == expected_worker_slots
            && health.running_operations == expected_worker_slots
            && health.queued_operations == 0
    })?;
    require_expected_default_limits(&saturated)?;

    let mut health_samples = Vec::with_capacity(operations.len());
    let mut status_samples = Vec::with_capacity(operations.len());
    for operation in operations {
        let started = Instant::now();
        let health = sampler.health().map_err(LifecycleError::Client)?;
        health_samples.push(started.elapsed());
        require_saturated_health(&health)?;

        let started = Instant::now();
        let status = workload
            .operation_status(operation)
            .map_err(LifecycleError::Client)?;
        status_samples.push(started.elapsed());
        if status.state != OperationState::Running {
            return Err(LifecycleError::UnexpectedSampledOperationState(
                status.state,
            ));
        }
    }

    require_saturated_health(&sampler.health().map_err(LifecycleError::Client)?)?;
    let cancel_samples = cancel_saturated_workers(Arc::clone(&workload), operations)?;
    for operation in operations {
        wait_for_client_terminal(&workload, operation, OperationState::Cancelled)?;
    }
    wait_for_health(&sampler, |health| {
        health.admitted_operations == 0
            && health.running_operations == 0
            && health.queued_operations == 0
    })?;

    Ok(ControlLatencyEvidence {
        limits: ControlLimits::from_health(&saturated)?,
        health: LatencySeries::new(health_samples)?,
        status: LatencySeries::new(status_samples)?,
        cancel: LatencySeries::new(cancel_samples)?,
    })
}

fn wait_for_health(
    client: &Client,
    predicate: impl Fn(&Health) -> bool,
) -> Result<Health, LifecycleError> {
    wait_for_health_until(client, predicate, quota_deadline(COMMAND_TIMEOUT)?)
}

fn wait_for_health_until(
    client: &Client,
    predicate: impl Fn(&Health) -> bool,
    deadline: Instant,
) -> Result<Health, LifecycleError> {
    loop {
        if Instant::now() >= deadline {
            return Err(LifecycleError::HealthStateTimedOut);
        }
        let health = client.health();
        if Instant::now() >= deadline {
            return Err(LifecycleError::HealthStateTimedOut);
        }
        let health = health.map_err(LifecycleError::Client)?;
        if predicate(&health) {
            return Ok(health);
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::HealthStateTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn quota_operation(identity: u8, ordinal: u32) -> Result<OperationId, LifecycleError> {
    let mut bytes = [identity; 16];
    bytes[12..].copy_from_slice(&ordinal.to_be_bytes());
    if bytes == [0; 16] {
        return Err(LifecycleError::InvalidWorkerConfiguration);
    }
    Ok(OperationId::from_bytes(bytes))
}

fn submit_guard_operations(
    guards: &[(Arc<Client>, Vec<OperationId>)],
    deadline: Instant,
) -> Result<(), LifecycleError> {
    if guards.is_empty() {
        return Err(LifecycleError::InvalidWorkerConfiguration);
    }

    for (client, operations) in guards {
        if operations.is_empty() {
            return Err(LifecycleError::InvalidWorkerConfiguration);
        }
        let barrier = Arc::new(Barrier::new(operations.len()));
        let mut submissions = Vec::with_capacity(operations.len());
        for operation in operations {
            let client = Arc::clone(client);
            let barrier = Arc::clone(&barrier);
            let operation = *operation;
            submissions.push(thread::spawn(move || {
                barrier.wait();
                let status = submit_quota_operation_until(&client, operation, deadline)?;
                if !matches!(
                    status.state,
                    OperationState::Running | OperationState::Queued
                ) {
                    return Err(LifecycleError::UnexpectedQuotaOperationState(status.state));
                }
                Ok(())
            }));
        }

        for submission in submissions {
            submission
                .join()
                .map_err(|_| LifecycleError::ClientThreadPanicked)??;
        }
    }
    Ok(())
}

fn submit_operation_batch(
    client: Arc<Client>,
    operations: Vec<OperationId>,
    worker_count: usize,
    deadline: Instant,
) -> Result<Vec<OperationId>, LifecycleError> {
    if worker_count == 0 || operations.len() < worker_count {
        return Err(LifecycleError::InvalidWorkerConfiguration);
    }
    let operation_count = operations.len();
    let operations = Arc::new(operations);
    let barrier = Arc::new(Barrier::new(worker_count));
    let mut submissions = Vec::with_capacity(worker_count);
    for worker in 0..worker_count {
        let client = Arc::clone(&client);
        let operations = Arc::clone(&operations);
        let barrier = Arc::clone(&barrier);
        submissions.push(thread::spawn(move || {
            barrier.wait();
            for index in (worker..operation_count).step_by(worker_count) {
                let operation = operations[index];
                let status = submit_quota_operation_until(&client, operation, deadline)?;
                if !matches!(
                    status.state,
                    OperationState::Running | OperationState::Queued
                ) {
                    return Err(LifecycleError::UnexpectedQuotaOperationState(status.state));
                }
            }
            Ok(())
        }));
    }
    for submission in submissions {
        submission
            .join()
            .map_err(|_| LifecycleError::ClientThreadPanicked)??;
    }
    Arc::try_unwrap(operations).map_err(|_| LifecycleError::InvalidWorkerConfiguration)
}

fn submit_quota_operation_until(
    client: &Client,
    operation: OperationId,
    deadline: Instant,
) -> Result<rootlight_client::OperationStatus, LifecycleError> {
    loop {
        require_quota_window(deadline)?;
        match client.operation_submit(operation) {
            Ok(status) => {
                require_quota_window(deadline)?;
                return Ok(status);
            }
            Err(error) if is_retryable_quota_transport(&error) => {
                require_quota_window(deadline)?;
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                require_quota_window(deadline)?;
                return Err(LifecycleError::Client(error));
            }
        }
    }
}

fn is_retryable_quota_transport(error: &ClientError) -> bool {
    match error {
        ClientError::Ipc(IpcError::TimedOut | IpcError::ConnectTimedOut) => true,
        ClientError::Ipc(IpcError::Transport(error)) => error.kind() == io::ErrorKind::TimedOut,
        _ => false,
    }
}

fn quota_deadline(timeout: Duration) -> Result<Instant, LifecycleError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(LifecycleError::Clock)
}

fn require_quota_window(deadline: Instant) -> Result<(), LifecycleError> {
    if Instant::now() >= deadline {
        return Err(LifecycleError::QuotaGuardWindowExpired);
    }
    Ok(())
}

fn is_client_operation_quota(error: &ClientError) -> bool {
    let Some(public) = error.as_public_error() else {
        return false;
    };
    if public.code() != ErrorCode::ResourceExhausted || !public.retryable() {
        return false;
    }
    public.details().iter().any(|(key, value)| {
        key.as_str() == "client_operation_limit"
            && *value == PublicValue::Unsigned(u64::from(EXPECTED_CLIENT_OPERATION_LIMIT))
    })
}

fn cancel_and_wait(client: &Client, operation: OperationId) -> Result<(), LifecycleError> {
    let (accepted, status) = client
        .operation_cancel(operation)
        .map_err(LifecycleError::Client)?;
    match status.state {
        OperationState::Cancelling | OperationState::Cancelled if accepted => {
            wait_for_client_terminal(client, operation, OperationState::Cancelled)
        }
        OperationState::Succeeded if !accepted => Ok(()),
        _ => Err(LifecycleError::UnexpectedCancellationState),
    }
}

fn deterministic_client_identity(index: usize) -> Result<[u8; 16], LifecycleError> {
    let ordinal = u64::try_from(index)
        .map_err(|_| LifecycleError::InvalidClientIdentity)?
        .checked_add(1)
        .ok_or(LifecycleError::InvalidClientIdentity)?;
    let mut identity = [0_u8; 16];
    identity[..8].copy_from_slice(&ordinal.to_be_bytes());
    identity[8..].copy_from_slice(&ordinal.rotate_left(17).to_be_bytes());
    Ok(identity)
}

fn default_worker_slots() -> Result<u32, LifecycleError> {
    u32::try_from(EXPECTED_OPERATION_WORKERS)
        .map_err(|_| LifecycleError::InvalidWorkerConfiguration)
}

fn worker_operations() -> Result<[OperationId; EXPECTED_OPERATION_WORKERS], LifecycleError> {
    let mut operations = [OperationId::from_bytes([0; 16]); EXPECTED_OPERATION_WORKERS];
    for (index, operation) in operations.iter_mut().enumerate() {
        let ordinal = u8::try_from(index)
            .map_err(|_| LifecycleError::InvalidWorkerConfiguration)?
            .checked_add(60)
            .ok_or(LifecycleError::InvalidWorkerConfiguration)?;
        *operation = OperationId::from_bytes([ordinal; 16]);
    }
    Ok(operations)
}

fn require_expected_default_limits(health: &Health) -> Result<(), LifecycleError> {
    let workers = default_worker_slots()?;
    if health.connection_limit == 128
        && health.operation_queue_limit == 256
        && workers == 4
        && EXPECTED_CLIENT_CONNECTION_LIMIT <= health.connection_limit
        && EXPECTED_CLIENT_OPERATION_LIMIT <= health.operation_queue_limit
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedDefaultLimits {
            connection: health.connection_limit,
            operation_queue: health.operation_queue_limit,
            workers,
        })
    }
}

fn require_saturated_health(health: &Health) -> Result<(), LifecycleError> {
    let workers = default_worker_slots()?;
    if health.ready
        && health.lifecycle == DaemonLifecycle::Ready
        && health.accepting_operations
        && health.admitted_operations == workers
        && health.running_operations == workers
        && health.queued_operations == 0
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedSaturationHealth {
            admitted: health.admitted_operations,
            running: health.running_operations,
            queued: health.queued_operations,
        })
    }
}

fn cancel_saturated_workers<const N: usize>(
    client: Arc<Client>,
    operations: [OperationId; N],
) -> Result<Vec<Duration>, LifecycleError> {
    let barrier = Arc::new(Barrier::new(
        N.checked_add(1)
            .ok_or(LifecycleError::InvalidWorkerConfiguration)?,
    ));
    let mut requests = Vec::with_capacity(N);
    for operation in operations {
        let client = Arc::clone(&client);
        let barrier = Arc::clone(&barrier);
        requests.push(thread::spawn(move || {
            barrier.wait();
            let started = Instant::now();
            let result = client.operation_cancel(operation);
            (started.elapsed(), result)
        }));
    }
    barrier.wait();

    let mut samples = Vec::with_capacity(N);
    for request in requests {
        let (elapsed, result) = request
            .join()
            .map_err(|_| LifecycleError::ClientThreadPanicked)?;
        let (accepted, status) = result.map_err(LifecycleError::Client)?;
        if !accepted
            || !matches!(
                status.state,
                OperationState::Cancelling | OperationState::Cancelled
            )
        {
            return Err(LifecycleError::UnexpectedCancellationObservation {
                accepted,
                state: status.state,
            });
        }
        samples.push(elapsed);
    }
    Ok(samples)
}

fn wait_for_client_terminal(
    client: &Client,
    operation: OperationId,
    expected: OperationState,
) -> Result<(), LifecycleError> {
    let deadline = Instant::now()
        .checked_add(COMMAND_TIMEOUT)
        .ok_or(LifecycleError::Clock)?;
    loop {
        let status = client
            .operation_status(operation)
            .map_err(LifecycleError::Client)?;
        if status.state == expected {
            return Ok(());
        }
        if matches!(
            status.state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Interrupted
        ) {
            return Err(LifecycleError::UnexpectedCancellationState);
        }
        if Instant::now() >= deadline {
            return Err(LifecycleError::OperationTimedOut);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[derive(Debug)]
struct ControlLimits {
    connection_limit: u32,
    client_connection_limit: u32,
    operation_queue_limit: u32,
    client_operation_limit: u32,
    worker_slots: u32,
}

impl ControlLimits {
    fn from_health(health: &Health) -> Result<Self, LifecycleError> {
        Ok(Self {
            connection_limit: health.connection_limit,
            client_connection_limit: EXPECTED_CLIENT_CONNECTION_LIMIT,
            operation_queue_limit: health.operation_queue_limit,
            client_operation_limit: EXPECTED_CLIENT_OPERATION_LIMIT,
            worker_slots: default_worker_slots()?,
        })
    }
}

#[derive(Debug)]
struct ControlLatencyEvidence {
    limits: ControlLimits,
    health: LatencySeries,
    status: LatencySeries,
    cancel: LatencySeries,
}

impl ControlLatencyEvidence {
    fn report(&self) {
        println!(
            "control latency evidence: profile=portable_shared_ci platform={} arch={} classification=observed reference_host_p95_target_us={} target_enforced=false connection_limit={} client_connection_limit={} operation_queue_limit={} client_operation_limit={} worker_slots={} initial_running={} initial_queued=0 sample_policy=health_and_status_while_fully_saturated_then_concurrent_cancel",
            std::env::consts::OS,
            std::env::consts::ARCH,
            REFERENCE_CONTROL_P95_TARGET.as_micros(),
            self.limits.connection_limit,
            self.limits.client_connection_limit,
            self.limits.operation_queue_limit,
            self.limits.client_operation_limit,
            self.limits.worker_slots,
            self.limits.worker_slots,
        );
        self.health.report("health");
        self.status.report("status");
        self.cancel.report("cancel");
    }
}

#[derive(Debug)]
struct LatencySeries {
    raw_micros: Vec<u128>,
    p50_micros: u128,
    p95_micros: u128,
    p99_micros: u128,
}

impl LatencySeries {
    fn new(samples: Vec<Duration>) -> Result<Self, LifecycleError> {
        if samples.is_empty() {
            return Err(LifecycleError::MissingLatencySamples);
        }
        let raw_micros = samples
            .into_iter()
            .map(|sample| sample.as_micros())
            .collect::<Vec<_>>();
        let mut sorted = raw_micros.clone();
        sorted.sort_unstable();
        Ok(Self {
            p50_micros: nearest_rank(&sorted, 50)?,
            p95_micros: nearest_rank(&sorted, 95)?,
            p99_micros: nearest_rank(&sorted, 99)?,
            raw_micros,
        })
    }

    fn report(&self, operation: &str) {
        println!(
            "control latency samples: operation={operation} unit=us count={} p50={} p95={} p99={} raw={:?}",
            self.raw_micros.len(),
            self.p50_micros,
            self.p95_micros,
            self.p99_micros,
            self.raw_micros
        );
    }
}

fn nearest_rank(sorted: &[u128], percentile: usize) -> Result<u128, LifecycleError> {
    if sorted.is_empty() || !(1..=100).contains(&percentile) {
        return Err(LifecycleError::InvalidPercentile);
    }
    let numerator = sorted
        .len()
        .checked_mul(percentile)
        .ok_or(LifecycleError::InvalidPercentile)?;
    let rank = numerator
        .checked_add(99)
        .ok_or(LifecycleError::InvalidPercentile)?
        / 100;
    sorted
        .get(rank.saturating_sub(1))
        .copied()
        .ok_or(LifecycleError::InvalidPercentile)
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

fn wait_until_autostart_quiescent(paths: &RuntimePaths) -> Result<(), LifecycleError> {
    let mut absent_since = None;
    loop {
        match paths.discover() {
            Err(error) if runtime_absence(&error) => {
                let observed = *absent_since.get_or_insert_with(Instant::now);
                if Instant::now().saturating_duration_since(observed) >= START_TIMEOUT {
                    return Ok(());
                }
            }
            Err(error) => return Err(LifecycleError::Runtime(error)),
            Ok(survivor) => return Err(LifecycleError::AutostartSurvivor(survivor.pid())),
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

fn read_bounded_stream(mut stream: impl io::Read) -> Result<Vec<u8>, LifecycleError> {
    let mut bytes = Vec::new();
    let mut overflowed = false;
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(LifecycleError::ReadOutput)?;
        if read == 0 {
            return if overflowed {
                Err(LifecycleError::DaemonStderrTooLarge)
            } else {
                Ok(bytes)
            };
        }
        let remaining = MAX_DAEMON_STDERR_BYTES.saturating_sub(bytes.len());
        let retained = read.min(remaining);
        bytes.extend_from_slice(&chunk[..retained]);
        overflowed |= retained != read;
    }
}

fn unix_time_ms() -> Result<u64, LifecycleError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| LifecycleError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| LifecycleError::Clock)
}

fn assert_support_telemetry(
    health: &Health,
    first: &rootlight_client::SupportBundle,
    second: &rootlight_client::SupportBundle,
) -> Result<(), LifecycleError> {
    if health.protocol_version != "1.5"
        || first.schema_version != CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION
        || second.schema_version != CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION
        || first.contains_source
        || second.contains_source
    {
        return Err(LifecycleError::InvalidSupportTelemetry);
    }
    let first = first
        .telemetry
        .as_ref()
        .ok_or(LifecycleError::InvalidSupportTelemetry)?;
    let second = second
        .telemetry
        .as_ref()
        .ok_or(LifecycleError::InvalidSupportTelemetry)?;
    let support_metric = second
        .metrics
        .ipc_requests
        .iter()
        .find(|metric| metric.method == ControlMethod::SupportBundle)
        .ok_or(LifecycleError::InvalidSupportTelemetry)?;
    let first_support_count = first
        .metrics
        .ipc_requests
        .iter()
        .find(|metric| metric.method == ControlMethod::SupportBundle)
        .map_or(0, |metric| metric.succeeded_total);
    let has_support_request_span = second.traces.iter().any(|span| {
        span.kind
            == SpanKind::IpcRequest {
                method: ControlMethod::SupportBundle,
            }
            && span.outcome == TelemetryOutcome::Succeeded
    });
    let has_support_work_span = second.traces.iter().any(|span| {
        span.kind == SpanKind::SupportBundle && span.outcome == TelemetryOutcome::Succeeded
    });
    if first.schema_version != TELEMETRY_SCHEMA_VERSION
        || second.schema_version != TELEMETRY_SCHEMA_VERSION
        || support_metric.succeeded_total <= first_support_count
        || !has_support_request_span
        || !has_support_work_span
    {
        return Err(LifecycleError::InvalidSupportTelemetry);
    }
    Ok(())
}

fn validate_daemon_stderr(bytes: &[u8], private_values: &[Vec<u8>]) -> Result<(), LifecycleError> {
    if bytes.is_empty() {
        return Err(LifecycleError::EmptyDaemonStderr);
    }
    if !bytes.ends_with(b"\n") {
        return Err(LifecycleError::IncompleteDaemonStderr);
    }
    assert_privacy_sentinels_absent("daemon stderr", bytes, private_values)?;

    let mut sequences = Vec::new();
    let mut saw_starting = false;
    let mut saw_ready = false;
    let mut saw_support_completion = false;
    let mut saw_draining = false;
    let mut saw_stopped = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if line.len() > MAX_STRUCTURED_LOG_LINE_BYTES {
            return Err(LifecycleError::OversizedDaemonLogLine(line.len()));
        }
        let payload = line
            .strip_suffix(b"\n")
            .ok_or(LifecycleError::IncompleteDaemonStderr)?;
        let record: StructuredLogRecord =
            serde_json::from_slice(payload).map_err(LifecycleError::DaemonLogJson)?;
        if record.schema_version != TELEMETRY_SCHEMA_VERSION {
            return Err(LifecycleError::InvalidDaemonLogSequence);
        }
        sequences.push(record.sequence);
        match record.event {
            LogEvent::LifecycleChanged {
                lifecycle: TelemetryDaemonLifecycle::Starting,
            } => saw_starting = true,
            LogEvent::LifecycleChanged {
                lifecycle: TelemetryDaemonLifecycle::Ready,
            } => saw_ready = true,
            LogEvent::DiagnosticCompleted {
                method: ControlMethod::SupportBundle,
                outcome: TelemetryOutcome::Succeeded,
                ..
            } => saw_support_completion = true,
            LogEvent::LifecycleChanged {
                lifecycle: TelemetryDaemonLifecycle::Draining,
            } => saw_draining = true,
            LogEvent::LifecycleChanged {
                lifecycle: TelemetryDaemonLifecycle::Stopped,
            } => saw_stopped = true,
            _ => {}
        }
    }
    if !sequences_are_strictly_increasing(&sequences) {
        return Err(LifecycleError::InvalidDaemonLogSequence);
    }
    if saw_starting && saw_ready && saw_support_completion && saw_draining && saw_stopped {
        Ok(())
    } else {
        Err(LifecycleError::MissingDaemonTelemetryEvidence)
    }
}

fn sequences_are_strictly_increasing(sequences: &[u64]) -> bool {
    sequences.windows(2).all(|pair| pair[0] < pair[1])
}

fn path_privacy_encodings(path: &Path) -> Vec<Vec<u8>> {
    let raw = path.to_string_lossy().into_owned();
    let mut values = vec![raw.as_bytes().to_vec()];
    if let Ok(encoded) = serde_json::to_string(&raw) {
        let encoded = encoded.as_bytes();
        if encoded.len() >= 2 {
            values.push(encoded[1..encoded.len() - 1].to_vec());
        }
    }
    values
}

fn assert_privacy_sentinels_absent(
    surface: &'static str,
    bytes: &[u8],
    private_values: &[Vec<u8>],
) -> Result<(), LifecycleError> {
    let fixed = [
        b"PRIVATE_SOURCE_BODY".as_slice(),
        b"C:\\Users\\private\\repo".as_slice(),
        br"C:\\Users\\private\\repo".as_slice(),
        b"/home/private/repo".as_slice(),
        b"prompt injection".as_slice(),
        b"sk-secret-token".as_slice(),
        b"client-capability-value".as_slice(),
    ];
    for sentinel in fixed.iter().copied().chain(
        private_values
            .iter()
            .map(Vec::as_slice)
            .filter(|value| !value.is_empty()),
    ) {
        if bytes
            .windows(sentinel.len())
            .any(|window| window == sentinel)
        {
            return Err(LifecycleError::PrivacySentinelFound(surface));
        }
    }
    Ok(())
}

fn privacy_checked_quota_stderr(bytes: &[u8], private_values: &[Vec<u8>]) -> QuotaDaemonStderr {
    if assert_privacy_sentinels_absent("quota daemon stderr", bytes, private_values).is_err() {
        QuotaDaemonStderr::Redacted
    } else {
        QuotaDaemonStderr::Captured(String::from_utf8_lossy(bytes).into_owned())
    }
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
    if value["contract_version"].as_str() == Some("1.0")
        && value["ok"].as_bool() == Some(false)
        && value["error"]["code"].as_str() == Some(expected)
    {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_cancel_accepted(value: &Value) -> Result<(), LifecycleError> {
    let data = &value["result"]["data"];
    if data["accepted"] == true && data["operation"]["cancellation_requested"] == true {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn operation_data(value: &Value) -> &Value {
    let data = &value["result"]["data"];
    if data.get("operation").is_some() && data.get("state").is_none() {
        &data["operation"]
    } else {
        data
    }
}

fn assert_operation_state(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if operation_data(value)["state"] == expected {
        Ok(())
    } else {
        Err(LifecycleError::UnexpectedEnvelope)
    }
}

fn assert_recovery_class(value: &Value, expected: &str) -> Result<(), LifecycleError> {
    if operation_data(value)["recovery_class"] == expected {
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
    let data = operation_data(value);
    let observed_deadline = data["deadline_unix_ms"]
        .as_u64()
        .map(|value| value.to_string());
    let observed_lease = data["lease_expires_unix_ms"]
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
    let left = operation_data(left);
    let right = operation_data(right);
    if left["operation"] == right["operation"]
        && left["kind"] == right["kind"]
        && left["plan_hash"] == right["plan_hash"]
        && left["detached"] == right["detached"]
        && left["deadline_unix_ms"] == right["deadline_unix_ms"]
        && left["lease_expires_unix_ms"] == right["lease_expires_unix_ms"]
    {
        Ok(())
    } else {
        Err(LifecycleError::OperationSubmitMismatch)
    }
}

fn assert_operation_submit_equivalent(left: &Value, right: &Value) -> Result<(), LifecycleError> {
    assert_same_operation(left, right)?;
    if operation_data(left)["state"] == "succeeded"
        && matches!(
            operation_data(right)["state"].as_str(),
            Some("queued" | "running" | "succeeded")
        )
    {
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
    #[error("daemon lifecycle runtime setup failed: {0}")]
    Runtime(#[source] RuntimeError),
    #[error("daemon lifecycle IPC setup failed")]
    Ipc(#[source] rootlight_ipc::IpcError),
    #[error("daemon lifecycle client failed")]
    Client(#[source] rootlight_client::ClientError),
    #[error("failed to spawn supervised daemon")]
    SpawnDaemon(#[source] io::Error),
    #[error("supervised daemon stderr pipe is missing")]
    MissingDaemonStderr,
    #[error("failed to spawn supervised daemon stderr reader")]
    SpawnDaemonStderrReader(#[source] io::Error),
    #[error("supervised daemon stderr reader panicked")]
    DaemonStderrThreadPanicked,
    #[error("supervised daemon stderr exceeded its evidence bound")]
    DaemonStderrTooLarge,
    #[error("supervised daemon produced no structured stderr evidence")]
    EmptyDaemonStderr,
    #[error("supervised daemon stderr ended with an incomplete record")]
    IncompleteDaemonStderr,
    #[error("supervised daemon log line exceeded its bound: {0} bytes")]
    OversizedDaemonLogLine(usize),
    #[error("supervised daemon stderr was not valid structured JSON")]
    DaemonLogJson(#[source] serde_json::Error),
    #[error("supervised daemon log sequences were not strictly increasing")]
    InvalidDaemonLogSequence,
    #[error("supervised daemon telemetry evidence was incomplete")]
    MissingDaemonTelemetryEvidence,
    #[error("schema-v3 support telemetry evidence was invalid")]
    InvalidSupportTelemetry,
    #[error("privacy sentinel appeared in {0}")]
    PrivacySentinelFound(&'static str),
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
    #[error("failed to read lifecycle command output")]
    ReadOutput(#[source] io::Error),
    #[error("daemon lifecycle command timed out")]
    CommandTimedOut,
    #[error("daemon readiness timed out")]
    ReadyTimedOut,
    #[error("daemon operation did not reach a terminal state")]
    OperationTimedOut,
    #[error("daemon health did not reach the required bounded state")]
    HealthStateTimedOut,
    #[error("daemon worker saturation state changed during control sampling")]
    UnexpectedSaturationState,
    #[error(
        "daemon saturation health changed: admitted={admitted}, running={running}, queued={queued}"
    )]
    UnexpectedSaturationHealth {
        admitted: u32,
        running: u32,
        queued: u32,
    },
    #[error("daemon sampled operation state changed: {0:?}")]
    UnexpectedSampledOperationState(OperationState),
    #[error("daemon cancellation did not reach the required state")]
    UnexpectedCancellationState,
    #[error("daemon cancellation response was accepted={accepted}, state={state:?}")]
    UnexpectedCancellationObservation {
        accepted: bool,
        state: OperationState,
    },
    #[error("daemon control latency samples are missing")]
    MissingLatencySamples,
    #[error("daemon control latency percentile is invalid")]
    InvalidPercentile,
    #[error("deterministic daemon client identity is invalid")]
    InvalidClientIdentity,
    #[error("daemon default worker configuration is invalid")]
    InvalidWorkerConfiguration,
    #[error(
        "daemon default limits changed: connection={connection}, operation_queue={operation_queue}, workers={workers}"
    )]
    UnexpectedDefaultLimits {
        connection: u32,
        operation_queue: u32,
        workers: u32,
    },
    #[error("one deterministic client returned unhealthy daemon state")]
    UnexpectedClientHealth,
    #[error("concurrent client connections did not drain before quota isolation")]
    ConcurrentClientDrainTimedOut,
    #[error("daemon operation quota isolation failed")]
    OperationQuotaFailed {
        #[source]
        source: Box<LifecycleError>,
    },
    #[error("daemon operation quota isolation failed while process state was unavailable")]
    OperationQuotaProcessStateUnavailable {
        #[source]
        source: Box<LifecycleError>,
    },
    #[error("supervised daemon exited during quota isolation with {status}: {stderr}")]
    DaemonExitedDuringQuota {
        status: ExitStatus,
        stderr: QuotaDaemonStderr,
        #[source]
        source: Box<LifecycleError>,
    },
    #[error("daemon did not enforce the per-client operation quota")]
    ClientOperationQuotaNotEnforced,
    #[error("daemon quota guard did not establish the required bounded backlog")]
    QuotaGuardBacklogUnavailable,
    #[error("daemon quota isolation evidence exceeded its bounded window")]
    QuotaGuardWindowExpired,
    #[error("daemon quota exercise returned an unexpected operation state: {0:?}")]
    UnexpectedQuotaOperationState(OperationState),
    #[error("daemon lifecycle client thread panicked")]
    ClientThreadPanicked,
    #[error("simultaneous autostart did not return the exact daemon owner")]
    MissingAutostartOwner,
    #[error("simultaneous autostart returned more than one daemon owner")]
    MultipleAutostartOwners,
    #[error("daemon discovery cleanup timed out")]
    CleanupTimedOut,
    #[error("simultaneous autostart left daemon process {0} after cleanup")]
    AutostartSurvivor(u32),
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuotaDaemonStderr {
    Captured(String),
    Redacted,
    Unavailable,
}

impl std::fmt::Display for QuotaDaemonStderr {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Captured(stderr) => formatter.write_str(stderr),
            Self::Redacted => formatter.write_str("<redacted: private value detected>"),
            Self::Unavailable => formatter.write_str("<unavailable: bounded capture failed>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use rootlight_client::ClientError;
    use rootlight_ipc::IpcError;

    use super::{
        QuotaDaemonStderr, is_retryable_quota_transport, privacy_checked_quota_stderr,
        sequences_are_strictly_increasing,
    };

    #[test]
    fn telemetry_sequence_order_rejects_reordering_and_duplicates() {
        assert!(sequences_are_strictly_increasing(&[]));
        assert!(sequences_are_strictly_increasing(&[1]));
        assert!(sequences_are_strictly_increasing(&[1, 2, 3]));
        assert!(!sequences_are_strictly_increasing(&[2, 1]));
        assert!(!sequences_are_strictly_increasing(&[1, 1]));
    }

    #[test]
    fn quota_retry_accepts_only_bounded_transport_timeouts() {
        assert!(is_retryable_quota_transport(&ClientError::Ipc(
            IpcError::TimedOut
        )));
        assert!(is_retryable_quota_transport(&ClientError::Ipc(
            IpcError::ConnectTimedOut
        )));
        assert!(is_retryable_quota_transport(&ClientError::Ipc(
            IpcError::Transport(io::Error::new(io::ErrorKind::TimedOut, "fixture"))
        )));
        assert!(!is_retryable_quota_transport(&ClientError::Ipc(
            IpcError::Transport(io::Error::new(io::ErrorKind::ConnectionReset, "fixture"))
        )));
        assert!(!is_retryable_quota_transport(
            &ClientError::UnexpectedResponse
        ));
    }

    #[test]
    fn quota_diagnostic_stderr_is_privacy_checked() {
        let private_values = vec![b"/private/runtime".to_vec()];
        assert_eq!(
            privacy_checked_quota_stderr(b"safe structured diagnostic", &private_values),
            QuotaDaemonStderr::Captured("safe structured diagnostic".to_owned())
        );
        assert_eq!(
            privacy_checked_quota_stderr(b"path=/private/runtime", &private_values),
            QuotaDaemonStderr::Redacted
        );
        assert_eq!(
            privacy_checked_quota_stderr(b"token=sk-secret-token", &private_values),
            QuotaDaemonStderr::Redacted
        );
    }
}
