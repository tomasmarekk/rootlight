//! Thin synchronous client for Rootlight's private daemon control protocol.
//!
//! The client validates negotiation, request identifiers, instance nonces, and
//! stable protocol errors before exposing typed control results to applications.

#![forbid(unsafe_code)]

use std::{
    io,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, PublicValue, SafeLabel};
use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use rootlight_ipc::{
    Endpoint, FrameCodec, IpcError, connect, read_response, read_server_hello, write_client_hello,
    write_request,
};
use rootlight_protocol::{
    CURRENT_PROTOCOL_MINOR, MINIMUM_PROTOCOL_MINOR,
    generated::{common::v1 as common, daemon::v1 as daemon},
};
use rootlight_runtime::RuntimePaths;

const CLIENT_CAPABILITIES: &[&str] = &[
    "health",
    "operation.cancel",
    "operation.lease.renew",
    "operation.lifecycle.v1",
    "operation.status",
    "operation.submit",
];
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(10);
const START_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CONTROL_PROBE_PLAN_HASH: [u8; 32] = [0; 32];

/// Source-free daemon lifecycle returned by health checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonLifecycle {
    /// Startup or journal recovery is still in progress.
    Starting,
    /// The daemon is ready for control and operation requests.
    Ready,
    /// Shutdown has begun and new operations are rejected.
    Draining,
    /// A required daemon subsystem failed.
    Faulted,
    /// The in-process host has stopped.
    Stopped,
}

/// Health data returned by the local daemon.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Health {
    /// Whether startup recovery completed.
    pub ready: bool,
    /// Durable nonterminal operations.
    pub active_operations: u32,
    /// Work currently admitted to execution.
    pub admitted_operations: u32,
    /// Selected daemon protocol version.
    pub protocol_version: String,
    /// Current daemon lifecycle phase.
    pub lifecycle: DaemonLifecycle,
    /// Whether new operations are currently accepted.
    pub accepting_operations: bool,
    /// Accepted control connections currently in flight.
    pub active_connections: u32,
    /// Configured maximum simultaneous control connections.
    pub connection_limit: u32,
    /// Operations waiting for execution capacity.
    pub queued_operations: u32,
    /// Operations currently executing.
    pub running_operations: u32,
    /// Configured durable operation queue limit.
    pub operation_queue_limit: u32,
    /// Whether the durable journal remains available.
    pub journal_healthy: bool,
}

/// Client-facing operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Deterministic infrastructure work used to prove control-plane lifecycle.
    ControlProbe,
}

/// Monotonic stage within the current operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStage {
    /// The operation is durably accepted.
    Accepted,
    /// The operation owns execution capacity.
    Executing,
    /// The operation is releasing temporary resources.
    Cleanup,
}

/// Stable restart or expiry classification for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryClass {
    /// Recovery classification does not apply.
    NotApplicable,
    /// Nonterminal work was interrupted by daemon restart.
    InterruptedByRestart,
    /// The durable deadline elapsed.
    DeadlineElapsed,
    /// The attached client lease expired.
    LeaseExpired,
}

/// Durable operation status returned by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OperationStatus {
    /// Stable operation identifier.
    pub operation: OperationId,
    /// Durable lifecycle state.
    pub state: OperationState,
    /// Monotonic state or progress revision.
    pub revision: u64,
    /// Completed units.
    pub completed_units: u32,
    /// Total units, or zero while unknown.
    pub total_units: u32,
    /// Stable terminal error, when present.
    pub error: Option<PublicError>,
    /// Submitted operation kind.
    pub kind: OperationKind,
    /// Current monotonic operation stage.
    pub stage: OperationStage,
    /// Canonical operation-plan digest.
    pub plan_hash: [u8; 32],
    /// Whether the operation is detached from its submitting client.
    pub detached: bool,
    /// Whether cancellation has won the durable state race.
    pub cancellation_requested: bool,
    /// Optional wall-clock deadline used for restart classification.
    pub deadline_unix_ms: Option<u64>,
    /// Optional attached-client lease expiry.
    pub lease_expires_unix_ms: Option<u64>,
    /// Stable restart or expiry classification.
    pub recovery_class: RecoveryClass,
}

/// Client-facing operation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Accepted and queued.
    Queued,
    /// Work is running.
    Running,
    /// Cancellation is pending cleanup.
    Cancelling,
    /// Work succeeded.
    Succeeded,
    /// Work failed.
    Failed,
    /// Work was interrupted by restart or shutdown.
    Interrupted,
    /// Cooperative cancellation completed.
    Cancelled,
}

/// Policy for resolving a daemon control client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectPolicy {
    /// Connect only when a validated ready daemon already exists.
    ExistingOnly,
    /// Coordinate startup when no validated daemon is ready.
    StartIfMissing,
}

/// One negotiated daemon control client.
#[derive(Debug)]
pub struct Client {
    endpoint: Endpoint,
    instance_nonce: [u8; 16],
    client_instance_id: [u8; 16],
    codec: FrameCodec,
    next_request_id: AtomicU64,
}

impl Client {
    /// Resolves a validated daemon, optionally coordinating sibling startup.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for runtime validation, discovery, launch-lock,
    /// sibling-spawn, timeout, negotiation, or readiness failures.
    pub fn connect_or_start(
        paths: &RuntimePaths,
        client_instance_id: [u8; 16],
        policy: ConnectPolicy,
    ) -> Result<Self, ClientError> {
        match paths.client_directories_absent() {
            Ok(true) => {
                return match policy {
                    ConnectPolicy::ExistingOnly => Err(ClientError::DaemonUnavailable),
                    ConnectPolicy::StartIfMissing => {
                        paths.prepare_owner().map_err(ClientError::Runtime)?;
                        coordinate_start(paths, client_instance_id)
                    }
                };
            }
            Ok(false) => {}
            Err(rootlight_runtime::RuntimeError::OwnerSetupIncomplete)
                if policy == ConnectPolicy::StartIfMissing =>
            {
                paths.complete_owner_setup().map_err(ClientError::Runtime)?;
            }
            Err(error) => return Err(ClientError::Runtime(error)),
        }
        let probe = match probe_ready_client(paths, client_instance_id) {
            Ok(probe) => probe,
            Err(ClientError::Runtime(error))
                if policy == ConnectPolicy::StartIfMissing
                    && windows_policy_startup_retry(&error) =>
            {
                return coordinate_start(paths, client_instance_id);
            }
            Err(error) => return Err(error),
        };
        match probe {
            ProbeOutcome::Ready(client) => return Ok(client),
            ProbeOutcome::Unavailable if policy == ConnectPolicy::ExistingOnly => {
                return Err(ClientError::DaemonUnavailable);
            }
            ProbeOutcome::Unavailable => {}
        }
        coordinate_start(paths, client_instance_id)
    }

    /// Creates a client bound to one discovered daemon and authenticated client instance.
    #[must_use]
    pub fn new(endpoint: Endpoint, instance_nonce: [u8; 16], client_instance_id: [u8; 16]) -> Self {
        Self {
            endpoint,
            instance_nonce,
            client_instance_id,
            codec: FrameCodec::default(),
            next_request_id: AtomicU64::new(1),
        }
    }

    /// Checks daemon negotiation without issuing a control request.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for transport, nonce, protocol, or public daemon errors.
    pub fn negotiate(&self) -> Result<(), ClientError> {
        let mut stream = connect(&self.endpoint)?;
        write_client_hello(
            self.codec,
            &mut stream,
            &client_hello(self.instance_nonce, self.client_instance_id),
        )?;
        let hello = read_server_hello(self.codec, &mut stream)?;
        validate_server_hello(&hello, self.instance_nonce).map(|_| ())
    }

    /// Reads daemon health.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for negotiation, transport, pairing, or response errors.
    pub fn health(&self) -> Result<Health, ClientError> {
        match self.request(daemon::request_envelope::Request::Health(
            daemon::HealthRequest {},
        ))? {
            daemon::response_envelope::Response::Health(health) => Ok(Health {
                ready: health.ready,
                active_operations: health.active_operations,
                admitted_operations: health.admitted_operations,
                protocol_version: health.protocol_version,
                lifecycle: parse_daemon_lifecycle(health.lifecycle)?,
                accepting_operations: health.accepting_operations,
                active_connections: health.active_connections,
                connection_limit: health.connection_limit,
                queued_operations: health.queued_operations,
                running_operations: health.running_operations,
                operation_queue_limit: health.operation_queue_limit,
                journal_healthy: health.journal_healthy,
            }),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Submits one durable operation for bounded admission.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for a reused identifier or invalid daemon response.
    pub fn operation_submit(&self, operation: OperationId) -> Result<OperationStatus, ClientError> {
        self.operation_submit_with_timeout(operation, None)
    }

    /// Submits one durable operation with an optional execution deadline.
    ///
    /// The absolute deadline is derived once before transport so a retry with the
    /// same request remains identical at the durable journal boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for a reused identifier, invalid timeout, or invalid daemon response.
    pub fn operation_submit_with_timeout(
        &self,
        operation: OperationId,
        timeout: Option<Duration>,
    ) -> Result<OperationStatus, ClientError> {
        let deadline_unix_ms = timeout.map(operation_deadline).transpose()?;
        self.operation_submit_detached(operation, deadline_unix_ms)
    }

    /// Submits detached work with an explicit retry-stable absolute deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for a zero deadline, reused identifier, or invalid response.
    pub fn operation_submit_detached(
        &self,
        operation: OperationId,
        deadline_unix_ms: Option<u64>,
    ) -> Result<OperationStatus, ClientError> {
        let request = operation_submit_request(operation, true, deadline_unix_ms, None)?;
        self.submit_operation_request(request)
    }

    /// Submits work attached to this authenticated client lease.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for zero timing values, reused identifiers, or invalid responses.
    pub fn operation_submit_attached(
        &self,
        operation: OperationId,
        deadline_unix_ms: Option<u64>,
        lease_expires_unix_ms: u64,
    ) -> Result<OperationStatus, ClientError> {
        let request = operation_submit_request(
            operation,
            false,
            deadline_unix_ms,
            Some(lease_expires_unix_ms),
        )?;
        self.submit_operation_request(request)
    }

    /// Extends one attached operation lease owned by this authenticated client.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for a zero expiry, foreign ownership, stale renewal, or invalid response.
    pub fn operation_renew_lease(
        &self,
        operation: OperationId,
        lease_expires_unix_ms: u64,
    ) -> Result<OperationStatus, ClientError> {
        if lease_expires_unix_ms == 0 {
            return Err(ClientError::InvalidOperationLease);
        }
        match self.request(daemon::request_envelope::Request::OperationLeaseRenew(
            daemon::OperationLeaseRenewRequest {
                operation: Some(operation_to_wire(operation)),
                lease_expires_unix_ms,
            },
        ))? {
            daemon::response_envelope::Response::OperationLeaseRenew(response) => {
                parse_operation_status(response.operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn submit_operation_request(
        &self,
        request: daemon::OperationSubmitRequest,
    ) -> Result<OperationStatus, ClientError> {
        match self.request(daemon::request_envelope::Request::OperationSubmit(request))? {
            daemon::response_envelope::Response::OperationSubmit(response) => {
                parse_operation_status(response.operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reads one durable operation status.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid or error responses.
    pub fn operation_status(&self, operation: OperationId) -> Result<OperationStatus, ClientError> {
        match self.request(daemon::request_envelope::Request::OperationStatus(
            daemon::OperationStatusRequest {
                operation: Some(operation_to_wire(operation)),
            },
        ))? {
            daemon::response_envelope::Response::OperationStatus(response) => {
                parse_operation_status(response.operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Requests cooperative cancellation and returns acknowledgement plus state.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid or error responses.
    pub fn operation_cancel(
        &self,
        operation: OperationId,
    ) -> Result<(bool, OperationStatus), ClientError> {
        match self.request(daemon::request_envelope::Request::OperationCancel(
            daemon::OperationCancelRequest {
                operation: Some(operation_to_wire(operation)),
            },
        ))? {
            daemon::response_envelope::Response::OperationCancel(response) => Ok((
                response.accepted,
                parse_operation_status(response.operation)?,
            )),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn request(
        &self,
        request: daemon::request_envelope::Request,
    ) -> Result<daemon::response_envelope::Response, ClientError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        if request_id == 0 {
            return Err(ClientError::RequestIdExhausted);
        }
        let mut stream = connect(&self.endpoint)?;
        write_client_hello(
            self.codec,
            &mut stream,
            &client_hello(self.instance_nonce, self.client_instance_id),
        )?;
        let hello = read_server_hello(self.codec, &mut stream)?;
        let selected_protocol_minor = validate_server_hello(&hello, self.instance_nonce)?;
        ensure_request_supported(&request, selected_protocol_minor)?;
        write_request(
            self.codec,
            &mut stream,
            &daemon::RequestEnvelope {
                request_id,
                instance_nonce: self.instance_nonce.to_vec(),
                timeout_ms: Some(
                    u32::try_from(DEFAULT_REQUEST_TIMEOUT.as_millis())
                        .map_err(|_| ClientError::InvalidRequestTimeout)?,
                ),
                request: Some(request),
            },
        )?;
        let response = read_response(self.codec, &mut stream)?;
        if response.request_id != request_id {
            return Err(ClientError::MismatchedRequestId);
        }
        match response.response.ok_or(ClientError::MissingResponse)? {
            daemon::response_envelope::Response::Error(error) => {
                Err(ClientError::Public(Box::new(parse_public_error(error)?)))
            }
            response => Ok(response),
        }
    }
}

fn coordinate_start(
    paths: &RuntimePaths,
    client_instance_id: [u8; 16],
) -> Result<Client, ClientError> {
    let deadline = startup_deadline()?;
    loop {
        match paths.acquire_launch_lock() {
            Ok(launch) => {
                let probe = probe_ready_client(paths, client_instance_id);
                if let Ok(ProbeOutcome::Ready(client)) = probe {
                    return Ok(client);
                }
                if let Err(error) = probe
                    && !startup_probe_retryable(&error)
                {
                    return Err(error);
                }
                let child = spawn_sibling_daemon(true)?;
                drop(launch);
                return wait_for_ready_daemon(paths, client_instance_id, deadline, Some(child));
            }
            Err(rootlight_runtime::RuntimeError::LaunchBusy) => {
                if let ProbeOutcome::Ready(client) = probe_ready_client(paths, client_instance_id)?
                {
                    return Ok(client);
                }
                wait_before_deadline(deadline)?;
            }
            Err(error) => return Err(ClientError::Runtime(error)),
        }
    }
}

fn wait_for_ready_daemon(
    paths: &RuntimePaths,
    client_instance_id: [u8; 16],
    deadline: Instant,
    mut child: Option<Child>,
) -> Result<Client, ClientError> {
    loop {
        let probe = probe_ready_client(paths, client_instance_id);
        if let Ok(ProbeOutcome::Ready(client)) = probe {
            return Ok(client);
        }
        if let Err(error) = probe
            && !startup_probe_retryable(&error)
        {
            return Err(error);
        }
        let child_exited = child
            .as_mut()
            .map(|process| process.try_wait().map(|status| status.is_some()))
            .transpose()
            .map_err(ClientError::LaunchIo)?
            .unwrap_or(false);
        if child_exited {
            child = None;
        }
        if Instant::now() >= deadline {
            if let Some(mut child) = child {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(ClientError::DaemonStartTimedOut);
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }
}

#[derive(Debug)]
enum ProbeOutcome {
    Ready(Client),
    Unavailable,
}

fn probe_ready_client(
    paths: &RuntimePaths,
    client_instance_id: [u8; 16],
) -> Result<ProbeOutcome, ClientError> {
    if let Err(error) = paths.validate_client() {
        return if runtime_absence(&error) {
            Ok(ProbeOutcome::Unavailable)
        } else {
            Err(ClientError::Runtime(error))
        };
    }
    let discovery = match paths.discover() {
        Ok(discovery) => discovery,
        Err(error) if runtime_absence(&error) => return Ok(ProbeOutcome::Unavailable),
        Err(error) => return Err(ClientError::Runtime(error)),
    };
    let client = Client::new(
        discovery.endpoint(paths).map_err(ClientError::Runtime)?,
        discovery.instance_nonce(),
        client_instance_id,
    );
    let health = client.health();
    classify_health_probe(client, health)
}

fn classify_health_probe(
    client: Client,
    health: Result<Health, ClientError>,
) -> Result<ProbeOutcome, ClientError> {
    match health {
        Ok(health) if health.ready && health.lifecycle == DaemonLifecycle::Ready => {
            Ok(ProbeOutcome::Ready(client))
        }
        Ok(_) => Ok(ProbeOutcome::Unavailable),
        Err(ClientError::Ipc(error)) if ipc_unavailable(&error) => Ok(ProbeOutcome::Unavailable),
        Err(error) => Err(error),
    }
}

fn startup_deadline() -> Result<Instant, ClientError> {
    Instant::now()
        .checked_add(DEFAULT_START_TIMEOUT)
        .ok_or(ClientError::InvalidRequestTimeout)
}

fn wait_before_deadline(deadline: Instant) -> Result<(), ClientError> {
    if Instant::now() >= deadline {
        return Err(ClientError::DaemonStartTimedOut);
    }
    std::thread::sleep(START_POLL_INTERVAL);
    Ok(())
}

#[cfg(windows)]
fn windows_policy_startup_retry(error: &rootlight_runtime::RuntimeError) -> bool {
    matches!(
        error,
        rootlight_runtime::RuntimeError::WindowsSecurityPolicy
    )
}

#[cfg(not(windows))]
fn windows_policy_startup_retry(_error: &rootlight_runtime::RuntimeError) -> bool {
    false
}

fn startup_probe_retryable(error: &ClientError) -> bool {
    match error {
        ClientError::Runtime(error) => {
            runtime_absence(error) || windows_policy_startup_retry(error)
        }
        ClientError::Ipc(error) => ipc_unavailable(error),
        _ => false,
    }
}

fn runtime_absence(error: &rootlight_runtime::RuntimeError) -> bool {
    matches!(
        error,
        rootlight_runtime::RuntimeError::Io(source)
            if source.kind() == io::ErrorKind::NotFound
    )
}

fn ipc_unavailable(error: &IpcError) -> bool {
    matches!(
        error,
        IpcError::Transport(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            )
    )
}

fn spawn_sibling_daemon(coordinated: bool) -> Result<Child, ClientError> {
    let executable = std::env::current_exe().map_err(ClientError::LaunchIo)?;
    let directory = executable
        .parent()
        .ok_or(ClientError::DaemonExecutableMissing)?;
    let daemon = directory.join(sibling_daemon_name());
    if !daemon.is_file() {
        return Err(ClientError::DaemonExecutableMissing);
    }
    let mut command = Command::new(daemon);
    if coordinated {
        command.arg("--coordinated-start");
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ClientError::LaunchIo)
}

#[cfg(windows)]
fn sibling_daemon_name() -> &'static str {
    "rootlight-daemon.exe"
}

#[cfg(not(windows))]
fn sibling_daemon_name() -> &'static str {
    "rootlight-daemon"
}

fn client_hello(instance_nonce: [u8; 16], client_instance_id: [u8; 16]) -> daemon::ClientHello {
    daemon::ClientHello {
        supported_protocols: Some(common::VersionRange {
            minimum: Some(common::ContractVersion {
                major: 1,
                minor: MINIMUM_PROTOCOL_MINOR,
            }),
            maximum: Some(common::ContractVersion {
                major: 1,
                minor: CURRENT_PROTOCOL_MINOR,
            }),
        }),
        capabilities: CLIENT_CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        expected_instance_nonce: instance_nonce.to_vec(),
        client_instance_id: client_instance_id.to_vec(),
    }
}

fn validate_server_hello(
    hello: &daemon::ServerHello,
    expected_nonce: [u8; 16],
) -> Result<u32, ClientError> {
    if !nonce_matches(&hello.instance_nonce, expected_nonce) {
        return Err(ClientError::NonceMismatch);
    }
    if let Some(error) = hello.error.clone() {
        return Err(ClientError::Public(Box::new(parse_public_error(error)?)));
    }
    let selected = hello
        .selected_protocol
        .as_ref()
        .ok_or(ClientError::MissingProtocol)?;
    if selected.major != 1
        || !(MINIMUM_PROTOCOL_MINOR..=CURRENT_PROTOCOL_MINOR).contains(&selected.minor)
    {
        return Err(ClientError::ProtocolMismatch);
    }
    Ok(selected.minor)
}

fn ensure_request_supported(
    request: &daemon::request_envelope::Request,
    selected_protocol_minor: u32,
) -> Result<(), ClientError> {
    let needs_minor_two = match request {
        daemon::request_envelope::Request::OperationLeaseRenew(_) => true,
        daemon::request_envelope::Request::OperationSubmit(request) => {
            request.deadline_unix_ms.is_some()
                || request.lease_expires_unix_ms.is_some()
                || !request.detached
        }
        daemon::request_envelope::Request::Health(_)
        | daemon::request_envelope::Request::OperationStatus(_)
        | daemon::request_envelope::Request::OperationCancel(_) => false,
    };
    if needs_minor_two && selected_protocol_minor < 2 {
        Err(ClientError::ProtocolFeatureUnavailable)
    } else {
        Ok(())
    }
}

fn parse_daemon_lifecycle(value: i32) -> Result<DaemonLifecycle, ClientError> {
    match daemon::DaemonLifecycle::try_from(value)
        .map_err(|_| ClientError::InvalidDaemonLifecycle)?
    {
        daemon::DaemonLifecycle::Starting => Ok(DaemonLifecycle::Starting),
        daemon::DaemonLifecycle::Ready => Ok(DaemonLifecycle::Ready),
        daemon::DaemonLifecycle::Draining => Ok(DaemonLifecycle::Draining),
        daemon::DaemonLifecycle::Faulted => Ok(DaemonLifecycle::Faulted),
        daemon::DaemonLifecycle::Stopped => Ok(DaemonLifecycle::Stopped),
        daemon::DaemonLifecycle::Unspecified => Err(ClientError::InvalidDaemonLifecycle),
    }
}

fn parse_operation_status(
    status: Option<daemon::OperationStatus>,
) -> Result<OperationStatus, ClientError> {
    let status = status.ok_or(ClientError::MissingOperation)?;
    let state = daemon::OperationState::try_from(status.state)
        .map_err(|_| ClientError::InvalidOperationState)?;
    let state = match state {
        daemon::OperationState::Queued => OperationState::Queued,
        daemon::OperationState::Running => OperationState::Running,
        daemon::OperationState::Cancelling => OperationState::Cancelling,
        daemon::OperationState::Succeeded => OperationState::Succeeded,
        daemon::OperationState::Failed => OperationState::Failed,
        daemon::OperationState::Interrupted => OperationState::Interrupted,
        daemon::OperationState::Cancelled => OperationState::Cancelled,
        daemon::OperationState::Unspecified => return Err(ClientError::InvalidOperationState),
    };
    let kind = match daemon::OperationKind::try_from(status.kind)
        .map_err(|_| ClientError::InvalidOperationKind)?
    {
        daemon::OperationKind::ControlProbe => OperationKind::ControlProbe,
        daemon::OperationKind::Unspecified => return Err(ClientError::InvalidOperationKind),
    };
    let stage = match daemon::OperationStage::try_from(status.stage)
        .map_err(|_| ClientError::InvalidOperationStage)?
    {
        daemon::OperationStage::Accepted => OperationStage::Accepted,
        daemon::OperationStage::Executing => OperationStage::Executing,
        daemon::OperationStage::Cleanup => OperationStage::Cleanup,
        daemon::OperationStage::Unspecified => return Err(ClientError::InvalidOperationStage),
    };
    let recovery_class = match daemon::RecoveryClass::try_from(status.recovery_class)
        .map_err(|_| ClientError::InvalidRecoveryClass)?
    {
        daemon::RecoveryClass::NotApplicable => RecoveryClass::NotApplicable,
        daemon::RecoveryClass::InterruptedByRestart => RecoveryClass::InterruptedByRestart,
        daemon::RecoveryClass::DeadlineElapsed => RecoveryClass::DeadlineElapsed,
        daemon::RecoveryClass::LeaseExpired => RecoveryClass::LeaseExpired,
        daemon::RecoveryClass::Unspecified => return Err(ClientError::InvalidRecoveryClass),
    };
    let plan_hash: [u8; 32] = status
        .plan_hash
        .try_into()
        .map_err(|_| ClientError::InvalidPlanHash)?;
    Ok(OperationStatus {
        operation: parse_operation(status.operation)?,
        state,
        revision: status.revision,
        completed_units: status.completed_units,
        total_units: status.total_units,
        error: status.error.map(parse_public_error).transpose()?,
        kind,
        stage,
        plan_hash,
        detached: status.detached,
        cancellation_requested: status.cancellation_requested,
        deadline_unix_ms: status.deadline_unix_ms,
        lease_expires_unix_ms: status.lease_expires_unix_ms,
        recovery_class,
    })
}

fn parse_operation(operation: Option<common::OperationId>) -> Result<OperationId, ClientError> {
    let bytes: [u8; 16] = operation
        .ok_or(ClientError::MissingOperation)?
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(OperationId::from_bytes(bytes))
}

fn operation_to_wire(operation: OperationId) -> common::OperationId {
    common::OperationId {
        value: operation.as_bytes().to_vec(),
    }
}

fn operation_submit_request(
    operation: OperationId,
    detached: bool,
    deadline_unix_ms: Option<u64>,
    lease_expires_unix_ms: Option<u64>,
) -> Result<daemon::OperationSubmitRequest, ClientError> {
    if deadline_unix_ms == Some(0)
        || lease_expires_unix_ms == Some(0)
        || detached == lease_expires_unix_ms.is_some()
    {
        return Err(ClientError::InvalidOperationTiming);
    }
    Ok(daemon::OperationSubmitRequest {
        operation: Some(operation_to_wire(operation)),
        kind: daemon::OperationKind::ControlProbe as i32,
        plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
        detached,
        timeout_ms: None,
        deadline_unix_ms,
        lease_expires_unix_ms,
    })
}

fn operation_deadline(timeout: Duration) -> Result<u64, ClientError> {
    let milliseconds =
        u64::try_from(timeout.as_millis()).map_err(|_| ClientError::InvalidRequestTimeout)?;
    if milliseconds == 0 {
        return Err(ClientError::InvalidRequestTimeout);
    }
    unix_time_ms()?
        .checked_add(milliseconds)
        .ok_or(ClientError::InvalidRequestTimeout)
}

fn unix_time_ms() -> Result<u64, ClientError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ClientError::InvalidSystemClock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| ClientError::InvalidSystemClock)
}

fn parse_public_error(error: common::PublicError) -> Result<PublicError, ClientError> {
    let code = match common::ErrorCode::try_from(error.code)
        .map_err(|_| ClientError::InvalidPublicError)?
    {
        common::ErrorCode::InvalidArgument => ErrorCode::InvalidArgument,
        common::ErrorCode::NotFound => ErrorCode::NotFound,
        common::ErrorCode::Conflict => ErrorCode::Conflict,
        common::ErrorCode::StaleGeneration => ErrorCode::StaleGeneration,
        common::ErrorCode::UnsupportedCapability => ErrorCode::UnsupportedCapability,
        common::ErrorCode::IncompleteCoverage => ErrorCode::IncompleteCoverage,
        common::ErrorCode::BudgetExceeded => ErrorCode::BudgetExceeded,
        common::ErrorCode::ResourceExhausted => ErrorCode::ResourceExhausted,
        common::ErrorCode::Cancelled => ErrorCode::Cancelled,
        common::ErrorCode::AdapterFailed => ErrorCode::AdapterFailed,
        common::ErrorCode::IndexCorrupt => ErrorCode::IndexCorrupt,
        common::ErrorCode::MigrationRequired => ErrorCode::MigrationRequired,
        common::ErrorCode::PermissionDenied => ErrorCode::PermissionDenied,
        common::ErrorCode::ProtocolMismatch => ErrorCode::ProtocolMismatch,
        common::ErrorCode::Busy => ErrorCode::Busy,
        common::ErrorCode::Internal => ErrorCode::Internal,
        common::ErrorCode::Unspecified => return Err(ClientError::InvalidPublicError),
    };
    let mut builder = PublicError::builder_with_message(code, error.message);
    if let Some(delay) = error.retry_after_ms {
        builder = builder.retry_after(Duration::from_millis(delay));
    } else if error.retryable {
        builder = builder.retryable();
    }
    if let Some(repository) = error.repository {
        builder = builder.repository(parse_repository(repository)?);
    }
    if let Some(operation) = error.operation {
        builder = builder.operation(parse_operation(Some(operation))?);
    }
    if let Some(generation) = error.generation {
        builder = builder.generation(parse_generation(generation)?);
    }
    for (key, value) in error.details {
        builder = builder.detail(
            DetailKey::parse(&key).map_err(|_| ClientError::InvalidPublicError)?,
            parse_public_value(value)?,
        );
    }
    for action in error.next_actions {
        builder = builder.next_action(parse_next_action(action)?);
    }
    builder.build().map_err(|_| ClientError::InvalidPublicError)
}

fn parse_repository(repository: common::RepositoryId) -> Result<RepositoryId, ClientError> {
    let bytes: [u8; 16] = repository
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(RepositoryId::from_bytes(bytes))
}

fn parse_generation(generation: common::GenerationId) -> Result<GenerationId, ClientError> {
    let bytes: [u8; 20] = generation
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(GenerationId::from_bytes(bytes))
}

fn parse_public_value(value: common::PublicValue) -> Result<PublicValue, ClientError> {
    use common::public_value::Value;
    match value.value.ok_or(ClientError::InvalidPublicError)? {
        Value::Boolean(value) => Ok(PublicValue::Boolean(value)),
        Value::Integer(value) => Ok(PublicValue::Integer(value)),
        Value::Unsigned(value) => Ok(PublicValue::Unsigned(value)),
        Value::Repository(value) => Ok(PublicValue::Repository(parse_repository(value)?)),
        Value::Generation(value) => Ok(PublicValue::Generation(parse_generation(value)?)),
        Value::Operation(value) => Ok(PublicValue::Operation(parse_operation(Some(value))?)),
        Value::Label(value) => Ok(PublicValue::Label(
            SafeLabel::parse(&value).map_err(|_| ClientError::InvalidPublicError)?,
        )),
    }
}

fn parse_next_action(action: common::NextAction) -> Result<NextAction, ClientError> {
    let kind = common::next_action::Kind::try_from(action.kind)
        .map_err(|_| ClientError::InvalidPublicError)?;
    match kind {
        common::next_action::Kind::CorrectField => Ok(NextAction::CorrectField {
            field: DetailKey::parse(
                action
                    .field
                    .as_deref()
                    .ok_or(ClientError::InvalidPublicError)?,
            )
            .map_err(|_| ClientError::InvalidPublicError)?,
        }),
        common::next_action::Kind::Retry if action.field.is_none() => Ok(NextAction::Retry),
        common::next_action::Kind::SelectSupportedVersion if action.field.is_none() => {
            Ok(NextAction::SelectSupportedVersion)
        }
        common::next_action::Kind::InspectOperation if action.field.is_none() => {
            Ok(NextAction::InspectOperation)
        }
        common::next_action::Kind::RebuildRepository if action.field.is_none() => {
            Ok(NextAction::RebuildRepository)
        }
        common::next_action::Kind::CollectSupportBundle if action.field.is_none() => {
            Ok(NextAction::CollectSupportBundle)
        }
        common::next_action::Kind::Unspecified
        | common::next_action::Kind::Retry
        | common::next_action::Kind::SelectSupportedVersion
        | common::next_action::Kind::InspectOperation
        | common::next_action::Kind::RebuildRepository
        | common::next_action::Kind::CollectSupportBundle => Err(ClientError::InvalidPublicError),
    }
}

fn nonce_matches(observed: &[u8], expected: [u8; 16]) -> bool {
    observed.len() == expected.len()
        && observed
            .iter()
            .zip(expected)
            .fold(0_u8, |difference, (left, right)| {
                difference | (*left ^ right)
            })
            == 0
}

/// Local daemon client failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Local IPC failed.
    #[error("daemon transport failed")]
    Ipc(#[from] IpcError),
    /// Daemon returned a stable public error.
    #[error("daemon request failed")]
    Public(Box<PublicError>),
    /// Negotiation returned a different daemon instance.
    #[error("daemon instance nonce does not match")]
    NonceMismatch,
    /// Negotiation omitted a selected protocol.
    #[error("daemon selected protocol is missing")]
    MissingProtocol,
    /// Negotiation selected an unsupported protocol.
    #[error("daemon protocol is unsupported")]
    ProtocolMismatch,
    /// The negotiated compatible minor predates the requested operation feature.
    #[error("daemon protocol feature is unavailable")]
    ProtocolFeatureUnavailable,
    /// Response request ID did not match the request.
    #[error("daemon response request identifier does not match")]
    MismatchedRequestId,
    /// Response envelope was empty.
    #[error("daemon response is missing")]
    MissingResponse,
    /// Response kind did not match the request.
    #[error("daemon response kind is invalid")]
    UnexpectedResponse,
    /// Operation payload was missing.
    #[error("daemon operation status is missing")]
    MissingOperation,
    /// Daemon lifecycle was unspecified or unknown.
    #[error("daemon lifecycle is invalid")]
    InvalidDaemonLifecycle,
    /// Operation state was unspecified or unknown.
    #[error("daemon operation state is invalid")]
    InvalidOperationState,
    /// Operation kind was unspecified or unknown.
    #[error("daemon operation kind is invalid")]
    InvalidOperationKind,
    /// Operation stage was unspecified or unknown.
    #[error("daemon operation stage is invalid")]
    InvalidOperationStage,
    /// Recovery classification was unspecified or unknown.
    #[error("daemon operation recovery class is invalid")]
    InvalidRecoveryClass,
    /// Operation plan hash length was invalid.
    #[error("daemon operation plan hash is invalid")]
    InvalidPlanHash,
    /// Relative request timeout could not be represented.
    #[error("daemon request timeout is invalid")]
    InvalidRequestTimeout,
    /// Absolute operation deadline and lease fields were inconsistent.
    #[error("daemon operation timing is invalid")]
    InvalidOperationTiming,
    /// Attached operation lease expiry was zero.
    #[error("daemon operation lease is invalid")]
    InvalidOperationLease,
    /// The system wall clock could not provide a supported Unix timestamp.
    #[error("system clock is invalid")]
    InvalidSystemClock,
    /// Binary identifier length was invalid.
    #[error("daemon identifier is invalid")]
    InvalidIdentifier,
    /// Public error wire content violated checked bounds or invariants.
    #[error("daemon public error is invalid")]
    InvalidPublicError,
    /// Request ID counter wrapped to zero.
    #[error("daemon request identifier is exhausted")]
    RequestIdExhausted,
    /// Runtime discovery or path validation failed.
    #[error("daemon runtime discovery failed")]
    Runtime(#[source] rootlight_runtime::RuntimeError),
    /// No validated ready daemon is available.
    #[error("daemon is unavailable")]
    DaemonUnavailable,
    /// Child-process startup IO failed.
    #[error("daemon startup IO failed")]
    LaunchIo(#[source] std::io::Error),
    /// The sibling daemon executable could not be resolved.
    #[error("daemon executable is unavailable")]
    DaemonExecutableMissing,
    /// The sibling daemon terminated before becoming ready.
    #[error("daemon startup failed")]
    DaemonLaunchFailed,
    /// The sibling daemon did not become ready within the bounded deadline.
    #[error("daemon startup timed out")]
    DaemonStartTimedOut,
}

impl ClientError {
    /// Returns the daemon's stable public error, when this failure contains one.
    #[must_use]
    pub fn as_public_error(&self) -> Option<&PublicError> {
        match self {
            Self::Public(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_endpoint(label: &str) -> Endpoint {
        #[cfg(unix)]
        let path = std::env::temp_dir().join(format!("rootlight-client-{label}.sock"));
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(r"\\.\pipe\rootlight-client-{label}"));
        Endpoint::new(path).expect("test endpoint validates")
    }

    #[test]
    fn launch_lock_remains_exclusive_until_startup_authority_releases_it() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let launch = paths
            .acquire_launch_lock()
            .expect("launch lock is acquired");

        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(rootlight_runtime::RuntimeError::LaunchBusy)
        ));
        drop(launch);
        paths
            .acquire_launch_lock()
            .expect("launch lock is released after startup authority ends");
    }

    #[test]
    fn operation_submit_requests_encode_stable_timing_and_ownership() {
        let detached =
            operation_submit_request(OperationId::from_bytes([7; 16]), true, Some(100), None)
                .expect("detached submission request builds");
        assert!(detached.detached);
        assert_eq!(detached.deadline_unix_ms, Some(100));
        assert_eq!(detached.timeout_ms, None);

        let attached = operation_submit_request(
            OperationId::from_bytes([8; 16]),
            false,
            Some(200),
            Some(300),
        )
        .expect("attached submission request builds");
        assert!(!attached.detached);
        assert_eq!(attached.deadline_unix_ms, Some(200));
        assert_eq!(attached.lease_expires_unix_ms, Some(300));

        assert!(matches!(
            operation_submit_request(OperationId::from_bytes([9; 16]), false, None, None),
            Err(ClientError::InvalidOperationTiming)
        ));
    }

    #[test]
    fn operation_timeout_conversion_is_checked() {
        assert!(operation_deadline(Duration::from_millis(25)).is_ok());
        assert!(matches!(
            operation_deadline(Duration::from_nanos(1)),
            Err(ClientError::InvalidRequestTimeout)
        ));
    }

    #[test]
    fn request_features_follow_the_negotiated_minor() {
        let attached = daemon::request_envelope::Request::OperationSubmit(
            operation_submit_request(OperationId::from_bytes([6; 16]), false, None, Some(100))
                .expect("attached request builds"),
        );
        assert!(matches!(
            ensure_request_supported(&attached, 1),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        assert!(ensure_request_supported(&attached, 2).is_ok());

        let status =
            daemon::request_envelope::Request::OperationStatus(daemon::OperationStatusRequest {
                operation: Some(operation_to_wire(OperationId::from_bytes([6; 16]))),
            });
        assert!(ensure_request_supported(&status, 1).is_ok());
    }

    #[test]
    fn protocol_negotiation_rejects_the_frozen_obsolete_minor() {
        let rejected = validate_server_hello(
            &daemon::ServerHello {
                selected_protocol: Some(common::ContractVersion { major: 1, minor: 0 }),
                capabilities: Vec::new(),
                error: None,
                instance_nonce: vec![7; 16],
            },
            [7; 16],
        );

        assert!(matches!(rejected, Err(ClientError::ProtocolMismatch)));
    }

    #[test]
    fn public_error_decoder_preserves_message_and_retry_delay() {
        let parsed = parse_public_error(common::PublicError {
            code: common::ErrorCode::ProtocolMismatch as i32,
            message: "client protocol range is missing".to_owned(),
            retryable: true,
            retry_after_ms: Some(250),
            repository: None,
            operation: None,
            generation: None,
            details: Default::default(),
            next_actions: vec![common::NextAction {
                kind: common::next_action::Kind::SelectSupportedVersion as i32,
                field: None,
            }],
        })
        .expect("valid public error decodes");

        assert_eq!(parsed.message(), "client protocol range is missing");
        assert!(parsed.retryable());
        assert_eq!(parsed.retry_after_ms(), Some(250));
        assert_eq!(parsed.next_actions(), &[NextAction::SelectSupportedVersion]);
    }

    #[test]
    fn readiness_probe_preserves_protocol_and_security_failures() {
        let endpoint = test_endpoint("protocol");
        let client = Client::new(endpoint, [1; 16], [2; 16]);
        assert!(matches!(
            classify_health_probe(client, Err(ClientError::ProtocolMismatch)),
            Err(ClientError::ProtocolMismatch)
        ));
    }

    #[test]
    fn readiness_probe_only_treats_connection_absence_as_unavailable() {
        let endpoint = test_endpoint("absent");
        let client = Client::new(endpoint, [1; 16], [2; 16]);
        let unavailable = classify_health_probe(
            client,
            Err(ClientError::Ipc(IpcError::Transport(io::Error::new(
                io::ErrorKind::NotFound,
                "fixture is absent",
            )))),
        )
        .expect("absence is retryable");
        assert!(matches!(unavailable, ProbeOutcome::Unavailable));

        let endpoint = test_endpoint("nonce");
        let client = Client::new(endpoint, [1; 16], [2; 16]);
        assert!(matches!(
            classify_health_probe(client, Err(ClientError::NonceMismatch)),
            Err(ClientError::NonceMismatch)
        ));
    }

    #[test]
    fn public_error_decoder_rejects_source_shaped_message() {
        let result = parse_public_error(common::PublicError {
            code: common::ErrorCode::Internal as i32,
            message: "/home/person/secret.rs".to_owned(),
            retryable: false,
            retry_after_ms: None,
            repository: None,
            operation: None,
            generation: None,
            details: Default::default(),
            next_actions: Vec::new(),
        });

        assert!(matches!(result, Err(ClientError::InvalidPublicError)));
    }
}
