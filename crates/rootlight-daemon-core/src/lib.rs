//! Typed daemon control service shared by local IPC and standalone callers.
//!
//! This crate validates protocol inputs, maps durable operation state, enforces
//! instance binding, and keeps health/status/cancel on a control path that does
//! not depend on future CPU-heavy indexing workers.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_error::{ErrorCode, NextAction, PublicError, PublicValue};
use rootlight_ids::OperationId;
use rootlight_ipc::{
    AsyncLocalStream, FrameCodec, IpcError, LocalStream, read_client_hello,
    read_client_hello_async, read_request, read_request_async, verify_peer, write_response,
    write_response_async, write_server_hello, write_server_hello_async,
};
use rootlight_observability::{
    Architecture as ObservabilityArchitecture, DaemonLifecycle as ObservabilityDaemonLifecycle,
    DiagnosticsQuickSnapshot, ErrorCode as ObservabilityErrorCode, HealthSnapshot, OperatingSystem,
    OperationsSummary, ProtocolVersion as ObservabilityProtocolVersion, SupportBundleInput,
    build_support_bundle,
};
use rootlight_operations::{
    ClientInstanceId, DeadlineRetry, OperationError, OperationJournal, OperationKind,
    OperationRecord, OperationStage, OperationState, OperationSubmission, PlanHash, Progress,
    RecoveryClass, SubmissionOutcome,
};
use rootlight_protocol::{
    CURRENT_PROTOCOL_MINOR, MINIMUM_PROTOCOL_MINOR, PROTOCOL_VERSION,
    generated::{common::v1 as common, daemon::v1 as daemon},
};

/// Protocol major supported by the first local daemon contract.
pub const PROTOCOL_MAJOR: u32 = 1;
/// Latest protocol minor supported by the current local daemon contract.
pub const PROTOCOL_MINOR: u32 = CURRENT_PROTOCOL_MINOR;
/// Maximum capability names accepted during negotiation.
pub const MAX_CAPABILITIES: usize = 32;
/// Maximum bytes in one capability name.
pub const MAX_CAPABILITY_BYTES: usize = 64;

const CAPABILITIES: &[&str] = &[
    "diagnostics.quick",
    "health",
    "operation.cancel",
    "operation.lease.renew",
    "operation.lifecycle.v1",
    "operation.status",
    "operation.submit",
    "support.bundle.v1",
];
/// Default simultaneous negotiated connection limit.
pub const DEFAULT_CONNECTION_LIMIT: u32 = 128;
/// Default simultaneous negotiated connection limit for one validated client-declared identity.
pub const DEFAULT_CLIENT_CONNECTION_LIMIT: u32 = 8;
/// Default bounded control-command queue capacity.
pub const DEFAULT_CONTROL_QUEUE_LIMIT: usize = 64;
/// Default durable operation admission limit.
pub const DEFAULT_OPERATION_QUEUE_LIMIT: u32 = 256;
/// Default durable operation admission limit for one validated client-declared identity.
pub const DEFAULT_CLIENT_OPERATION_LIMIT: u32 = 32;
/// Default fixed synthetic operation worker count.
pub const DEFAULT_OPERATION_WORKERS: usize = 4;
/// Fixed bounded CPU work performed by one infrastructure control probe.
pub const CONTROL_PROBE_WORK: Duration = Duration::from_secs(3);
/// Default maximum server-side request response time.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Retained compatibility interval from the former polling scheduler.
pub const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_millis(100);
/// Default orderly shutdown grace period.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const CONTROL_PROBE_PLAN_HASH: [u8; 32] = [0; 32];
const LIFECYCLE_STARTING: u8 = 1;
const LIFECYCLE_READY: u8 = 2;
const LIFECYCLE_DRAINING: u8 = 3;
const LIFECYCLE_FAULTED: u8 = 4;
const LIFECYCLE_STOPPED: u8 = 5;
const HEALTH_HEALTHY: u8 = 1;
const HEALTH_DEGRADED: u8 = 2;
const HEALTH_UNAVAILABLE: u8 = 3;
const HEALTH_NOT_CONFIGURED: u8 = 4;
const HEALTH_FAILED: u8 = 5;
const RESOURCE_NORMAL: u8 = 1;
const RESOURCE_ELEVATED: u8 = 2;
const RESOURCE_HIGH: u8 = 3;
const RESOURCE_CRITICAL: u8 = 4;
const RESOURCE_UNKNOWN: u8 = 5;

/// Source-free daemon lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonLifecycle {
    /// Startup or recovery is in progress.
    Starting,
    /// The daemon is ready for requests.
    Ready,
    /// Shutdown has begun and admission is closed.
    Draining,
    /// A required subsystem failed.
    Faulted,
    /// The in-process host stopped.
    Stopped,
}

impl DaemonLifecycle {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Starting => LIFECYCLE_STARTING,
            Self::Ready => LIFECYCLE_READY,
            Self::Draining => LIFECYCLE_DRAINING,
            Self::Faulted => LIFECYCLE_FAULTED,
            Self::Stopped => LIFECYCLE_STOPPED,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            LIFECYCLE_READY => Self::Ready,
            LIFECYCLE_DRAINING => Self::Draining,
            LIFECYCLE_FAULTED => Self::Faulted,
            LIFECYCLE_STOPPED => Self::Stopped,
            _ => Self::Starting,
        }
    }
}

/// Closed status for one daemon subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// The subsystem is operating normally.
    Healthy,
    /// The subsystem is available with a known limitation.
    Degraded,
    /// The subsystem is temporarily unavailable.
    Unavailable,
    /// The subsystem does not exist in the current product slice.
    NotConfigured,
    /// The subsystem failed validation and needs repair.
    Failed,
}

impl HealthStatus {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Healthy => HEALTH_HEALTHY,
            Self::Degraded => HEALTH_DEGRADED,
            Self::Unavailable => HEALTH_UNAVAILABLE,
            Self::NotConfigured => HEALTH_NOT_CONFIGURED,
            Self::Failed => HEALTH_FAILED,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            HEALTH_HEALTHY => Self::Healthy,
            HEALTH_DEGRADED => Self::Degraded,
            HEALTH_NOT_CONFIGURED => Self::NotConfigured,
            HEALTH_FAILED => Self::Failed,
            _ => Self::Unavailable,
        }
    }
}

/// Closed bounded host resource-pressure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourcePressure {
    /// Resource use is within configured bounds.
    Normal,
    /// One or more bounded resources approach policy limits.
    Elevated,
    /// Resource pressure is sustained near a configured limit.
    High,
    /// Admission must be rejected to preserve host stability.
    Critical,
    /// No bounded sampler exists for the current slice.
    Unknown,
}

impl ResourcePressure {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Normal => RESOURCE_NORMAL,
            Self::Elevated => RESOURCE_ELEVATED,
            Self::High => RESOURCE_HIGH,
            Self::Critical => RESOURCE_CRITICAL,
            Self::Unknown => RESOURCE_UNKNOWN,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            RESOURCE_NORMAL => Self::Normal,
            RESOURCE_ELEVATED => Self::Elevated,
            RESOURCE_HIGH => Self::High,
            RESOURCE_CRITICAL => Self::Critical,
            _ => Self::Unknown,
        }
    }
}

/// Validated bounds for one daemon host instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonLimits {
    /// Maximum simultaneous negotiated connections.
    pub connection_limit: u32,
    /// Maximum simultaneous negotiated connections for one validated client-declared identity.
    pub client_connection_limit: u32,
    /// Capacity of the high-priority control lane.
    pub control_queue_limit: usize,
    /// Maximum admitted nonterminal operations.
    pub operation_queue_limit: u32,
    /// Maximum admitted nonterminal operations for one validated client-declared identity.
    pub client_operation_limit: u32,
    /// Exact number of synthetic operation workers.
    pub operation_workers: usize,
    /// Maximum response time accepted from a request envelope.
    pub request_timeout: Duration,
    /// Retained compatibility interval from the former polling scheduler.
    pub maintenance_interval: Duration,
    /// Maximum graceful drain duration.
    pub shutdown_grace: Duration,
}

impl DaemonLimits {
    /// Creates checked daemon resource bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration is zero.
    pub const fn new(
        connection_limit: u32,
        control_queue_limit: usize,
        operation_queue_limit: u32,
        operation_workers: usize,
        request_timeout: Duration,
        maintenance_interval: Duration,
        shutdown_grace: Duration,
    ) -> Result<Self, ServiceError> {
        Self::new_with_client_limits(
            connection_limit,
            connection_limit,
            control_queue_limit,
            operation_queue_limit,
            operation_queue_limit,
            operation_workers,
            request_timeout,
            maintenance_interval,
            shutdown_grace,
        )
    }

    /// Creates checked daemon resource bounds with an explicit per-client operation limit.
    ///
    /// The expanded constructor intentionally keeps all resource dimensions together so
    /// callers cannot construct a partially validated limit set.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration is zero,
    /// or when the client operation limit exceeds the global operation limit.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one validated daemon resource dimension"
    )]
    pub const fn new_with_client_operation_limit(
        connection_limit: u32,
        control_queue_limit: usize,
        operation_queue_limit: u32,
        client_operation_limit: u32,
        operation_workers: usize,
        request_timeout: Duration,
        maintenance_interval: Duration,
        shutdown_grace: Duration,
    ) -> Result<Self, ServiceError> {
        Self::new_with_client_limits(
            connection_limit,
            connection_limit,
            control_queue_limit,
            operation_queue_limit,
            client_operation_limit,
            operation_workers,
            request_timeout,
            maintenance_interval,
            shutdown_grace,
        )
    }

    /// Creates checked daemon resource bounds with explicit per-client limits.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration is zero,
    /// or when a client limit exceeds its corresponding global limit.
    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one validated daemon resource dimension"
    )]
    pub const fn new_with_client_limits(
        connection_limit: u32,
        client_connection_limit: u32,
        control_queue_limit: usize,
        operation_queue_limit: u32,
        client_operation_limit: u32,
        operation_workers: usize,
        request_timeout: Duration,
        maintenance_interval: Duration,
        shutdown_grace: Duration,
    ) -> Result<Self, ServiceError> {
        if connection_limit == 0
            || client_connection_limit == 0
            || client_connection_limit > connection_limit
            || control_queue_limit == 0
            || operation_queue_limit == 0
            || client_operation_limit == 0
            || client_operation_limit > operation_queue_limit
            || operation_workers == 0
            || request_timeout.is_zero()
            || maintenance_interval.is_zero()
            || shutdown_grace.is_zero()
        {
            return Err(ServiceError::InvalidLimits);
        }
        Ok(Self {
            connection_limit,
            client_connection_limit,
            control_queue_limit,
            operation_queue_limit,
            client_operation_limit,
            operation_workers,
            request_timeout,
            maintenance_interval,
            shutdown_grace,
        })
    }
}

impl Default for DaemonLimits {
    fn default() -> Self {
        Self {
            connection_limit: DEFAULT_CONNECTION_LIMIT,
            client_connection_limit: DEFAULT_CLIENT_CONNECTION_LIMIT,
            control_queue_limit: DEFAULT_CONTROL_QUEUE_LIMIT,
            operation_queue_limit: DEFAULT_OPERATION_QUEUE_LIMIT,
            client_operation_limit: DEFAULT_CLIENT_OPERATION_LIMIT,
            operation_workers: DEFAULT_OPERATION_WORKERS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            maintenance_interval: DEFAULT_MAINTENANCE_INTERVAL,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

/// Lock-free source-free counters shared by transport and orchestration.
#[derive(Debug)]
pub struct DaemonState {
    lifecycle: AtomicU8,
    accepting_operations: AtomicBool,
    active_connections: AtomicU32,
    admitted_operations: AtomicU32,
    queued_operations: AtomicU32,
    running_operations: AtomicU32,
    cancelling_operations: AtomicU32,
    persisting_operations: AtomicU32,
    journal_healthy: AtomicBool,
    catalog_status: AtomicU8,
    endpoint_status: AtomicU8,
    resource_pressure: AtomicU8,
}

impl DaemonState {
    /// Creates the initial starting state.
    #[must_use]
    pub fn starting() -> Self {
        Self {
            lifecycle: AtomicU8::new(DaemonLifecycle::Starting.as_u8()),
            accepting_operations: AtomicBool::new(false),
            active_connections: AtomicU32::new(0),
            admitted_operations: AtomicU32::new(0),
            queued_operations: AtomicU32::new(0),
            running_operations: AtomicU32::new(0),
            cancelling_operations: AtomicU32::new(0),
            persisting_operations: AtomicU32::new(0),
            journal_healthy: AtomicBool::new(true),
            catalog_status: AtomicU8::new(HealthStatus::Unavailable.as_u8()),
            endpoint_status: AtomicU8::new(HealthStatus::Unavailable.as_u8()),
            resource_pressure: AtomicU8::new(ResourcePressure::Unknown.as_u8()),
        }
    }

    /// Returns the current lifecycle phase.
    #[must_use]
    pub fn lifecycle(&self) -> DaemonLifecycle {
        DaemonLifecycle::from_u8(self.lifecycle.load(Ordering::Acquire))
    }

    /// Changes the lifecycle and operation admission state together.
    pub fn set_lifecycle(&self, lifecycle: DaemonLifecycle) {
        self.accepting_operations
            .store(lifecycle == DaemonLifecycle::Ready, Ordering::Release);
        self.lifecycle.store(lifecycle.as_u8(), Ordering::Release);
    }

    /// Records whether the journal remains available.
    pub fn set_journal_healthy(&self, healthy: bool) {
        self.journal_healthy.store(healthy, Ordering::Release);
        self.set_catalog_status(if healthy {
            HealthStatus::Healthy
        } else {
            HealthStatus::Failed
        });
        if !healthy {
            self.set_lifecycle(DaemonLifecycle::Faulted);
        }
    }

    /// Records the cached result of startup or explicit catalog validation.
    pub fn set_catalog_status(&self, status: HealthStatus) {
        self.catalog_status.store(status.as_u8(), Ordering::Release);
        if status == HealthStatus::Failed {
            self.set_lifecycle(DaemonLifecycle::Faulted);
        }
    }

    /// Records whether the private local endpoint has completed publication.
    pub fn set_endpoint_status(&self, status: HealthStatus) {
        self.endpoint_status
            .store(status.as_u8(), Ordering::Release);
    }

    /// Records the latest bounded host-pressure classification.
    pub fn set_resource_pressure(&self, pressure: ResourcePressure) {
        self.resource_pressure
            .store(pressure.as_u8(), Ordering::Release);
    }

    /// Sets bounded operation counters after one serialized scheduler update.
    pub fn set_operation_counts(&self, admitted: u32, queued: u32, running: u32) {
        self.admitted_operations.store(admitted, Ordering::Release);
        self.queued_operations.store(queued, Ordering::Release);
        self.running_operations.store(running, Ordering::Release);
        self.cancelling_operations.store(0, Ordering::Release);
        self.persisting_operations.store(0, Ordering::Release);
    }

    fn operation_counts(&self) -> OperationsSummary {
        OperationsSummary {
            queued: self.queued_operations.load(Ordering::Acquire),
            running: self.running_operations.load(Ordering::Acquire),
            cancelling: self.cancelling_operations.load(Ordering::Acquire),
        }
    }

    /// Returns the current active connection count.
    #[must_use]
    pub fn active_connections(&self) -> u32 {
        self.active_connections.load(Ordering::Acquire)
    }

    /// Increments the active connection count, saturating only after invariant failure.
    pub fn connection_started(&self) {
        self.active_connections.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrements the active connection count after one handler exits.
    pub fn connection_finished(&self) {
        let previous = self.active_connections.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "active connection count cannot underflow");
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::starting()
    }
}

/// Reply payload returned by the dedicated journal actor.
pub type JournalReply = Result<ControlResponse, OperationError>;

enum JournalCommand {
    Execute {
        request: ControlRequest,
        reply: tokio::sync::oneshot::Sender<JournalReply>,
    },
    Submit {
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
        reply: tokio::sync::oneshot::Sender<Result<SubmissionOutcome, OperationError>>,
    },
    RetryStatus {
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    StartOperation {
        operation: OperationId,
        started: SyncSender<WorkerStart>,
    },
    FinishOperation {
        operation: OperationId,
        cancellation_reason: Option<rootlight_operations::CancellationReason>,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    InterruptDeadline {
        operation: OperationId,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    InterruptLease {
        operation: OperationId,
        expected_expiry_unix_ms: u64,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    Interrupt {
        maximum: usize,
        reply: tokio::sync::oneshot::Sender<Result<u32, OperationError>>,
    },
    Checkpoint {
        reply: tokio::sync::oneshot::Sender<Result<(), OperationError>>,
    },
}

#[derive(Debug)]
struct JournalSenders {
    control: SyncSender<JournalCommand>,
    normal: SyncSender<JournalCommand>,
}

/// Bounded two-lane handle to one journal-owning thread.
#[derive(Debug, Clone)]
pub struct JournalActorHandle {
    senders: Arc<Mutex<Option<JournalSenders>>>,
    stopping: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy)]
enum JournalLane {
    Control,
    Normal,
}

impl JournalActorHandle {
    /// Executes health, status, or cancellation on the high-priority lane.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn control(&self, request: ControlRequest) -> Result<ControlResponse, ServiceError> {
        self.send(JournalLane::Control, JournalCommandKind::Execute(request))
            .await
    }

    /// Executes operation submission on the bounded normal lane.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn normal(&self, request: ControlRequest) -> Result<ControlResponse, ServiceError> {
        self.send(JournalLane::Normal, JournalCommandKind::Execute(request))
            .await
    }

    /// Submits immutable metadata and reports whether this call inserted it.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn submit(
        &self,
        submission: OperationSubmission,
    ) -> Result<SubmissionOutcome, ServiceError> {
        self.submit_with_deadline_retry(submission, DeadlineRetry::Exact)
            .await
    }

    async fn submit_with_deadline_retry(
        &self,
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
    ) -> Result<SubmissionOutcome, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::Submit {
                submission,
                deadline_retry,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Returns existing retry-compatible work on the high-priority lane.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, conflict, or missing-record failure.
    pub async fn retry_status(
        &self,
        submission: OperationSubmission,
    ) -> Result<OperationRecord, ServiceError> {
        self.retry_status_with_deadline_retry(submission, DeadlineRetry::Exact)
            .await
    }

    async fn retry_status_with_deadline_retry(
        &self,
        submission: OperationSubmission,
        deadline_retry: DeadlineRetry,
    ) -> Result<OperationRecord, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Control,
            JournalCommand::RetryStatus {
                submission,
                deadline_retry,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Queues durable worker authorization after a bounded worker owns the job.
    ///
    /// # Errors
    ///
    /// Returns a typed queue or actor failure.
    fn start_operation_blocking(&self, operation: OperationId) -> WorkerStart {
        let (started, receiver) = mpsc::sync_channel(1);
        let mut command = JournalCommand::StartOperation { operation, started };
        loop {
            match self.try_send_preserving(JournalLane::Normal, command) {
                Ok(()) => {
                    return receiver.recv().unwrap_or(Err(ServiceError::ChannelClosed));
                }
                Err((ServiceError::QueueFull, returned)) => {
                    command = *returned;
                    thread::yield_now();
                }
                Err((error, _)) => return Err(error),
            }
        }
    }

    /// Persists synthetic completion or cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn finish_operation(
        &self,
        operation: OperationId,
        cancellation_reason: Option<rootlight_operations::CancellationReason>,
    ) -> Result<OperationRecord, ServiceError> {
        self.finish_operation_receiver(operation, cancellation_reason)?
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    fn finish_operation_receiver(
        &self,
        operation: OperationId,
        cancellation_reason: Option<rootlight_operations::CancellationReason>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>, ServiceError>
    {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::FinishOperation {
                operation,
                cancellation_reason,
                reply,
            },
        )?;
        Ok(receiver)
    }

    fn interrupt_deadline_receiver(
        &self,
        operation: OperationId,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>, ServiceError>
    {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Control,
            JournalCommand::InterruptDeadline { operation, reply },
        )?;
        Ok(receiver)
    }

    fn interrupt_lease_receiver(
        &self,
        operation: OperationId,
        expected_expiry_unix_ms: u64,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>, ServiceError>
    {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Control,
            JournalCommand::InterruptLease {
                operation,
                expected_expiry_unix_ms,
                reply,
            },
        )?;
        Ok(receiver)
    }

    /// Interrupts one bounded batch of remaining nonterminal work.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn interrupt(&self, maximum: usize) -> Result<u32, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Control,
            JournalCommand::Interrupt { maximum, reply },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Checkpoints the journal write-ahead log.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn checkpoint(&self) -> Result<(), ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(JournalLane::Control, JournalCommand::Checkpoint { reply })?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    fn stop(&self) {
        let Ok(mut senders) = self.senders.lock() else {
            self.stopping.store(true, Ordering::Release);
            return;
        };
        self.stopping.store(true, Ordering::Release);
        senders.take();
    }

    fn try_send(&self, lane: JournalLane, command: JournalCommand) -> Result<(), ServiceError> {
        self.try_send_preserving(lane, command)
            .map_err(|(error, _)| error)
    }

    fn try_send_preserving(
        &self,
        lane: JournalLane,
        command: JournalCommand,
    ) -> Result<(), (ServiceError, Box<JournalCommand>)> {
        let senders = match self.senders.lock() {
            Ok(senders) => senders,
            Err(_) => return Err((ServiceError::ChannelClosed, Box::new(command))),
        };
        if self.stopping.load(Ordering::Acquire) {
            return Err((ServiceError::ChannelClosed, Box::new(command)));
        }
        let Some(senders) = senders.as_ref() else {
            return Err((ServiceError::ChannelClosed, Box::new(command)));
        };
        let sender = match lane {
            JournalLane::Control => &senders.control,
            JournalLane::Normal => &senders.normal,
        };
        match sender.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(command)) => Err((ServiceError::QueueFull, Box::new(command))),
            Err(TrySendError::Disconnected(command)) => {
                Err((ServiceError::ChannelClosed, Box::new(command)))
            }
        }
    }

    async fn send(
        &self,
        lane: JournalLane,
        command: JournalCommandKind,
    ) -> Result<ControlResponse, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let JournalCommandKind::Execute(request) = command;
        self.try_send(lane, JournalCommand::Execute { request, reply })?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }
}

enum JournalCommandKind {
    Execute(ControlRequest),
}

/// Owner for the journal actor thread and its bounded handle.
#[derive(Debug)]
pub struct JournalActor {
    handle: JournalActorHandle,
    join: Option<JoinHandle<()>>,
}

impl JournalActor {
    /// Starts one dedicated journal thread with bounded priority lanes.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] for a zero queue capacity or a
    /// thread-spawn failure.
    pub fn start(
        journal: Arc<OperationJournal>,
        control_capacity: usize,
        normal_capacity: usize,
    ) -> Result<Self, ServiceError> {
        if control_capacity == 0 || normal_capacity == 0 {
            return Err(ServiceError::InvalidLimits);
        }
        let (control_tx, control_rx) = mpsc::sync_channel(control_capacity);
        let (normal_tx, normal_rx) = mpsc::sync_channel(normal_capacity);
        let stopping = Arc::new(AtomicBool::new(false));
        let actor_stopping = Arc::clone(&stopping);
        let thread = thread::Builder::new()
            .name("rootlight-journal".to_owned())
            .spawn(move || journal_actor_loop(journal, control_rx, normal_rx, actor_stopping))
            .map_err(ServiceError::ThreadSpawn)?;
        Ok(Self {
            handle: JournalActorHandle {
                senders: Arc::new(Mutex::new(Some(JournalSenders {
                    control: control_tx,
                    normal: normal_tx,
                }))),
                stopping,
            },
            join: Some(thread),
        })
    }

    /// Returns the cloneable bounded actor handle.
    #[must_use]
    pub fn handle(&self) -> JournalActorHandle {
        self.handle.clone()
    }

    /// Stops and joins the journal thread.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ThreadPanicked`] when the actor panicked.
    pub fn join(mut self) -> Result<(), ServiceError> {
        self.handle.stop();
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join().map_err(|_| ServiceError::ThreadPanicked)
    }
}

impl Drop for JournalActor {
    fn drop(&mut self) {
        self.handle.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn journal_actor_loop(
    journal: Arc<OperationJournal>,
    control: Receiver<JournalCommand>,
    normal: Receiver<JournalCommand>,
    stopping: Arc<AtomicBool>,
) {
    const CONTROL_BURST: usize = 16;
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let mut handled = false;
        for _ in 0..CONTROL_BURST {
            if stopping.load(Ordering::Acquire) {
                return;
            }
            match control.try_recv() {
                Ok(command) => {
                    handled = true;
                    execute_journal_command(&journal, command);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if stopping.load(Ordering::Acquire) {
            return;
        }
        match normal.try_recv() {
            Ok(command) => {
                handled = true;
                execute_journal_command(&journal, command);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) if !handled => return,
            Err(TryRecvError::Disconnected) => {}
        }
        if handled {
            continue;
        }
        match control.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => {
                if stopping.load(Ordering::Acquire) {
                    return;
                }
                execute_journal_command(&journal, command);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stopping.load(Ordering::Acquire) {
                    return;
                }
                match normal.try_recv() {
                    Ok(command) => {
                        execute_journal_command(&journal, command);
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn start_operation(
    journal: &OperationJournal,
    operation: OperationId,
) -> Result<(OperationRecord, Option<rootlight_operations::Cancellation>), OperationError> {
    let current = journal.status(operation)?;
    if current.state.is_terminal() || current.cancellation_requested {
        return Ok((current, None));
    }
    let cancellation = journal.cancellation_token(operation)?;
    journal
        .start_execution(operation)
        .map(|started| (started, Some(cancellation)))
        .or_else(|error| match error {
            OperationError::CancellationWon
            | OperationError::ConcurrentUpdate
            | OperationError::IllegalTransition { .. } => {
                let observed = journal.status(operation)?;
                if observed.state.is_terminal() || observed.cancellation_requested {
                    Ok((observed, None))
                } else {
                    Err(error)
                }
            }
            _ => Err(error),
        })
}

fn execute_journal_command(journal: &OperationJournal, command: JournalCommand) {
    match command {
        JournalCommand::Execute { request, reply } => {
            let _ = reply.send(execute_journal_request(journal, request));
        }
        JournalCommand::Submit {
            submission,
            deadline_retry,
            reply,
        } => {
            let _ = reply.send(journal.submit_with_deadline_retry(submission, deadline_retry));
        }
        JournalCommand::RetryStatus {
            submission,
            deadline_retry,
            reply,
        } => {
            let _ =
                reply.send(journal.retry_status_with_deadline_retry(submission, deadline_retry));
        }
        JournalCommand::StartOperation { operation, started } => {
            let _ =
                started.send(start_operation(journal, operation).map_err(ServiceError::Operations));
        }
        JournalCommand::FinishOperation {
            operation,
            cancellation_reason,
            reply,
        } => {
            let current = journal.status(operation);
            let result = current.and_then(|record| {
                if matches!(
                    record.state,
                    OperationState::Interrupted | OperationState::Cancelled
                ) {
                    return Ok(record);
                }
                if let Some(reason) = cancellation_reason {
                    match record.state {
                        OperationState::Running => journal
                            .request_cancellation(operation, reason)
                            .map(|outcome| outcome.operation)
                            .and_then(|_| journal.update_stage(operation, OperationStage::Cleanup))
                            .and_then(|_| {
                                journal.transition(operation, OperationState::Cancelled, None)
                            }),
                        OperationState::Cancelling => journal
                            .update_stage(operation, OperationStage::Cleanup)
                            .or_else(|error| {
                                if matches!(error, OperationError::InvalidStage) {
                                    Ok(record.clone())
                                } else {
                                    Err(error)
                                }
                            })
                            .and_then(|_| {
                                journal.transition(operation, OperationState::Cancelled, None)
                            }),
                        _ => Err(OperationError::InvalidStage),
                    }
                } else {
                    journal
                        .update_progress(
                            operation,
                            Progress::new(1, 1).unwrap_or_else(|_| {
                                unreachable!("fixed synthetic progress is valid")
                            }),
                        )
                        .and_then(|_| {
                            journal.transition(operation, OperationState::Succeeded, None)
                        })
                        .or_else(|error| {
                            if matches!(error, OperationError::CancellationWon) {
                                journal
                                    .update_stage(operation, OperationStage::Cleanup)
                                    .and_then(|_| {
                                        journal.transition(
                                            operation,
                                            OperationState::Cancelled,
                                            None,
                                        )
                                    })
                            } else {
                                Err(error)
                            }
                        })
                }
            });
            let _ = reply.send(result);
        }
        JournalCommand::InterruptDeadline { operation, reply } => {
            let _ = reply.send(journal.interrupt_deadline(operation));
        }
        JournalCommand::InterruptLease {
            operation,
            expected_expiry_unix_ms,
            reply,
        } => {
            let _ = reply.send(journal.interrupt_lease(operation, expected_expiry_unix_ms));
        }
        JournalCommand::Interrupt { maximum, reply } => {
            let _ = reply.send(journal.interrupt_nonterminal(maximum));
        }
        JournalCommand::Checkpoint { reply } => {
            let _ = reply.send(journal.checkpoint());
        }
    }
}

fn execute_journal_request(
    journal: &OperationJournal,
    request: ControlRequest,
) -> Result<ControlResponse, OperationError> {
    match request {
        ControlRequest::Health
        | ControlRequest::DiagnosticsQuick
        | ControlRequest::SupportBundle => Ok(ControlResponse::Error(invalid_argument(
            "request is served outside the journal actor",
        ))),
        ControlRequest::OperationSubmit(_) => Ok(ControlResponse::Error(invalid_argument(
            "operation submission requires asynchronous orchestration",
        ))),
        ControlRequest::OperationStatus(operation) => journal
            .status(operation)
            .map(ControlResponse::OperationStatus),
        ControlRequest::OperationLeaseRenew { operation, .. } => {
            Ok(ControlResponse::Error(lease_renewal_unsupported(operation)))
        }
        ControlRequest::OperationCancel(operation) => {
            journal.cancel(operation).map(|(accepted, operation)| {
                ControlResponse::OperationCancel {
                    accepted,
                    operation,
                }
            })
        }
    }
}

/// Source-free health state returned through every control boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// Whether startup recovery completed and the catalog is ready.
    pub ready: bool,
    /// Durable operations that are not terminal.
    pub active_operations: u32,
    /// Operations currently admitted to future worker execution.
    pub admitted_operations: u32,
    /// Selected protocol version.
    pub protocol_version: &'static str,
    /// Current lifecycle phase.
    pub lifecycle: DaemonLifecycle,
    /// Whether new operation submissions are accepted.
    pub accepting_operations: bool,
    /// Accepted control connections currently in flight.
    pub active_connections: u32,
    /// Maximum simultaneous control connections.
    pub connection_limit: u32,
    /// Durable operations waiting for workers.
    pub queued_operations: u32,
    /// Durable operations currently executing.
    pub running_operations: u32,
    /// Maximum durable operation queue size.
    pub operation_queue_limit: u32,
    /// Whether the durable journal remains healthy.
    pub journal_healthy: bool,
    /// Cached startup or explicit catalog validation status.
    pub catalog_status: HealthStatus,
    /// Current operation catalog schema version.
    pub catalog_schema_version: u32,
    /// Generation storage status; not configured in the M04 control slice.
    pub generation_status: HealthStatus,
    /// Adapter status; not configured before parser providers exist.
    pub adapter_status: HealthStatus,
    /// Watcher status; not configured before incremental discovery exists.
    pub watcher_status: HealthStatus,
    /// Latest bounded host-pressure classification.
    pub resource_pressure: ResourcePressure,
    /// Private local endpoint ownership and publication status.
    pub endpoint_status: HealthStatus,
    /// Current discovery-record schema version.
    pub endpoint_schema_version: u32,
}

/// Closed outcome for one bounded diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticOutcome {
    /// The check passed.
    Passed,
    /// The check completed and proved a failure.
    Failed,
    /// The check exceeded its bounded request deadline.
    TimedOut,
    /// The check could not be admitted or executed.
    Unavailable,
}

/// One source-free diagnostic result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticResult {
    /// Closed check outcome.
    pub outcome: DiagnosticOutcome,
    /// Monotonic elapsed time rounded to milliseconds.
    pub duration_ms: u32,
    /// Stable source-redacted public failure, when applicable.
    pub error: Option<PublicError>,
}

/// Bounded quick-diagnostics response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsQuick {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Aggregate source-free status.
    pub overall_status: HealthStatus,
    /// Current catalog quick-check result.
    pub catalog: DiagnosticResult,
}

/// Validated bounded support archive returned by daemon and standalone modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportBundle {
    /// Support-bundle schema version.
    pub schema_version: u32,
    /// Deterministic stored ZIP bytes.
    pub archive: Vec<u8>,
    /// SHA-256 of the complete archive.
    pub sha256: [u8; 32],
    /// Encoded archive byte count.
    pub archive_bytes: u64,
    /// Always false for the default support contract.
    pub contains_source: bool,
}

/// Typed control request independent of protobuf or CLI JSON representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRequest {
    /// Read readiness and operation pressure.
    Health,
    /// Execute the bounded catalog quick check.
    DiagnosticsQuick,
    /// Build a bounded source-free support archive.
    SupportBundle,
    /// Submit one durable operation for admission.
    OperationSubmit(OperationSubmission),
    /// Read one durable operation status.
    OperationStatus(OperationId),
    /// Extend one attached operation lease for its authenticated owner.
    OperationLeaseRenew {
        /// Stable operation identifier.
        operation: OperationId,
        /// Authenticated owner from the negotiated client hello.
        owner: ClientInstanceId,
        /// New absolute lease expiry.
        expiry_unix_ms: u64,
    },
    /// Request cooperative cancellation.
    OperationCancel(OperationId),
}

/// Typed control response shared by daemon and standalone composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlResponse {
    /// Health result.
    Health(Health),
    /// Bounded quick-diagnostics result.
    DiagnosticsQuick(DiagnosticsQuick),
    /// Bounded source-free support archive.
    SupportBundle(SupportBundle),
    /// Newly queued durable operation.
    OperationSubmit(OperationRecord),
    /// Durable operation status.
    OperationStatus(OperationRecord),
    /// Durable operation after attached lease renewal.
    OperationLeaseRenew(OperationRecord),
    /// Cancellation acknowledgement and resulting state.
    OperationCancel {
        /// Whether this request first set the cancellation token.
        accepted: bool,
        /// Durable state after the request.
        operation: OperationRecord,
    },
    /// Stable public error.
    Error(PublicError),
}

/// A durable submission paired with process-local monotonic timing authority.
#[derive(Debug, Clone, Copy)]
pub struct PreparedOperationSubmission {
    submission: OperationSubmission,
    deadline: Option<tokio::time::Instant>,
    lease_deadline: Option<tokio::time::Instant>,
    deadline_retry: DeadlineRetry,
}

impl PreparedOperationSubmission {
    /// Prepares a detached control probe with an optional relative timeout.
    ///
    /// # Errors
    ///
    /// Returns [`OperationPreparationError`] for a zero or unrepresentable timeout,
    /// or when the system wall clock cannot provide durable audit metadata.
    pub fn control_probe(
        operation: OperationId,
        owner: ClientInstanceId,
        timeout: Option<Duration>,
    ) -> Result<Self, OperationPreparationError> {
        match timeout {
            Some(timeout) => {
                Self::control_probe_at(operation, owner, timeout, capture_admission_clock()?)
            }
            None => Self::new(
                OperationSubmission::new(
                    operation,
                    OperationKind::ControlProbe,
                    PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
                    owner,
                    true,
                    None,
                    None,
                )
                .map_err(|_| OperationPreparationError::InvalidTimeout)?,
                None,
                None,
            ),
        }
    }

    fn control_probe_at(
        operation: OperationId,
        owner: ClientInstanceId,
        timeout: Duration,
        clock: AdmissionClockSample,
    ) -> Result<Self, OperationPreparationError> {
        let timeout_ms = duration_millis(timeout)?;
        let deadline_unix_ms = clock
            .wall_unix_ms
            .checked_add(timeout_ms)
            .ok_or(OperationPreparationError::InvalidTimeout)?;
        let deadline = clock
            .monotonic
            .checked_add(Duration::from_millis(timeout_ms))
            .ok_or(OperationPreparationError::InvalidTimeout)?;
        Self::new_with_deadline_retry(
            OperationSubmission::new(
                operation,
                OperationKind::ControlProbe,
                PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
                owner,
                true,
                Some(deadline_unix_ms),
                None,
            )
            .map_err(|_| OperationPreparationError::InvalidTimeout)?,
            Some(deadline),
            None,
            DeadlineRetry::ReanchoredRelative { timeout_ms },
        )
    }

    /// Prepares one control probe with explicit durable deadline and lease metadata.
    ///
    /// # Errors
    ///
    /// Returns [`OperationPreparationError`] when timing metadata is invalid or the
    /// paired wall/monotonic clock sample cannot be captured.
    pub fn control_probe_timing(
        operation: OperationId,
        owner: ClientInstanceId,
        detached: bool,
        deadline_unix_ms: Option<u64>,
        lease_expires_unix_ms: Option<u64>,
    ) -> Result<Self, OperationPreparationError> {
        let submission = OperationSubmission::new(
            operation,
            OperationKind::ControlProbe,
            PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            owner,
            detached,
            deadline_unix_ms,
            lease_expires_unix_ms,
        )
        .map_err(|_| OperationPreparationError::InvalidTimeout)?;
        if deadline_unix_ms.is_none() && lease_expires_unix_ms.is_none() {
            return Self::new(submission, None, None);
        }
        let clock = capture_admission_clock()?;
        let deadline = deadline_unix_ms
            .map(|target| monotonic_target(clock, target))
            .transpose()?;
        let lease_deadline = lease_expires_unix_ms
            .map(|target| monotonic_target(clock, target))
            .transpose()?;
        Self::new(submission, deadline, lease_deadline)
    }

    #[cfg(test)]
    fn from_submission(submission: OperationSubmission) -> Result<Self, OperationPreparationError> {
        if submission.deadline_unix_ms.is_none() && submission.lease_expires_unix_ms.is_none() {
            return Self::new(submission, None, None);
        }
        let clock = capture_admission_clock()?;
        let deadline = submission
            .deadline_unix_ms
            .map(|target| monotonic_target(clock, target))
            .transpose()?;
        let lease_deadline = submission
            .lease_expires_unix_ms
            .map(|target| monotonic_target(clock, target))
            .transpose()?;
        Self::new(submission, deadline, lease_deadline)
    }

    fn new(
        submission: OperationSubmission,
        deadline: Option<tokio::time::Instant>,
        lease_deadline: Option<tokio::time::Instant>,
    ) -> Result<Self, OperationPreparationError> {
        Self::new_with_deadline_retry(submission, deadline, lease_deadline, DeadlineRetry::Exact)
    }

    fn new_with_deadline_retry(
        submission: OperationSubmission,
        deadline: Option<tokio::time::Instant>,
        lease_deadline: Option<tokio::time::Instant>,
        deadline_retry: DeadlineRetry,
    ) -> Result<Self, OperationPreparationError> {
        if submission.deadline_unix_ms.is_some() != deadline.is_some()
            || submission.lease_expires_unix_ms.is_some() != lease_deadline.is_some()
        {
            return Err(OperationPreparationError::InvalidTimeout);
        }
        Ok(Self {
            submission,
            deadline,
            lease_deadline,
            deadline_retry,
        })
    }

    /// Returns the stable operation identifier.
    #[must_use]
    pub const fn operation(self) -> OperationId {
        self.submission.operation
    }

    fn into_parts(
        self,
    ) -> (
        OperationSubmission,
        Option<tokio::time::Instant>,
        Option<tokio::time::Instant>,
        DeadlineRetry,
    ) {
        (
            self.submission,
            self.deadline,
            self.lease_deadline,
            self.deadline_retry,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct AdmissionClockSample {
    wall_unix_ms: u64,
    monotonic: tokio::time::Instant,
}

fn capture_admission_clock() -> Result<AdmissionClockSample, OperationPreparationError> {
    // Sampling monotonic time first makes a suspension between reads consume the
    // live budget instead of extending execution past its durable wall deadline.
    let monotonic = tokio::time::Instant::now();
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| OperationPreparationError::Clock)?;
    admission_clock_sample(monotonic, elapsed)
}

fn admission_clock_sample(
    monotonic: tokio::time::Instant,
    wall_elapsed: Duration,
) -> Result<AdmissionClockSample, OperationPreparationError> {
    let whole_milliseconds =
        u64::try_from(wall_elapsed.as_millis()).map_err(|_| OperationPreparationError::Clock)?;
    let partial_millisecond = u64::from(!wall_elapsed.subsec_nanos().is_multiple_of(1_000_000));
    let wall_unix_ms = whole_milliseconds
        .checked_add(partial_millisecond)
        .ok_or(OperationPreparationError::Clock)?;
    Ok(AdmissionClockSample {
        wall_unix_ms,
        monotonic,
    })
}

fn duration_millis(timeout: Duration) -> Result<u64, OperationPreparationError> {
    let milliseconds = u64::try_from(timeout.as_millis())
        .map_err(|_| OperationPreparationError::InvalidTimeout)?;
    if milliseconds == 0 {
        return Err(OperationPreparationError::InvalidTimeout);
    }
    Ok(milliseconds)
}

fn monotonic_target(
    clock: AdmissionClockSample,
    target_unix_ms: u64,
) -> Result<tokio::time::Instant, OperationPreparationError> {
    clock
        .monotonic
        .checked_add(Duration::from_millis(
            target_unix_ms.saturating_sub(clock.wall_unix_ms),
        ))
        .ok_or(OperationPreparationError::InvalidTimeout)
}

/// Failure to prepare durable and process-local operation timing.
#[derive(Debug, thiserror::Error)]
pub enum OperationPreparationError {
    /// A timeout or absolute timestamp is zero or cannot be represented.
    #[error("operation timeout is invalid")]
    InvalidTimeout,
    /// The system wall clock cannot provide a supported audit timestamp.
    #[error("system clock is invalid")]
    Clock,
}

/// Bounded host lanes serialized by the host-owned orchestrator.
#[derive(Debug, Clone)]
pub struct OrchestratorSenders {
    submissions: tokio::sync::mpsc::Sender<OperationAdmission>,
}

impl OrchestratorSenders {
    /// Creates the bounded operation-submission lane.
    #[must_use]
    pub const fn new(submissions: tokio::sync::mpsc::Sender<OperationAdmission>) -> Self {
        Self { submissions }
    }
}

#[derive(Debug)]
struct PendingAdmissionRegistry {
    next_generation: u64,
    by_operation: BTreeMap<OperationId, BTreeMap<u64, Arc<AtomicBool>>>,
}

impl Default for PendingAdmissionRegistry {
    fn default() -> Self {
        Self {
            next_generation: 1,
            by_operation: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct PendingAdmissionHandle {
    operation: OperationId,
    generation: Option<u64>,
    cancelled: Arc<AtomicBool>,
    registry: Arc<Mutex<PendingAdmissionRegistry>>,
}

impl PendingAdmissionHandle {
    #[cfg(test)]
    fn cancelled(&self) -> &AtomicBool {
        self.cancelled.as_ref()
    }

    fn handoff_to_durable(&mut self) -> Result<bool, ServiceError> {
        let registry = Arc::clone(&self.registry);
        let mut registry = registry
            .lock()
            .map_err(|_| ServiceError::AdmissionStatePoisoned)?;
        self.unregister(&mut registry);
        Ok(self.cancelled.load(Ordering::Acquire))
    }

    fn unregister(&mut self, registry: &mut PendingAdmissionRegistry) {
        let Some(generation) = self.generation.take() else {
            return;
        };
        let remove_operation =
            if let Some(generations) = registry.by_operation.get_mut(&self.operation) {
                generations.remove(&generation);
                generations.is_empty()
            } else {
                false
            };
        if remove_operation {
            registry.by_operation.remove(&self.operation);
        }
    }
}

impl Drop for PendingAdmissionHandle {
    fn drop(&mut self) {
        let registry = Arc::clone(&self.registry);
        let Ok(mut registry) = registry.lock() else {
            return;
        };
        self.unregister(&mut registry);
    }
}

/// One queued operation admission paired with its response channel.
#[derive(Debug)]
pub struct OperationAdmission {
    prepared: PreparedOperationSubmission,
    reply: tokio::sync::oneshot::Sender<Result<OperationRecord, PublicError>>,
    pending: Option<PendingAdmissionHandle>,
}

impl OperationAdmission {
    /// Creates one bounded admission and its response receiver.
    #[must_use]
    pub fn new(
        prepared: PreparedOperationSubmission,
        cancelled_before_persist: Arc<AtomicBool>,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<Result<OperationRecord, PublicError>>,
    ) {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                prepared,
                reply,
                pending: Some(PendingAdmissionHandle {
                    operation: prepared.operation(),
                    generation: None,
                    cancelled: cancelled_before_persist,
                    registry: Arc::new(Mutex::new(PendingAdmissionRegistry::default())),
                }),
            },
            receiver,
        )
    }

    fn registered(
        prepared: PreparedOperationSubmission,
        pending: PendingAdmissionHandle,
    ) -> (
        Self,
        tokio::sync::oneshot::Receiver<Result<OperationRecord, PublicError>>,
    ) {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                prepared,
                reply,
                pending: Some(pending),
            },
            receiver,
        )
    }

    /// Returns the admitted operation identifier.
    #[must_use]
    pub const fn operation(&self) -> OperationId {
        self.prepared.operation()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerPermitStage {
    Queued,
    Running,
    Cancelling,
    Persisting,
    Completed,
}

#[derive(Debug, Default)]
struct ClientOperationAdmissions {
    admitted: BTreeMap<ClientInstanceId, u32>,
}

impl ClientOperationAdmissions {
    fn reserve(&mut self, owner: ClientInstanceId, limit: u32) -> Result<(), ServiceError> {
        let admitted = self.admitted.entry(owner).or_default();
        if *admitted >= limit {
            return Err(ServiceError::ClientOperationLimit { limit });
        }
        *admitted = admitted.checked_add(1).ok_or(ServiceError::InvalidLimits)?;
        Ok(())
    }

    fn release(&mut self, owner: ClientInstanceId) {
        match self.admitted.get(&owner).copied() {
            Some(1) => {
                self.admitted.remove(&owner);
            }
            Some(admitted) if admitted > 1 => {
                self.admitted.insert(owner, admitted - 1);
            }
            Some(_) => debug_assert!(false, "client operation count cannot be zero"),
            None => debug_assert!(false, "client operation permit must have an owner bucket"),
        }
    }
}

#[derive(Debug)]
struct SchedulerPermit {
    state: Arc<DaemonState>,
    client_admissions: Arc<Mutex<ClientOperationAdmissions>>,
    owner: ClientInstanceId,
    stage: SchedulerPermitStage,
}

impl SchedulerPermit {
    fn reserve(
        state: Arc<DaemonState>,
        client_admissions: Arc<Mutex<ClientOperationAdmissions>>,
        owner: ClientInstanceId,
        global_limit: u32,
        client_limit: u32,
    ) -> Result<Self, ServiceError> {
        let mut admissions = client_admissions
            .lock()
            .map_err(|_| ServiceError::AdmissionStatePoisoned)?;
        let admitted = state.admitted_operations.load(Ordering::Acquire);
        if admitted >= global_limit {
            return Err(ServiceError::QueueFull);
        }
        admissions.reserve(owner, client_limit)?;
        state.admitted_operations.fetch_add(1, Ordering::AcqRel);
        state.queued_operations.fetch_add(1, Ordering::AcqRel);
        drop(admissions);
        Ok(Self {
            state,
            client_admissions,
            owner,
            stage: SchedulerPermitStage::Queued,
        })
    }

    fn start(&mut self) {
        if self.stage != SchedulerPermitStage::Queued {
            return;
        }
        let previous = self.state.queued_operations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "queued operation count cannot underflow");
        self.state.running_operations.fetch_add(1, Ordering::AcqRel);
        self.stage = SchedulerPermitStage::Running;
    }

    fn persist(&mut self, cancellation_cleanup: bool) {
        if self.stage != SchedulerPermitStage::Running {
            return;
        }
        let previous = self.state.running_operations.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "running operation count cannot underflow");
        if cancellation_cleanup {
            self.state
                .cancelling_operations
                .fetch_add(1, Ordering::AcqRel);
            self.stage = SchedulerPermitStage::Cancelling;
        } else {
            self.state
                .persisting_operations
                .fetch_add(1, Ordering::AcqRel);
            self.stage = SchedulerPermitStage::Persisting;
        }
    }

    fn finish(mut self) {
        self.release();
    }

    fn release(&mut self) {
        match std::mem::replace(&mut self.stage, SchedulerPermitStage::Completed) {
            SchedulerPermitStage::Queued => {
                let previous = self.state.queued_operations.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "queued operation count cannot underflow");
                let previous = self
                    .state
                    .admitted_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "admitted operation count cannot underflow");
            }
            SchedulerPermitStage::Running => {
                let previous = self.state.running_operations.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "running operation count cannot underflow");
                let previous = self
                    .state
                    .admitted_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "admitted operation count cannot underflow");
            }
            SchedulerPermitStage::Cancelling => {
                let previous = self
                    .state
                    .cancelling_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "cancelling operation count cannot underflow");
                let previous = self
                    .state
                    .admitted_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "admitted operation count cannot underflow");
            }
            SchedulerPermitStage::Persisting => {
                let previous = self
                    .state
                    .persisting_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "persisting operation count cannot underflow");
                let previous = self
                    .state
                    .admitted_operations
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "admitted operation count cannot underflow");
            }
            SchedulerPermitStage::Completed => return,
        }
        match self.client_admissions.lock() {
            Ok(mut admissions) => admissions.release(self.owner),
            Err(poisoned) => poisoned.into_inner().release(self.owner),
        }
    }
}

impl Drop for SchedulerPermit {
    fn drop(&mut self) {
        self.release();
    }
}

/// Shared fairness admission state for negotiated client-declared identities.
///
/// The OS-authorized local channel and global connection semaphore remain the hard
/// security and resource bounds. A same-user client can rotate declared identities,
/// so this ledger provides cooperative isolation and load shedding rather than an
/// unforgeable anti-Sybil principal.
#[derive(Debug)]
pub struct ClientConnectionAdmissions {
    limit: u32,
    active: Arc<Mutex<BTreeMap<ClientInstanceId, u32>>>,
}

impl ClientConnectionAdmissions {
    /// Creates a client connection governor from validated daemon limits.
    #[must_use]
    pub fn new(limits: DaemonLimits) -> Self {
        Self {
            limit: limits.client_connection_limit,
            active: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn reserve(&self, client: ClientInstanceId) -> Result<ClientConnectionPermit, ServiceError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| ServiceError::AdmissionStatePoisoned)?;
        let connections = active.entry(client).or_default();
        if *connections >= self.limit {
            return Err(ServiceError::ClientConnectionLimit { limit: self.limit });
        }
        *connections = connections
            .checked_add(1)
            .ok_or(ServiceError::InvalidLimits)?;
        drop(active);
        Ok(ClientConnectionPermit {
            active: Arc::clone(&self.active),
            client,
            released: false,
        })
    }
}

#[derive(Debug)]
struct ClientConnectionPermit {
    active: Arc<Mutex<BTreeMap<ClientInstanceId, u32>>>,
    client: ClientInstanceId,
    released: bool,
}

impl ClientConnectionPermit {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut active = match self.active.lock() {
            Ok(active) => active,
            Err(poisoned) => poisoned.into_inner(),
        };
        match active.get(&self.client).copied() {
            Some(1) => {
                active.remove(&self.client);
            }
            Some(connections) if connections > 1 => {
                active.insert(self.client, connections - 1);
            }
            Some(_) => debug_assert!(false, "client connection count cannot be zero"),
            None => debug_assert!(false, "client connection permit must have an owner bucket"),
        }
    }
}

impl Drop for ClientConnectionPermit {
    fn drop(&mut self) {
        self.release();
    }
}

type WorkerStart =
    Result<(OperationRecord, Option<rootlight_operations::Cancellation>), ServiceError>;

#[derive(Debug)]
struct WorkerJob {
    operation: OperationId,
    admitted: Receiver<()>,
    journal: JournalActorHandle,
    permit: SchedulerPermit,
    #[cfg(test)]
    started: Option<SyncSender<()>>,
}

#[derive(Debug)]
struct WorkerCompletion {
    operation: OperationId,
    start: WorkerStart,
    cancellation_reason: Option<rootlight_operations::CancellationReason>,
    permit: SchedulerPermit,
}

#[derive(Debug)]
struct PendingWorkerCompletion {
    completion: WorkerCompletion,
    reply: Option<tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>>,
}

/// Fixed bounded synthetic worker pool used by the infrastructure operation kind.
#[derive(Debug)]
pub struct SyntheticWorkerPool {
    sender: Option<SyncSender<WorkerJob>>,
    completions: tokio::sync::mpsc::Receiver<WorkerCompletion>,
    workers: Vec<JoinHandle<()>>,
}

impl SyntheticWorkerPool {
    /// Starts an exact number of workers behind a bounded queue.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] for zero bounds or a thread error.
    pub fn start(workers: usize, queue_limit: usize) -> Result<Self, ServiceError> {
        if workers == 0 || queue_limit == 0 {
            return Err(ServiceError::InvalidLimits);
        }
        let (sender, receiver) = mpsc::sync_channel(queue_limit);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let (completion_tx, completions) = tokio::sync::mpsc::channel(queue_limit);
        let mut joins = Vec::with_capacity(workers);
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let completion_tx = completion_tx.clone();
            joins.push(
                thread::Builder::new()
                    .name(format!("rootlight-worker-{index}"))
                    .spawn(move || synthetic_worker_loop(receiver, completion_tx))
                    .map_err(ServiceError::ThreadSpawn)?,
            );
        }
        drop(completion_tx);
        Ok(Self {
            sender: Some(sender),
            completions,
            workers: joins,
        })
    }

    fn submit(&self, job: WorkerJob) -> Result<(), Box<(ServiceError, WorkerJob)>> {
        let Some(sender) = &self.sender else {
            return Err(Box::new((ServiceError::ChannelClosed, job)));
        };
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job)) => Err(Box::new((ServiceError::QueueFull, job))),
            Err(TrySendError::Disconnected(job)) => {
                Err(Box::new((ServiceError::ChannelClosed, job)))
            }
        }
    }

    async fn completion(&mut self) -> Option<WorkerCompletion> {
        self.completions.recv().await
    }

    fn close(&mut self) {
        self.sender.take();
    }

    fn join(&mut self) -> Result<(), ServiceError> {
        self.close();
        for worker in self.workers.drain(..) {
            worker.join().map_err(|_| ServiceError::ThreadPanicked)?;
        }
        Ok(())
    }
}

impl Drop for SyntheticWorkerPool {
    fn drop(&mut self) {
        self.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn synthetic_worker_loop(
    receiver: Arc<std::sync::Mutex<Receiver<WorkerJob>>>,
    completion: tokio::sync::mpsc::Sender<WorkerCompletion>,
) {
    loop {
        let job = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        let Ok(mut job) = job else {
            return;
        };
        if job.admitted.recv().is_err() {
            continue;
        }
        let start = job.journal.start_operation_blocking(job.operation);
        let cancellation_reason = match &start {
            Ok((operation, Some(cancellation))) if operation.state == OperationState::Running => {
                job.permit.start();
                #[cfg(test)]
                if let Some(started) = job.started.as_ref() {
                    let _ = started.send(());
                }
                let deadline = std::time::Instant::now() + CONTROL_PROBE_WORK;
                let mut state = u64::from(job.operation.as_bytes()[0]) | 1;
                loop {
                    if let Err(cancelled) = cancellation.check() {
                        break Some(cancelled.reason());
                    }
                    for _ in 0..1_024 {
                        state = state
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        std::hint::black_box(state);
                    }
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        break None;
                    }
                    thread::sleep((deadline - now).min(Duration::from_millis(1)));
                }
            }
            Ok((_, None)) | Err(_) | Ok(_) => None,
        };
        if matches!(start, Ok((_, Some(_)))) {
            job.permit.persist(cancellation_reason.is_some());
        }
        if completion
            .blocking_send(WorkerCompletion {
                operation: job.operation,
                start,
                cancellation_reason,
                permit: job.permit,
            })
            .is_err()
        {
            return;
        }
    }
}

const TIMER_BATCH_LIMIT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TimerKind {
    Deadline,
    Lease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerId {
    operation: OperationId,
    kind: TimerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TimerReason {
    Deadline,
    Lease { expected_expiry_unix_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ScheduledTimer {
    operation: OperationId,
    reason: TimerReason,
}

impl ScheduledTimer {
    const fn id(self) -> TimerId {
        let kind = match self.reason {
            TimerReason::Deadline => TimerKind::Deadline,
            TimerReason::Lease { .. } => TimerKind::Lease,
        };
        TimerId {
            operation: self.operation,
            kind,
        }
    }
}

#[derive(Debug, Default)]
struct TimerSchedule {
    by_deadline: BTreeSet<(tokio::time::Instant, ScheduledTimer)>,
    by_timer: BTreeMap<TimerId, (tokio::time::Instant, ScheduledTimer)>,
}

impl TimerSchedule {
    fn register(
        &mut self,
        timer: ScheduledTimer,
        deadline: tokio::time::Instant,
    ) -> Result<(), ServiceError> {
        let id = timer.id();
        if self.by_timer.contains_key(&id) {
            return Err(ServiceError::TimerAlreadyRegistered);
        }
        self.by_timer.insert(id, (deadline, timer));
        self.by_deadline.insert((deadline, timer));
        Ok(())
    }

    fn remove(&mut self, id: TimerId) {
        if let Some((deadline, timer)) = self.by_timer.remove(&id) {
            self.by_deadline.remove(&(deadline, timer));
        }
    }

    fn remove_operation(&mut self, operation: OperationId) {
        self.remove(TimerId {
            operation,
            kind: TimerKind::Deadline,
        });
        self.remove(TimerId {
            operation,
            kind: TimerKind::Lease,
        });
    }

    fn next_deadline(&self) -> Option<tokio::time::Instant> {
        self.by_deadline.first().map(|(deadline, _)| *deadline)
    }

    fn due(&self, now: tokio::time::Instant, maximum: usize) -> Vec<ScheduledTimer> {
        self.by_deadline
            .iter()
            .take_while(|(deadline, _)| *deadline <= now)
            .take(maximum)
            .map(|(_, timer)| *timer)
            .collect()
    }

    fn due_for_operation(
        &self,
        operation: OperationId,
        now: tokio::time::Instant,
    ) -> Vec<ScheduledTimer> {
        self.by_deadline
            .iter()
            .filter(|(deadline, timer)| *deadline <= now && timer.operation == operation)
            .map(|(_, timer)| *timer)
            .collect()
    }
}

/// One ready orchestrator event that still requires durable processing.
#[derive(Debug)]
#[must_use]
pub struct OrchestratorEvent {
    kind: OrchestratorEventKind,
}

#[derive(Debug)]
enum OrchestratorEventKind {
    Timer,
    Completion,
}

/// Bounded daemon scheduling and monotonic-timer coordinator.
#[derive(Debug)]
pub struct DaemonOrchestrator {
    journal: JournalActorHandle,
    workers: SyntheticWorkerPool,
    pending_completion: Option<PendingWorkerCompletion>,
    timers: TimerSchedule,
    state: Arc<DaemonState>,
    client_admissions: Arc<Mutex<ClientOperationAdmissions>>,
    active_operations: BTreeMap<OperationId, OperationRecord>,
    limits: DaemonLimits,
}

impl DaemonOrchestrator {
    /// Creates the coordinator around one actor and fixed worker pool.
    ///
    /// # Errors
    ///
    /// Returns a typed worker-pool setup failure.
    pub fn new(
        journal: JournalActorHandle,
        state: Arc<DaemonState>,
        limits: DaemonLimits,
    ) -> Result<Self, ServiceError> {
        let queue_limit = usize::try_from(limits.operation_queue_limit)
            .map_err(|_| ServiceError::InvalidLimits)?;
        Ok(Self {
            journal,
            workers: SyntheticWorkerPool::start(limits.operation_workers, queue_limit)?,
            pending_completion: None,
            timers: TimerSchedule::default(),
            state,
            client_admissions: Arc::new(Mutex::new(ClientOperationAdmissions::default())),
            active_operations: BTreeMap::new(),
            limits,
        })
    }

    /// Durably admits and schedules one synthetic operation.
    ///
    /// # Errors
    ///
    /// Returns a typed admission, actor, journal, or worker-queue failure.
    pub async fn submit(
        &mut self,
        admission: OperationAdmission,
    ) -> Result<OperationRecord, ServiceError> {
        let OperationAdmission {
            prepared,
            reply,
            pending,
        } = admission;
        let result = self.schedule_submission(prepared, pending).await;
        let response = match &result {
            Ok(operation) => Ok(operation.clone()),
            Err(error) => Err(error.to_public()),
        };
        let _ = reply.send(response);
        result
    }

    /// Durably admits and schedules one synthetic operation without a response channel.
    ///
    /// Standalone composition uses this direct path so daemon and in-process execution
    /// share the same journal, admission, worker, deadline, and completion semantics.
    ///
    /// # Errors
    ///
    /// Returns a typed admission, actor, journal, or worker-queue failure.
    pub async fn schedule(
        &mut self,
        prepared: PreparedOperationSubmission,
    ) -> Result<OperationRecord, ServiceError> {
        self.schedule_submission(prepared, None).await
    }

    async fn schedule_submission(
        &mut self,
        prepared: PreparedOperationSubmission,
        mut pending: Option<PendingAdmissionHandle>,
    ) -> Result<OperationRecord, ServiceError> {
        let (submission, deadline, lease_deadline, deadline_retry) = prepared.into_parts();
        if !self.state.accepting_operations.load(Ordering::Acquire) {
            return Err(ServiceError::NotAccepting);
        }
        if self.active_operations.contains_key(&submission.operation) {
            return self
                .journal
                .retry_status_with_deadline_retry(submission, deadline_retry)
                .await;
        }
        let permit = match SchedulerPermit::reserve(
            Arc::clone(&self.state),
            Arc::clone(&self.client_admissions),
            submission.owner,
            self.limits.operation_queue_limit,
            self.limits.client_operation_limit,
        ) {
            Ok(permit) => permit,
            Err(error @ (ServiceError::QueueFull | ServiceError::ClientOperationLimit { .. })) => {
                return match self
                    .journal
                    .retry_status_with_deadline_retry(submission, deadline_retry)
                    .await
                {
                    Ok(operation) => Ok(operation),
                    Err(ServiceError::Operations(OperationError::NotFound)) => Err(error),
                    Err(retry_error) => Err(retry_error),
                };
            }
            Err(error) => return Err(error),
        };
        let (admitted_tx, admitted_rx) = mpsc::sync_channel(1);
        if let Err(failure) = self.workers.submit(WorkerJob {
            operation: submission.operation,
            admitted: admitted_rx,
            journal: self.journal.clone(),
            permit,
            #[cfg(test)]
            started: None,
        }) {
            let (error, job) = *failure;
            drop(job);
            return Err(error);
        }
        let outcome = self
            .journal
            .submit_with_deadline_retry(submission, deadline_retry)
            .await?;
        let cancelled_at_handoff = pending
            .as_mut()
            .map(PendingAdmissionHandle::handoff_to_durable)
            .transpose()?
            .unwrap_or(false);
        if cancelled_at_handoff {
            let ControlResponse::OperationCancel { operation, .. } = self
                .journal
                .control(ControlRequest::OperationCancel(outcome.operation.operation))
                .await?
            else {
                return Err(ServiceError::UnexpectedResponse);
            };
            return Ok(operation);
        }
        if !outcome.inserted {
            return Ok(outcome.operation);
        }
        let operation = outcome.operation;
        if let Some(deadline) = deadline {
            self.timers.register(
                ScheduledTimer {
                    operation: operation.operation,
                    reason: TimerReason::Deadline,
                },
                deadline,
            )?;
        }
        if let (Some(lease_deadline), Some(expected_expiry_unix_ms)) =
            (lease_deadline, operation.lease_expires_unix_ms)
        {
            self.timers.register(
                ScheduledTimer {
                    operation: operation.operation,
                    reason: TimerReason::Lease {
                        expected_expiry_unix_ms,
                    },
                },
                lease_deadline,
            )?;
        }
        if let Some(terminal) = self.expire_operation_if_due(operation.operation).await? {
            return Ok(terminal);
        }
        match operation.state {
            OperationState::Queued => {}
            OperationState::Running | OperationState::Cancelling => {
                return Err(ServiceError::UnexpectedResponse);
            }
            OperationState::Succeeded
            | OperationState::Failed
            | OperationState::Cancelled
            | OperationState::Interrupted => {
                self.timers.remove_operation(operation.operation);
                return Ok(operation);
            }
        }
        self.active_operations
            .insert(operation.operation, operation.clone());
        if admitted_tx.send(()).is_err() {
            self.timers.remove_operation(operation.operation);
            self.active_operations.remove(&operation.operation);
            return Err(ServiceError::ChannelClosed);
        }
        Ok(operation)
    }

    async fn expire_operation_if_due(
        &mut self,
        operation: OperationId,
    ) -> Result<Option<OperationRecord>, ServiceError> {
        let now = tokio::time::Instant::now();
        let due = self.timers.due_for_operation(operation, now);
        for timer in due {
            let observed = self.interrupt_timer_with_retry(timer).await?;
            if observed.state.is_terminal() || observed.cancellation_requested {
                self.timers.remove_operation(operation);
                self.active_operations.remove(&operation);
                return Ok(Some(observed));
            }
        }
        Ok(None)
    }

    async fn interrupt_timer_with_retry(
        &self,
        timer: ScheduledTimer,
    ) -> Result<OperationRecord, ServiceError> {
        loop {
            let receiver = match timer.reason {
                TimerReason::Deadline => self.journal.interrupt_deadline_receiver(timer.operation),
                TimerReason::Lease {
                    expected_expiry_unix_ms,
                } => self
                    .journal
                    .interrupt_lease_receiver(timer.operation, expected_expiry_unix_ms),
            };
            match receiver {
                Ok(receiver) => {
                    return receiver
                        .await
                        .map_err(|_| ServiceError::ChannelClosed)?
                        .map_err(ServiceError::Operations);
                }
                Err(ServiceError::QueueFull) => tokio::task::yield_now().await,
                Err(error) => return Err(error),
            }
        }
    }

    async fn expire_timers_at(&mut self, now: tokio::time::Instant) -> Result<(), ServiceError> {
        let due = self.timers.due(now, TIMER_BATCH_LIMIT);
        for timer in due {
            let observed = self.interrupt_timer_with_retry(timer).await?;
            self.timers.remove(timer.id());
            if observed.state.is_terminal() || observed.cancellation_requested {
                self.timers.remove_operation(timer.operation);
                self.active_operations.remove(&timer.operation);
            }
        }
        Ok(())
    }

    /// Reports whether no synthetic worker result is currently pending.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending_completion.is_none()
            && self.state.admitted_operations.load(Ordering::Acquire) == 0
    }

    /// Waits until a worker completion or process-local timer is ready.
    ///
    /// This method only consumes readiness. Call [`Self::process_event`] after the
    /// surrounding host `select!` chooses this branch so durable work is not dropped.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelClosed`] when no completion can arrive.
    pub async fn next_event(&mut self) -> Result<OrchestratorEvent, ServiceError> {
        if self.pending_completion.is_some() {
            return Ok(OrchestratorEvent {
                kind: OrchestratorEventKind::Completion,
            });
        }
        if let Some(deadline) = self.timers.next_deadline() {
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(deadline) => Ok(OrchestratorEvent {
                    kind: OrchestratorEventKind::Timer,
                }),
                completion = self.workers.completion() => {
                    let completion = completion.ok_or(ServiceError::ChannelClosed)?;
                    self.pending_completion = Some(PendingWorkerCompletion {
                        completion,
                        reply: None,
                    });
                    Ok(OrchestratorEvent {
                        kind: OrchestratorEventKind::Completion,
                    })
                }
            }
        } else {
            let completion = self
                .workers
                .completion()
                .await
                .ok_or(ServiceError::ChannelClosed)?;
            self.pending_completion = Some(PendingWorkerCompletion {
                completion,
                reply: None,
            });
            Ok(OrchestratorEvent {
                kind: OrchestratorEventKind::Completion,
            })
        }
    }

    /// Durably processes one event returned by [`Self::next_event`].
    ///
    /// # Errors
    ///
    /// Returns a typed actor, journal, or timer failure.
    pub async fn process_event(
        &mut self,
        event: OrchestratorEvent,
    ) -> Result<Option<OperationRecord>, ServiceError> {
        match event.kind {
            OrchestratorEventKind::Timer => {
                self.expire_timers_at(tokio::time::Instant::now()).await?;
                Ok(None)
            }
            OrchestratorEventKind::Completion => {
                let pending = self
                    .pending_completion
                    .as_ref()
                    .ok_or(ServiceError::UnexpectedResponse)?;
                match &pending.completion.start {
                    Ok((operation, Some(_))) if operation.state == OperationState::Running => {}
                    Ok((operation, None)) => {
                        let operation = operation.clone();
                        let pending = self
                            .pending_completion
                            .take()
                            .ok_or(ServiceError::UnexpectedResponse)?;
                        pending.completion.permit.finish();
                        if operation.state.is_terminal() || operation.cancellation_requested {
                            self.timers.remove_operation(operation.operation);
                            self.active_operations.remove(&operation.operation);
                        }
                        return Ok(Some(operation));
                    }
                    Ok(_) => return Err(ServiceError::UnexpectedResponse),
                    Err(_) => {
                        let pending = self
                            .pending_completion
                            .take()
                            .ok_or(ServiceError::UnexpectedResponse)?;
                        let error = match pending.completion.start {
                            Err(error) => error,
                            Ok(_) => return Err(ServiceError::UnexpectedResponse),
                        };
                        pending.completion.permit.finish();
                        return Err(error);
                    }
                }
                let operation = pending.completion.operation;
                if let Some(terminal) = self.expire_operation_if_due(operation).await? {
                    let pending = self
                        .pending_completion
                        .take()
                        .ok_or(ServiceError::UnexpectedResponse)?;
                    pending.completion.permit.finish();
                    return Ok(Some(terminal));
                }
                self.process_pending_completion().await
            }
        }
    }

    async fn process_pending_completion(
        &mut self,
    ) -> Result<Option<OperationRecord>, ServiceError> {
        let pending = self
            .pending_completion
            .as_mut()
            .ok_or(ServiceError::UnexpectedResponse)?;
        if pending.reply.is_none() {
            let completion = &pending.completion;
            pending.reply =
                Some(self.journal.finish_operation_receiver(
                    completion.operation,
                    completion.cancellation_reason,
                )?);
        }
        let result = pending
            .reply
            .as_mut()
            .ok_or(ServiceError::UnexpectedResponse)?
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations);
        match result {
            Ok(operation) => {
                let pending = self
                    .pending_completion
                    .take()
                    .ok_or(ServiceError::UnexpectedResponse)?;
                pending.completion.permit.finish();
                if operation.state.is_terminal() {
                    self.timers.remove_operation(operation.operation);
                    self.active_operations.remove(&operation.operation);
                }
                Ok(Some(operation))
            }
            Err(error) => Err(error),
        }
    }

    /// Waits for and durably processes one orchestrator event.
    ///
    /// # Errors
    ///
    /// Returns a typed actor, journal, timer, or worker-channel failure.
    pub async fn complete_next(&mut self) -> Result<Option<OperationRecord>, ServiceError> {
        let event = self.next_event().await?;
        self.process_event(event).await
    }

    /// Stops admission, interrupts remaining work, and checkpoints the journal.
    ///
    /// # Errors
    ///
    /// Returns a typed actor, journal, or worker join failure.
    pub async fn shutdown(&mut self) -> Result<(), ServiceError> {
        self.state.set_lifecycle(DaemonLifecycle::Draining);
        self.workers.close();
        loop {
            let changed = self.journal.interrupt(256).await?;
            if changed == 0 {
                break;
            }
        }
        self.workers.join()?;
        if let Some(completion) = self.pending_completion.take() {
            completion.completion.permit.finish();
        }
        while let Ok(completion) = self.workers.completions.try_recv() {
            completion.permit.finish();
        }
        self.timers = TimerSchedule::default();
        self.active_operations.clear();
        self.journal.checkpoint().await?;
        self.state.set_operation_counts(0, 0, 0);
        self.state.set_lifecycle(DaemonLifecycle::Stopped);
        Ok(())
    }
}

enum DiagnosticCommand {
    Execute {
        support_bundle: bool,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<ControlResponse>,
    },
}

#[derive(Debug)]
struct DiagnosticActorState {
    stopping: AtomicBool,
    busy: AtomicBool,
}

#[derive(Debug, Clone)]
struct DiagnosticActorHandle {
    sender: SyncSender<DiagnosticCommand>,
    state: Arc<DiagnosticActorState>,
}

impl DiagnosticActorHandle {
    fn request(
        &self,
        support_bundle: bool,
        deadline: Instant,
    ) -> Result<tokio::sync::oneshot::Receiver<ControlResponse>, ServiceError> {
        if self.state.stopping.load(Ordering::Acquire) {
            return Err(ServiceError::ChannelClosed);
        }
        if self
            .state
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ServiceError::QueueFull);
        }
        if self.state.stopping.load(Ordering::Acquire) {
            self.state.busy.store(false, Ordering::Release);
            return Err(ServiceError::ChannelClosed);
        }
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.sender
            .try_send(DiagnosticCommand::Execute {
                support_bundle,
                deadline,
                reply,
            })
            .map_err(|error| {
                self.state.busy.store(false, Ordering::Release);
                match error {
                    TrySendError::Full(_) => ServiceError::QueueFull,
                    TrySendError::Disconnected(_) => ServiceError::ChannelClosed,
                }
            })?;
        Ok(receiver)
    }

    fn stop(&self) {
        self.state.stopping.store(true, Ordering::Release);
    }
}

/// Owner for the single-flight diagnostic worker thread.
#[derive(Debug)]
pub struct DiagnosticActor {
    handle: DiagnosticActorHandle,
    join: Option<JoinHandle<()>>,
}

impl DiagnosticActor {
    /// Starts one single-flight worker around the source-free diagnostic service.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ThreadSpawn`] when the worker thread cannot start.
    pub fn start(service: ControlService) -> Result<Self, ServiceError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let state = Arc::new(DiagnosticActorState {
            stopping: AtomicBool::new(false),
            busy: AtomicBool::new(false),
        });
        let worker_state = Arc::clone(&state);
        let join = thread::Builder::new()
            .name("rootlight-diagnostics".to_owned())
            .spawn(move || diagnostic_actor_loop(service, receiver, worker_state))
            .map_err(ServiceError::ThreadSpawn)?;
        Ok(Self {
            handle: DiagnosticActorHandle { sender, state },
            join: Some(join),
        })
    }

    /// Returns the cloneable single-flight diagnostic handle.
    #[must_use]
    fn handle(&self) -> DiagnosticActorHandle {
        self.handle.clone()
    }

    /// Stops new diagnostic admission without waiting for an OS-blocked check.
    pub fn stop(&self) {
        self.handle.stop();
    }

    #[cfg(test)]
    fn join_for_test(mut self) -> Result<(), ServiceError> {
        self.stop();
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join().map_err(|_| ServiceError::ThreadPanicked)
    }
}

impl Drop for DiagnosticActor {
    fn drop(&mut self) {
        self.stop();
        let _ = self.join.take();
    }
}

fn diagnostic_actor_loop(
    service: ControlService,
    receiver: Receiver<DiagnosticCommand>,
    state: Arc<DiagnosticActorState>,
) {
    while !state.stopping.load(Ordering::Acquire) {
        let command = match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                state.busy.store(false, Ordering::Release);
                return;
            }
        };
        let DiagnosticCommand::Execute {
            support_bundle,
            deadline,
            reply,
        } = command;
        if state.stopping.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = reply.send(ControlResponse::Error(request_timed_out()));
            state.busy.store(false, Ordering::Release);
            continue;
        }
        let response = if support_bundle {
            service.support_bundle_until(deadline)
        } else {
            service.diagnostics_quick_until(deadline)
        };
        let _ = reply.send(response);
        state.busy.store(false, Ordering::Release);
    }
}

/// Shared local daemon control service.
#[derive(Debug, Clone)]
pub struct ControlService {
    journal: Arc<OperationJournal>,
    catalog_path: Option<Arc<std::path::PathBuf>>,
    instance_nonce: [u8; 16],
    state: Arc<DaemonState>,
    limits: DaemonLimits,
    diagnostic_actor: Option<DiagnosticActorHandle>,
    pending_admissions: Arc<Mutex<PendingAdmissionRegistry>>,
}

impl ControlService {
    /// Creates a ready service for one daemon instance.
    #[must_use]
    pub fn new(journal: Arc<OperationJournal>, instance_nonce: [u8; 16]) -> Self {
        let state = Arc::new(DaemonState::starting());
        state.set_catalog_status(HealthStatus::Healthy);
        state.set_endpoint_status(HealthStatus::NotConfigured);
        state.set_lifecycle(DaemonLifecycle::Ready);
        Self::with_state(journal, instance_nonce, state, DaemonLimits::default())
    }

    /// Creates a service attached to explicit host state and limits.
    #[must_use]
    pub fn with_state(
        journal: Arc<OperationJournal>,
        instance_nonce: [u8; 16],
        state: Arc<DaemonState>,
        limits: DaemonLimits,
    ) -> Self {
        Self {
            journal,
            catalog_path: None,
            instance_nonce,
            state,
            limits,
            diagnostic_actor: None,
            pending_admissions: Arc::new(Mutex::new(PendingAdmissionRegistry::default())),
        }
    }

    /// Associates a persistent catalog path with the independent diagnostic connection.
    #[must_use]
    pub fn with_catalog_path(mut self, path: std::path::PathBuf) -> Self {
        self.catalog_path = Some(Arc::new(path));
        self
    }

    /// Starts and associates the single-flight diagnostic actor used by async IPC.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ThreadSpawn`] when the worker thread cannot start.
    pub fn with_diagnostic_actor(mut self) -> Result<(Self, DiagnosticActor), ServiceError> {
        let actor = DiagnosticActor::start(self.clone())?;
        self.diagnostic_actor = Some(actor.handle());
        Ok((self, actor))
    }

    /// Returns the instance nonce used to reject stale discovery records.
    #[must_use]
    pub const fn instance_nonce(&self) -> [u8; 16] {
        self.instance_nonce
    }

    /// Returns shared host state for connection and lifecycle accounting.
    #[must_use]
    pub fn state(&self) -> Arc<DaemonState> {
        Arc::clone(&self.state)
    }

    /// Returns the validated host limits.
    #[must_use]
    pub const fn limits(&self) -> DaemonLimits {
        self.limits
    }

    fn register_pending_admission(
        &self,
        operation: OperationId,
    ) -> Result<PendingAdmissionHandle, ServiceError> {
        let mut registry = self
            .pending_admissions
            .lock()
            .map_err(|_| ServiceError::AdmissionStatePoisoned)?;
        let generation = registry.next_generation;
        registry.next_generation = registry
            .next_generation
            .checked_add(1)
            .ok_or(ServiceError::InvalidLimits)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        registry
            .by_operation
            .entry(operation)
            .or_default()
            .insert(generation, Arc::clone(&cancelled));
        drop(registry);
        Ok(PendingAdmissionHandle {
            operation,
            generation: Some(generation),
            cancelled,
            registry: Arc::clone(&self.pending_admissions),
        })
    }

    fn cancel_pending_admission(&self, operation: OperationId) -> Result<bool, ServiceError> {
        let registry = self
            .pending_admissions
            .lock()
            .map_err(|_| ServiceError::AdmissionStatePoisoned)?;
        let Some(generations) = registry.by_operation.get(&operation) else {
            return Ok(false);
        };
        for cancelled in generations.values() {
            cancelled.store(true, Ordering::Release);
        }
        Ok(true)
    }

    /// Records the current admitted work count for compatibility callers.
    pub fn set_admitted_operations(&self, admitted: u32) {
        self.state.set_operation_counts(admitted, admitted, 0);
    }

    /// Negotiates one bounded protocol and capability set.
    #[must_use]
    pub fn negotiate(&self, hello: &daemon::ClientHello) -> daemon::ServerHello {
        match validate_client_hello(hello, self.instance_nonce) {
            Ok(selected_protocol) => self.server_hello(Some(selected_protocol), None),
            Err(error) => self.server_hello(None, Some(error.as_ref())),
        }
    }

    fn server_hello(
        &self,
        selected_protocol: Option<common::ContractVersion>,
        error: Option<&PublicError>,
    ) -> daemon::ServerHello {
        daemon::ServerHello {
            selected_protocol,
            capabilities: if error.is_none() {
                let selected_minor = selected_protocol
                    .as_ref()
                    .map_or(PROTOCOL_MINOR, |version| version.minor);
                CAPABILITIES
                    .iter()
                    .filter(|value| {
                        selected_minor >= 3
                            || !matches!(**value, "diagnostics.quick" | "support.bundle.v1")
                    })
                    .map(|value| (*value).to_owned())
                    .collect()
            } else {
                Vec::new()
            },
            error: error.map(public_error_to_wire),
            instance_nonce: self.instance_nonce.to_vec(),
        }
    }

    /// Returns a source-free lock-free host health snapshot.
    #[must_use]
    pub fn health(&self) -> Health {
        let lifecycle = self.state.lifecycle();
        let admitted_operations = self.state.admitted_operations.load(Ordering::Acquire);
        let queued_operations = self.state.queued_operations.load(Ordering::Acquire);
        let running_operations = self.state.running_operations.load(Ordering::Acquire);
        let journal_healthy = self.state.journal_healthy.load(Ordering::Acquire);
        let catalog_status =
            HealthStatus::from_u8(self.state.catalog_status.load(Ordering::Acquire));
        let endpoint_status =
            HealthStatus::from_u8(self.state.endpoint_status.load(Ordering::Acquire));
        let endpoint_ready = matches!(
            endpoint_status,
            HealthStatus::Healthy | HealthStatus::NotConfigured
        );
        Health {
            ready: lifecycle == DaemonLifecycle::Ready
                && journal_healthy
                && catalog_status == HealthStatus::Healthy
                && endpoint_ready,
            active_operations: admitted_operations,
            admitted_operations,
            protocol_version: PROTOCOL_VERSION,
            lifecycle,
            accepting_operations: self.state.accepting_operations.load(Ordering::Acquire),
            active_connections: self.state.active_connections.load(Ordering::Acquire),
            connection_limit: self.limits.connection_limit,
            queued_operations,
            running_operations,
            operation_queue_limit: self.limits.operation_queue_limit,
            journal_healthy,
            catalog_status,
            catalog_schema_version: rootlight_operations::OPERATION_SCHEMA_VERSION,
            generation_status: HealthStatus::NotConfigured,
            adapter_status: HealthStatus::NotConfigured,
            watcher_status: HealthStatus::NotConfigured,
            resource_pressure: ResourcePressure::from_u8(
                self.state.resource_pressure.load(Ordering::Acquire),
            ),
            endpoint_status,
            endpoint_schema_version: 2,
        }
    }

    fn diagnostics_quick(&self, timeout: Duration) -> ControlResponse {
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return ControlResponse::Error(request_timed_out());
        };
        self.diagnostics_quick_until(deadline)
    }

    fn diagnostics_quick_until(&self, deadline: Instant) -> ControlResponse {
        let started = Instant::now();
        if started >= deadline {
            return ControlResponse::Error(request_timed_out());
        }
        let checked = self.catalog_path.as_deref().map_or_else(
            || self.journal.quick_check(),
            |path| OperationJournal::quick_check_path_until(path, deadline),
        );
        let duration_ms = duration_ms(started.elapsed());
        if Instant::now() >= deadline {
            return ControlResponse::Error(request_timed_out());
        }
        match checked {
            Ok(()) => {
                self.update_catalog_status_from_diagnostic(HealthStatus::Healthy);
                ControlResponse::DiagnosticsQuick(DiagnosticsQuick {
                    schema_version: 1,
                    overall_status: HealthStatus::Healthy,
                    catalog: DiagnosticResult {
                        outcome: DiagnosticOutcome::Passed,
                        duration_ms,
                        error: None,
                    },
                })
            }
            Err(error) => {
                let status = diagnostic_health_status(&error);
                if diagnostic_result_is_conclusive(&error) {
                    self.update_catalog_status_from_diagnostic(status);
                }
                ControlResponse::DiagnosticsQuick(DiagnosticsQuick {
                    schema_version: 1,
                    overall_status: status,
                    catalog: DiagnosticResult {
                        outcome: diagnostic_outcome_for_error(&error),
                        duration_ms,
                        error: Some(operation_error_to_public(&error, None)),
                    },
                })
            }
        }
    }

    fn update_catalog_status_from_diagnostic(&self, status: HealthStatus) {
        if !matches!(
            self.state.lifecycle(),
            DaemonLifecycle::Draining | DaemonLifecycle::Stopped
        ) {
            self.state.set_catalog_status(status);
        }
    }

    fn diagnostics_quick_path(&self, path: &std::path::Path, timeout: Duration) -> ControlResponse {
        let started = Instant::now();
        let checked = OperationJournal::quick_check_path_with_timeout(path, timeout);
        let duration_ms = duration_ms(started.elapsed());
        match checked {
            Ok(()) => {
                self.update_catalog_status_from_diagnostic(HealthStatus::Healthy);
                ControlResponse::DiagnosticsQuick(DiagnosticsQuick {
                    schema_version: 1,
                    overall_status: HealthStatus::Healthy,
                    catalog: DiagnosticResult {
                        outcome: DiagnosticOutcome::Passed,
                        duration_ms,
                        error: None,
                    },
                })
            }
            Err(error) => {
                let status = diagnostic_health_status(&error);
                if diagnostic_result_is_conclusive(&error) {
                    self.update_catalog_status_from_diagnostic(status);
                }
                ControlResponse::DiagnosticsQuick(DiagnosticsQuick {
                    schema_version: 1,
                    overall_status: status,
                    catalog: DiagnosticResult {
                        outcome: diagnostic_outcome_for_error(&error),
                        duration_ms,
                        error: Some(operation_error_to_public(&error, None)),
                    },
                })
            }
        }
    }

    fn support_bundle(&self, timeout: Duration) -> ControlResponse {
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return ControlResponse::Error(request_timed_out());
        };
        self.support_bundle_until(deadline)
    }

    fn support_bundle_until(&self, deadline: Instant) -> ControlResponse {
        let diagnostics = match self.diagnostics_quick_until(deadline) {
            ControlResponse::DiagnosticsQuick(diagnostics) => diagnostics,
            ControlResponse::Error(error) => return ControlResponse::Error(error),
            _ => unreachable!("diagnostics helper returns diagnostics or error"),
        };
        if Instant::now() >= deadline {
            return ControlResponse::Error(request_timed_out());
        }
        let health = self.health();
        let input = SupportBundleInput {
            protocol_version: ObservabilityProtocolVersion::V1_3,
            operating_system: observability_operating_system(),
            architecture: observability_architecture(),
            health: health_snapshot(&health),
            diagnostics: diagnostics_snapshot(&diagnostics),
            operations: self.state.operation_counts(),
        };
        match build_support_bundle(&input) {
            Ok(bundle) if Instant::now() < deadline => {
                ControlResponse::SupportBundle(SupportBundle {
                    schema_version: rootlight_observability::SUPPORT_BUNDLE_SCHEMA_VERSION,
                    archive: bundle.archive().to_vec(),
                    sha256: bundle.sha256(),
                    archive_bytes: bundle.archive_bytes(),
                    contains_source: bundle.contains_source(),
                })
            }
            Ok(_) => ControlResponse::Error(request_timed_out()),
            Err(_) => ControlResponse::Error(internal_error()),
        }
    }

    /// Executes the quick check through a separate read-only catalog connection.
    #[must_use]
    pub fn execute_diagnostics_path(&self, path: &std::path::Path) -> ControlResponse {
        self.diagnostics_quick_path(path, self.limits.request_timeout)
    }

    /// Executes one typed control request.
    #[must_use]
    pub fn execute(&self, request: ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::Health => ControlResponse::Health(self.health()),
            ControlRequest::DiagnosticsQuick => self.diagnostics_quick(self.limits.request_timeout),
            ControlRequest::SupportBundle => self.support_bundle(self.limits.request_timeout),
            ControlRequest::OperationSubmit(submission)
                if !self.state.accepting_operations.load(Ordering::Acquire) =>
            {
                ControlResponse::Error(daemon_not_accepting(submission.operation))
            }
            ControlRequest::OperationSubmit(_) => ControlResponse::Error(invalid_argument(
                "operation submission requires asynchronous orchestration",
            )),
            ControlRequest::OperationStatus(operation) => match self.journal.status(operation) {
                Ok(record) => ControlResponse::OperationStatus(record),
                Err(error) => {
                    ControlResponse::Error(operation_error_to_public(&error, Some(operation)))
                }
            },
            ControlRequest::OperationLeaseRenew { operation, .. } => {
                ControlResponse::Error(lease_renewal_unsupported(operation))
            }
            ControlRequest::OperationCancel(operation) => match self.journal.cancel(operation) {
                Ok((accepted, operation)) => ControlResponse::OperationCancel {
                    accepted,
                    operation,
                },
                Err(error) => {
                    ControlResponse::Error(operation_error_to_public(&error, Some(operation)))
                }
            },
        }
    }

    /// Validates and executes one protobuf request envelope.
    #[must_use]
    pub fn dispatch(&self, envelope: daemon::RequestEnvelope) -> daemon::ResponseEnvelope {
        self.dispatch_for_client(envelope, ClientInstanceId::SYSTEM, PROTOCOL_MINOR)
    }

    fn dispatch_for_client(
        &self,
        envelope: daemon::RequestEnvelope,
        client_instance_id: ClientInstanceId,
        selected_protocol_minor: u32,
    ) -> daemon::ResponseEnvelope {
        let request_id = envelope.request_id;
        let response = if envelope.timeout_ms == Some(0) {
            daemon::response_envelope::Response::Error(public_error_to_wire(&invalid_argument(
                "daemon request timeout is invalid",
            )))
        } else if !nonce_matches(&envelope.instance_nonce, self.instance_nonce) {
            daemon::response_envelope::Response::Error(public_error_to_wire(&permission_denied(
                "daemon instance nonce does not match",
            )))
        } else {
            match request_from_wire(
                envelope.request,
                client_instance_id,
                selected_protocol_minor,
            ) {
                Ok(DecodedRequest::Control(request)) => response_to_wire(self.execute(request)),
                Ok(DecodedRequest::Submission(_)) => daemon::response_envelope::Response::Error(
                    public_error_to_wire(&invalid_argument(
                        "operation lifecycle mutation requires asynchronous orchestration",
                    )),
                ),
                Err(error) => {
                    daemon::response_envelope::Response::Error(public_error_to_wire(&error))
                }
            }
        };
        daemon::ResponseEnvelope {
            request_id,
            response: Some(response),
        }
    }
}

/// Serves one negotiated request/response exchange on an accepted stream.
///
/// A rejected negotiation is returned to the client and closes the connection
/// without reading a request frame.
///
/// # Errors
///
/// Returns [`ServiceError`] when bounded transport or framing fails.
pub fn handle_connection(
    service: &ControlService,
    codec: FrameCodec,
    stream: &mut LocalStream,
) -> Result<(), ServiceError> {
    verify_peer(stream)?;
    let hello = read_client_hello(codec, stream)?;
    let response = service.negotiate(&hello);
    let accepted = response.error.is_none();
    let selected_protocol_minor = response
        .selected_protocol
        .as_ref()
        .map_or(PROTOCOL_MINOR, |version| version.minor);
    write_server_hello(codec, stream, &response)?;
    if !accepted {
        return Ok(());
    }
    let client_instance_id = parse_client_instance_id(&hello.client_instance_id)
        .map_err(|_| ServiceError::InvalidNegotiatedClient)?;
    let request = read_request(codec, stream)?;
    write_response(
        codec,
        stream,
        &service.dispatch_for_client(request, client_instance_id, selected_protocol_minor),
    )?;
    Ok(())
}

/// Serves one negotiated request through bounded async transport and actor lanes.
///
/// Health is answered from lock-free state. Status and cancellation use the
/// high-priority journal lane; submission uses the bounded normal lane.
///
/// # Errors
///
/// Returns [`ServiceError`] for transport, queue, timeout, or actor failures.
pub async fn handle_connection_async(
    service: Arc<ControlService>,
    journal: JournalActorHandle,
    commands: OrchestratorSenders,
    client_connections: Arc<ClientConnectionAdmissions>,
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<(), ServiceError> {
    let hello = read_client_hello_async(codec, stream).await?;
    let selected_protocol = match validate_client_hello(&hello, service.instance_nonce) {
        Ok(selected_protocol) => selected_protocol,
        Err(error) => {
            let response = service.server_hello(None, Some(error.as_ref()));
            write_server_hello_async(codec, stream, &response).await?;
            return Ok(());
        }
    };
    let client_instance_id = parse_client_instance_id(&hello.client_instance_id)
        .map_err(|_| ServiceError::InvalidNegotiatedClient)?;
    let _client_permit = match client_connections.reserve(client_instance_id) {
        Ok(permit) => permit,
        Err(ServiceError::ClientConnectionLimit { limit }) => {
            let error = client_connection_limit(limit);
            let response = service.server_hello(None, Some(&error));
            write_server_hello_async(codec, stream, &response).await?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let selected_protocol_minor = selected_protocol.minor;
    let response = service.server_hello(Some(selected_protocol), None);
    write_server_hello_async(codec, stream, &response).await?;
    let envelope = read_request_async(codec, stream).await?;
    let response = dispatch_async(
        &service,
        &journal,
        &commands,
        envelope,
        client_instance_id,
        selected_protocol_minor,
    )
    .await;
    write_response_async(codec, stream, &response).await?;
    Ok(())
}

async fn dispatch_async(
    service: &ControlService,
    journal: &JournalActorHandle,
    commands: &OrchestratorSenders,
    envelope: daemon::RequestEnvelope,
    client_instance_id: ClientInstanceId,
    selected_protocol_minor: u32,
) -> daemon::ResponseEnvelope {
    let request_id = envelope.request_id;
    let response = if envelope.timeout_ms == Some(0) {
        daemon::response_envelope::Response::Error(public_error_to_wire(&invalid_argument(
            "daemon request timeout is invalid",
        )))
    } else if !nonce_matches(&envelope.instance_nonce, service.instance_nonce) {
        daemon::response_envelope::Response::Error(public_error_to_wire(&permission_denied(
            "daemon instance nonce does not match",
        )))
    } else {
        match request_from_wire(
            envelope.request,
            client_instance_id,
            selected_protocol_minor,
        ) {
            Ok(DecodedRequest::Control(ControlRequest::Health)) => {
                response_to_wire(ControlResponse::Health(service.health()))
            }
            Ok(DecodedRequest::Control(ControlRequest::DiagnosticsQuick)) => {
                run_diagnostic_request(service.clone(), false, envelope.timeout_ms).await
            }
            Ok(DecodedRequest::Control(ControlRequest::SupportBundle)) => {
                run_diagnostic_request(service.clone(), true, envelope.timeout_ms).await
            }
            Ok(DecodedRequest::Submission(prepared))
                if !service.state.accepting_operations.load(Ordering::Acquire) =>
            {
                response_to_wire(ControlResponse::Error(daemon_not_accepting(
                    prepared.operation(),
                )))
            }
            Ok(DecodedRequest::Submission(prepared)) => {
                let timeout_ms = envelope.timeout_ms;
                let operation = prepared.operation();
                let response = async {
                    let pending = service.register_pending_admission(operation)?;
                    let (admission, receiver) = OperationAdmission::registered(prepared, pending);
                    match commands.submissions.try_send(admission) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Full(admission)) => {
                            let (submission, _, _, deadline_retry) =
                                admission.prepared.into_parts();
                            return match journal
                                .retry_status_with_deadline_retry(submission, deadline_retry)
                                .await
                            {
                                Ok(operation) => Ok(ControlResponse::OperationSubmit(operation)),
                                Err(ServiceError::Operations(OperationError::NotFound)) => {
                                    Err(ServiceError::QueueFull)
                                }
                                Err(error) => Err(error),
                            };
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            return Err(ServiceError::ChannelClosed);
                        }
                    }
                    let operation = receiver
                        .await
                        .map_err(|_| ServiceError::ChannelClosed)?
                        .map_err(|error| ServiceError::Public(Box::new(error)))?;
                    Ok(ControlResponse::OperationSubmit(operation))
                };
                await_journal_response(service, response, timeout_ms).await
            }
            Ok(DecodedRequest::Control(ControlRequest::OperationCancel(operation))) => {
                let response = async {
                    match journal
                        .control(ControlRequest::OperationCancel(operation))
                        .await
                    {
                        Err(ServiceError::Operations(OperationError::NotFound)) => {
                            let pending = service.cancel_pending_admission(operation)?;
                            match journal
                                .control(ControlRequest::OperationCancel(operation))
                                .await
                            {
                                Err(ServiceError::Operations(OperationError::NotFound))
                                    if pending =>
                                {
                                    Ok(ControlResponse::Error(operation_not_ready(operation)))
                                }
                                result => result,
                            }
                        }
                        result => result,
                    }
                };
                await_journal_response(service, response, envelope.timeout_ms).await
            }
            Ok(DecodedRequest::Control(request)) => {
                await_journal_response(service, journal.control(request), envelope.timeout_ms).await
            }
            Err(error) => daemon::response_envelope::Response::Error(public_error_to_wire(&error)),
        }
    };
    daemon::ResponseEnvelope {
        request_id,
        response: Some(response),
    }
}

async fn run_diagnostic_request(
    service: ControlService,
    support_bundle: bool,
    requested_timeout_ms: Option<u32>,
) -> daemon::response_envelope::Response {
    let timeout = bounded_request_timeout(&service, requested_timeout_ms);
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return response_to_wire(ControlResponse::Error(request_timed_out()));
    };
    let Some(actor) = service.diagnostic_actor.as_ref() else {
        return response_to_wire(ControlResponse::Error(internal_error()));
    };
    let receiver = match actor.request(support_bundle, deadline) {
        Ok(receiver) => receiver,
        Err(ServiceError::QueueFull) if support_bundle => {
            return response_to_wire(ControlResponse::Error(queue_full(1)));
        }
        Err(ServiceError::QueueFull) => {
            return response_to_wire(ControlResponse::DiagnosticsQuick(DiagnosticsQuick {
                schema_version: 1,
                overall_status: HealthStatus::Degraded,
                catalog: DiagnosticResult {
                    outcome: DiagnosticOutcome::Unavailable,
                    duration_ms: 0,
                    error: Some(queue_full(1)),
                },
            }));
        }
        Err(ServiceError::ChannelClosed) => {
            return response_to_wire(ControlResponse::Error(request_timed_out()));
        }
        Err(_) => return response_to_wire(ControlResponse::Error(internal_error())),
    };
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver).await {
        Ok(Ok(response)) => response_to_wire(response),
        Ok(Err(_)) => response_to_wire(ControlResponse::Error(internal_error())),
        Err(_) => response_to_wire(ControlResponse::Error(request_timed_out())),
    }
}

async fn await_journal_response(
    service: &ControlService,
    response: impl std::future::Future<Output = Result<ControlResponse, ServiceError>>,
    requested_timeout_ms: Option<u32>,
) -> daemon::response_envelope::Response {
    match tokio::time::timeout(
        bounded_request_timeout(service, requested_timeout_ms),
        response,
    )
    .await
    {
        Ok(Ok(response)) => response_to_wire(response),
        Ok(Err(ServiceError::Operations(error))) => response_to_wire(ControlResponse::Error(
            operation_error_to_public(&error, None),
        )),
        Ok(Err(ServiceError::Public(error))) => response_to_wire(ControlResponse::Error(*error)),
        Ok(Err(ServiceError::QueueFull)) => response_to_wire(ControlResponse::Error(queue_full(
            service.limits.operation_queue_limit,
        ))),
        Ok(Err(ServiceError::ClientOperationLimit { limit })) => {
            response_to_wire(ControlResponse::Error(client_operation_limit(limit)))
        }
        Ok(Err(_)) => response_to_wire(ControlResponse::Error(internal_error())),
        Err(_) => response_to_wire(ControlResponse::Error(request_timed_out())),
    }
}

fn bounded_request_timeout(
    service: &ControlService,
    requested_timeout_ms: Option<u32>,
) -> Duration {
    requested_timeout_ms.map_or(service.limits.request_timeout, |milliseconds| {
        Duration::from_millis(u64::from(milliseconds)).min(service.limits.request_timeout)
    })
}

fn duration_ms(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn diagnostic_outcome_for_error(error: &OperationError) -> DiagnosticOutcome {
    match error {
        OperationError::DiagnosticTimedOut => DiagnosticOutcome::TimedOut,
        OperationError::Busy | OperationError::WriterBusy | OperationError::ConcurrentUpdate => {
            DiagnosticOutcome::Unavailable
        }
        _ => DiagnosticOutcome::Failed,
    }
}

fn diagnostic_result_is_conclusive(error: &OperationError) -> bool {
    !matches!(
        error,
        OperationError::DiagnosticTimedOut
            | OperationError::Busy
            | OperationError::WriterBusy
            | OperationError::ConcurrentUpdate
    )
}

fn diagnostic_health_status(error: &OperationError) -> HealthStatus {
    match error {
        OperationError::Busy
        | OperationError::WriterBusy
        | OperationError::ConcurrentUpdate
        | OperationError::DiagnosticTimedOut => HealthStatus::Degraded,
        OperationError::CorruptState
        | OperationError::CorruptSchema
        | OperationError::ForeignCatalog
        | OperationError::MigrationChecksumMismatch
        | OperationError::UnsupportedLegacySchema
        | OperationError::UnsupportedSchemaVersion { .. } => HealthStatus::Failed,
        _ => HealthStatus::Unavailable,
    }
}

fn health_snapshot(health: &Health) -> HealthSnapshot {
    HealthSnapshot {
        ready: health.ready,
        lifecycle: match health.lifecycle {
            DaemonLifecycle::Starting => ObservabilityDaemonLifecycle::Starting,
            DaemonLifecycle::Ready => ObservabilityDaemonLifecycle::Ready,
            DaemonLifecycle::Draining => ObservabilityDaemonLifecycle::Draining,
            DaemonLifecycle::Faulted => ObservabilityDaemonLifecycle::Faulted,
            DaemonLifecycle::Stopped => ObservabilityDaemonLifecycle::Stopped,
        },
        accepting_operations: health.accepting_operations,
        active_connections: health.active_connections,
        connection_limit: health.connection_limit,
        admitted_operations: health.admitted_operations,
        queued_operations: health.queued_operations,
        running_operations: health.running_operations,
        operation_queue_limit: health.operation_queue_limit,
        catalog_status: observability_health_status(health.catalog_status),
        catalog_schema_version: health.catalog_schema_version,
        generation_status: observability_health_status(health.generation_status),
        adapter_status: observability_health_status(health.adapter_status),
        watcher_status: observability_health_status(health.watcher_status),
        endpoint_status: observability_health_status(health.endpoint_status),
        endpoint_schema_version: health.endpoint_schema_version,
        resource_pressure: match health.resource_pressure {
            ResourcePressure::Normal => rootlight_observability::ResourcePressure::Normal,
            ResourcePressure::Elevated => rootlight_observability::ResourcePressure::Elevated,
            ResourcePressure::High => rootlight_observability::ResourcePressure::High,
            ResourcePressure::Critical => rootlight_observability::ResourcePressure::Critical,
            ResourcePressure::Unknown => rootlight_observability::ResourcePressure::Unknown,
        },
    }
}

const fn observability_health_status(
    status: HealthStatus,
) -> rootlight_observability::HealthStatus {
    match status {
        HealthStatus::Healthy => rootlight_observability::HealthStatus::Healthy,
        HealthStatus::Degraded => rootlight_observability::HealthStatus::Degraded,
        HealthStatus::Unavailable => rootlight_observability::HealthStatus::Unavailable,
        HealthStatus::NotConfigured => rootlight_observability::HealthStatus::NotConfigured,
        HealthStatus::Failed => rootlight_observability::HealthStatus::Failed,
    }
}

fn diagnostics_snapshot(diagnostics: &DiagnosticsQuick) -> DiagnosticsQuickSnapshot {
    DiagnosticsQuickSnapshot {
        schema_version: diagnostics.schema_version,
        overall_status: observability_health_status(diagnostics.overall_status),
        catalog_quick_check: match diagnostics.catalog.outcome {
            DiagnosticOutcome::Passed => rootlight_observability::DiagnosticOutcome::Passed,
            DiagnosticOutcome::Failed => rootlight_observability::DiagnosticOutcome::Failed,
            DiagnosticOutcome::TimedOut => rootlight_observability::DiagnosticOutcome::TimedOut,
            DiagnosticOutcome::Unavailable => {
                rootlight_observability::DiagnosticOutcome::Unavailable
            }
        },
        duration_ms: diagnostics.catalog.duration_ms,
        error_code: diagnostics
            .catalog
            .error
            .as_ref()
            .map(|error| observability_error_code(error.code())),
    }
}

const fn observability_error_code(code: ErrorCode) -> ObservabilityErrorCode {
    match code {
        ErrorCode::InvalidArgument => ObservabilityErrorCode::InvalidArgument,
        ErrorCode::NotFound => ObservabilityErrorCode::NotFound,
        ErrorCode::Conflict => ObservabilityErrorCode::Conflict,
        ErrorCode::StaleGeneration => ObservabilityErrorCode::StaleGeneration,
        ErrorCode::UnsupportedCapability => ObservabilityErrorCode::UnsupportedCapability,
        ErrorCode::IncompleteCoverage => ObservabilityErrorCode::IncompleteCoverage,
        ErrorCode::BudgetExceeded => ObservabilityErrorCode::BudgetExceeded,
        ErrorCode::ResourceExhausted => ObservabilityErrorCode::ResourceExhausted,
        ErrorCode::Cancelled => ObservabilityErrorCode::Cancelled,
        ErrorCode::AdapterFailed => ObservabilityErrorCode::AdapterFailed,
        ErrorCode::IndexCorrupt => ObservabilityErrorCode::IndexCorrupt,
        ErrorCode::MigrationRequired => ObservabilityErrorCode::MigrationRequired,
        ErrorCode::PermissionDenied => ObservabilityErrorCode::PermissionDenied,
        ErrorCode::ProtocolMismatch => ObservabilityErrorCode::ProtocolMismatch,
        ErrorCode::Busy => ObservabilityErrorCode::Busy,
        ErrorCode::Internal => ObservabilityErrorCode::Internal,
        _ => ObservabilityErrorCode::Internal,
    }
}

fn observability_operating_system() -> OperatingSystem {
    match std::env::consts::OS {
        "linux" => OperatingSystem::Linux,
        "macos" => OperatingSystem::Macos,
        "windows" => OperatingSystem::Windows,
        _ => OperatingSystem::Other,
    }
}

fn observability_architecture() -> ObservabilityArchitecture {
    match std::env::consts::ARCH {
        "aarch64" => ObservabilityArchitecture::Aarch64,
        "arm" => ObservabilityArchitecture::Arm,
        "x86" => ObservabilityArchitecture::X86,
        "x86_64" => ObservabilityArchitecture::X86_64,
        _ => ObservabilityArchitecture::Other,
    }
}

fn validate_client_hello(
    hello: &daemon::ClientHello,
    instance_nonce: [u8; 16],
) -> Result<common::ContractVersion, Box<PublicError>> {
    if !nonce_matches(&hello.expected_instance_nonce, instance_nonce) {
        return Err(Box::new(permission_denied(
            "daemon instance nonce does not match",
        )));
    }
    if hello.client_instance_id.len() != 16
        || hello.client_instance_id.iter().all(|byte| *byte == 0)
    {
        return Err(Box::new(invalid_argument(
            "client instance identifier is invalid",
        )));
    }
    if hello.capabilities.len() > MAX_CAPABILITIES
        || hello.capabilities.iter().any(|capability| {
            capability.is_empty()
                || capability.len() > MAX_CAPABILITY_BYTES
                || !capability.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b'_')
                })
        })
    {
        return Err(Box::new(invalid_argument(
            "client capabilities are invalid",
        )));
    }
    let range = hello
        .supported_protocols
        .as_ref()
        .ok_or_else(|| Box::new(protocol_mismatch("client protocol range is missing")))?;
    let minimum = range
        .minimum
        .as_ref()
        .ok_or_else(|| Box::new(protocol_mismatch("client protocol range is invalid")))?;
    let maximum = range
        .maximum
        .as_ref()
        .ok_or_else(|| Box::new(protocol_mismatch("client protocol range is invalid")))?;
    if (minimum.major, minimum.minor) > (maximum.major, maximum.minor)
        || minimum.major != PROTOCOL_MAJOR
        || maximum.major != PROTOCOL_MAJOR
    {
        return Err(Box::new(protocol_mismatch(
            "client protocol range is unsupported",
        )));
    }
    let selected_minor = maximum.minor.min(PROTOCOL_MINOR);
    if selected_minor < minimum.minor || selected_minor < MINIMUM_PROTOCOL_MINOR {
        return Err(Box::new(protocol_mismatch(
            "client protocol range is unsupported",
        )));
    }
    Ok(common::ContractVersion {
        major: PROTOCOL_MAJOR,
        minor: selected_minor,
    })
}

enum DecodedRequest {
    Control(ControlRequest),
    Submission(PreparedOperationSubmission),
}

fn request_from_wire(
    request: Option<daemon::request_envelope::Request>,
    client_instance_id: ClientInstanceId,
    selected_protocol_minor: u32,
) -> Result<DecodedRequest, Box<PublicError>> {
    match request {
        Some(daemon::request_envelope::Request::Health(_)) => {
            Ok(DecodedRequest::Control(ControlRequest::Health))
        }
        Some(daemon::request_envelope::Request::DiagnosticsQuick(_)) => {
            if selected_protocol_minor < 3 {
                return Err(Box::new(protocol_mismatch(
                    "quick diagnostics need protocol minor three",
                )));
            }
            Ok(DecodedRequest::Control(ControlRequest::DiagnosticsQuick))
        }
        Some(daemon::request_envelope::Request::SupportBundle(_)) => {
            if selected_protocol_minor < 3 {
                return Err(Box::new(protocol_mismatch(
                    "support bundle needs protocol minor three",
                )));
            }
            Ok(DecodedRequest::Control(ControlRequest::SupportBundle))
        }
        Some(daemon::request_envelope::Request::OperationSubmit(request)) => {
            operation_submission_from_wire(request, client_instance_id, selected_protocol_minor)
                .map(DecodedRequest::Submission)
        }
        Some(daemon::request_envelope::Request::OperationStatus(request)) => {
            parse_operation(request.operation)
                .map(ControlRequest::OperationStatus)
                .map(DecodedRequest::Control)
        }
        Some(daemon::request_envelope::Request::OperationCancel(request)) => {
            parse_operation(request.operation)
                .map(ControlRequest::OperationCancel)
                .map(DecodedRequest::Control)
        }
        Some(daemon::request_envelope::Request::OperationLeaseRenew(request)) => {
            if selected_protocol_minor < 2 {
                return Err(Box::new(protocol_mismatch(
                    "operation lease renewal needs protocol minor two",
                )));
            }
            if request.lease_expires_unix_ms == 0 {
                return Err(Box::new(invalid_argument(
                    "operation lease expiry is invalid",
                )));
            }
            let operation = parse_operation(request.operation)?;
            Err(Box::new(lease_renewal_unsupported(operation)))
        }
        None => Err(Box::new(invalid_argument("daemon request is missing"))),
    }
}

fn operation_submission_from_wire(
    request: daemon::OperationSubmitRequest,
    owner: ClientInstanceId,
    selected_protocol_minor: u32,
) -> Result<PreparedOperationSubmission, Box<PublicError>> {
    if daemon::OperationKind::try_from(request.kind).ok()
        != Some(daemon::OperationKind::ControlProbe)
    {
        return Err(Box::new(invalid_argument("operation kind is invalid")));
    }
    if request.plan_hash.as_slice() != CONTROL_PROBE_PLAN_HASH {
        return Err(Box::new(invalid_argument("operation plan hash is invalid")));
    }
    if request.timeout_ms == Some(0) {
        return Err(Box::new(invalid_argument("operation timeout is invalid")));
    }
    let operation = parse_operation(request.operation)?;
    if selected_protocol_minor < 2 {
        if request.deadline_unix_ms.is_some() || request.lease_expires_unix_ms.is_some() {
            return Err(Box::new(protocol_mismatch(
                "absolute operation timing needs protocol minor two",
            )));
        }
        if !request.detached && owner != ClientInstanceId::SYSTEM {
            return Err(Box::new(protocol_mismatch(
                "attached operations need protocol minor two",
            )));
        }
    }
    if request.timeout_ms.is_some() && request.deadline_unix_ms.is_some() {
        return Err(Box::new(invalid_argument(
            "operation deadline is ambiguous",
        )));
    }
    let clock = if request.timeout_ms.is_some()
        || request.deadline_unix_ms.is_some()
        || request.lease_expires_unix_ms.is_some()
    {
        Some(capture_admission_clock().map_err(operation_preparation_public)?)
    } else {
        None
    };
    let deadline_unix_ms = match request.deadline_unix_ms {
        Some(0) => return Err(Box::new(invalid_argument("operation deadline is invalid"))),
        Some(deadline) => Some(deadline),
        None => match request.timeout_ms {
            Some(timeout_ms) => Some(
                clock
                    .ok_or_else(|| Box::new(invalid_argument("system clock is invalid")))?
                    .wall_unix_ms
                    .checked_add(timeout_ms)
                    .ok_or_else(|| Box::new(invalid_argument("operation timeout is invalid")))?,
            ),
            None => None,
        },
    };
    let detached = request.detached;
    let lease_expires_unix_ms = match (detached, request.lease_expires_unix_ms) {
        (true, None) => None,
        (true, Some(_)) => {
            return Err(Box::new(invalid_argument(
                "detached operation lease is invalid",
            )));
        }
        (false, Some(0) | None) => {
            return Err(Box::new(invalid_argument(
                "attached operation lease is invalid",
            )));
        }
        (false, Some(expiry)) => Some(expiry),
    };
    let submission = OperationSubmission::new(
        operation,
        OperationKind::ControlProbe,
        PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
        owner,
        detached,
        deadline_unix_ms,
        lease_expires_unix_ms,
    )
    .map_err(|_| Box::new(invalid_argument("operation submission is invalid")))?;
    let deadline = deadline_unix_ms
        .zip(clock)
        .map(|(target, clock)| monotonic_target(clock, target))
        .transpose()
        .map_err(operation_preparation_public)?;
    let lease_deadline = lease_expires_unix_ms
        .zip(clock)
        .map(|(target, clock)| monotonic_target(clock, target))
        .transpose()
        .map_err(operation_preparation_public)?;
    let deadline_retry = match request.timeout_ms {
        Some(timeout_ms) => DeadlineRetry::ReanchoredRelative { timeout_ms },
        None => DeadlineRetry::Exact,
    };
    PreparedOperationSubmission::new_with_deadline_retry(
        submission,
        deadline,
        lease_deadline,
        deadline_retry,
    )
    .map_err(operation_preparation_public)
}

fn operation_preparation_public(error: OperationPreparationError) -> Box<PublicError> {
    match error {
        OperationPreparationError::InvalidTimeout => {
            Box::new(invalid_argument("operation timeout is invalid"))
        }
        OperationPreparationError::Clock => Box::new(invalid_argument("system clock is invalid")),
    }
}

fn parse_client_instance_id(bytes: &[u8]) -> Result<ClientInstanceId, Box<PublicError>> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| Box::new(invalid_argument("client instance identifier is invalid")))?;
    ClientInstanceId::new(bytes)
        .map_err(|_| Box::new(invalid_argument("client instance identifier is invalid")))
}

fn parse_operation(
    operation: Option<common::OperationId>,
) -> Result<OperationId, Box<PublicError>> {
    let bytes = operation
        .ok_or_else(|| Box::new(invalid_argument("operation identifier is missing")))?
        .value;
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|_| Box::new(invalid_argument("operation identifier is invalid")))?;
    Ok(OperationId::from_bytes(bytes))
}

fn response_to_wire(response: ControlResponse) -> daemon::response_envelope::Response {
    match response {
        ControlResponse::Health(health) => {
            daemon::response_envelope::Response::Health(daemon::HealthResponse {
                ready: health.ready,
                active_operations: health.active_operations,
                admitted_operations: health.admitted_operations,
                protocol_version: health.protocol_version.to_owned(),
                lifecycle: daemon_lifecycle_to_wire(health.lifecycle) as i32,
                accepting_operations: health.accepting_operations,
                active_connections: health.active_connections,
                connection_limit: health.connection_limit,
                queued_operations: health.queued_operations,
                running_operations: health.running_operations,
                operation_queue_limit: health.operation_queue_limit,
                journal_healthy: health.journal_healthy,
                catalog_status: health_status_to_wire(health.catalog_status) as i32,
                catalog_schema_version: health.catalog_schema_version,
                generation_status: health_status_to_wire(health.generation_status) as i32,
                adapter_status: health_status_to_wire(health.adapter_status) as i32,
                watcher_status: health_status_to_wire(health.watcher_status) as i32,
                resource_pressure: resource_pressure_to_wire(health.resource_pressure) as i32,
                endpoint_status: health_status_to_wire(health.endpoint_status) as i32,
                endpoint_schema_version: health.endpoint_schema_version,
            })
        }
        ControlResponse::DiagnosticsQuick(diagnostics) => {
            daemon::response_envelope::Response::DiagnosticsQuick(
                daemon::DiagnosticsQuickResponse {
                    schema_version: diagnostics.schema_version,
                    overall_status: health_status_to_wire(diagnostics.overall_status) as i32,
                    results: vec![daemon::DiagnosticResult {
                        check: daemon::DiagnosticCheck::CatalogQuickCheck as i32,
                        outcome: diagnostic_outcome_to_wire(diagnostics.catalog.outcome) as i32,
                        duration_ms: diagnostics.catalog.duration_ms,
                        error: diagnostics.catalog.error.as_ref().map(public_error_to_wire),
                    }],
                },
            )
        }
        ControlResponse::SupportBundle(bundle) => {
            daemon::response_envelope::Response::SupportBundle(daemon::SupportBundleResponse {
                schema_version: bundle.schema_version,
                archive: bundle.archive,
                sha256: bundle.sha256.to_vec(),
                archive_bytes: bundle.archive_bytes,
                contains_source: bundle.contains_source,
            })
        }
        ControlResponse::OperationSubmit(operation) => {
            daemon::response_envelope::Response::OperationSubmit(daemon::OperationSubmitResponse {
                operation: Some(operation_record_to_wire(&operation)),
            })
        }
        ControlResponse::OperationStatus(operation) => {
            daemon::response_envelope::Response::OperationStatus(daemon::OperationStatusResponse {
                operation: Some(operation_record_to_wire(&operation)),
            })
        }
        ControlResponse::OperationLeaseRenew(operation) => {
            daemon::response_envelope::Response::OperationLeaseRenew(
                daemon::OperationLeaseRenewResponse {
                    operation: Some(operation_record_to_wire(&operation)),
                },
            )
        }
        ControlResponse::OperationCancel {
            accepted,
            operation,
        } => {
            daemon::response_envelope::Response::OperationCancel(daemon::OperationCancelResponse {
                operation: Some(operation_record_to_wire(&operation)),
                accepted,
            })
        }
        ControlResponse::Error(error) => {
            daemon::response_envelope::Response::Error(public_error_to_wire(&error))
        }
    }
}

const fn health_status_to_wire(status: HealthStatus) -> daemon::HealthStatus {
    match status {
        HealthStatus::Healthy => daemon::HealthStatus::Healthy,
        HealthStatus::Degraded => daemon::HealthStatus::Degraded,
        HealthStatus::Unavailable => daemon::HealthStatus::Unavailable,
        HealthStatus::NotConfigured => daemon::HealthStatus::NotConfigured,
        HealthStatus::Failed => daemon::HealthStatus::Failed,
    }
}

const fn resource_pressure_to_wire(pressure: ResourcePressure) -> daemon::ResourcePressure {
    match pressure {
        ResourcePressure::Normal => daemon::ResourcePressure::Normal,
        ResourcePressure::Elevated => daemon::ResourcePressure::Elevated,
        ResourcePressure::High => daemon::ResourcePressure::High,
        ResourcePressure::Critical => daemon::ResourcePressure::Critical,
        ResourcePressure::Unknown => daemon::ResourcePressure::Unknown,
    }
}

const fn diagnostic_outcome_to_wire(outcome: DiagnosticOutcome) -> daemon::DiagnosticOutcome {
    match outcome {
        DiagnosticOutcome::Passed => daemon::DiagnosticOutcome::Passed,
        DiagnosticOutcome::Failed => daemon::DiagnosticOutcome::Failed,
        DiagnosticOutcome::TimedOut => daemon::DiagnosticOutcome::TimedOut,
        DiagnosticOutcome::Unavailable => daemon::DiagnosticOutcome::Unavailable,
    }
}

const fn daemon_lifecycle_to_wire(lifecycle: DaemonLifecycle) -> daemon::DaemonLifecycle {
    match lifecycle {
        DaemonLifecycle::Starting => daemon::DaemonLifecycle::Starting,
        DaemonLifecycle::Ready => daemon::DaemonLifecycle::Ready,
        DaemonLifecycle::Draining => daemon::DaemonLifecycle::Draining,
        DaemonLifecycle::Faulted => daemon::DaemonLifecycle::Faulted,
        DaemonLifecycle::Stopped => daemon::DaemonLifecycle::Stopped,
    }
}

fn operation_record_to_wire(record: &OperationRecord) -> daemon::OperationStatus {
    daemon::OperationStatus {
        operation: Some(common::OperationId {
            value: record.operation.as_bytes().to_vec(),
        }),
        state: operation_state_to_wire(record.state) as i32,
        revision: record.revision,
        completed_units: record.progress.completed,
        total_units: record.progress.total,
        error: record.error.as_ref().map(public_error_to_wire),
        kind: operation_kind_to_wire(record.kind) as i32,
        stage: operation_stage_to_wire(record.stage) as i32,
        plan_hash: record.plan_hash.as_bytes().to_vec(),
        detached: record.detached,
        cancellation_requested: record.cancellation_requested,
        deadline_unix_ms: record.deadline_unix_ms,
        lease_expires_unix_ms: record.lease_expires_unix_ms,
        recovery_class: recovery_class_to_wire(record.recovery_class) as i32,
    }
}

const fn operation_kind_to_wire(kind: OperationKind) -> daemon::OperationKind {
    match kind {
        OperationKind::ControlProbe => daemon::OperationKind::ControlProbe,
    }
}

const fn operation_stage_to_wire(stage: OperationStage) -> daemon::OperationStage {
    match stage {
        OperationStage::Accepted => daemon::OperationStage::Accepted,
        OperationStage::Executing => daemon::OperationStage::Executing,
        OperationStage::Cleanup => daemon::OperationStage::Cleanup,
    }
}

const fn recovery_class_to_wire(recovery: RecoveryClass) -> daemon::RecoveryClass {
    match recovery {
        RecoveryClass::NotApplicable => daemon::RecoveryClass::NotApplicable,
        RecoveryClass::InterruptedByRestart => daemon::RecoveryClass::InterruptedByRestart,
        RecoveryClass::DeadlineElapsed => daemon::RecoveryClass::DeadlineElapsed,
        RecoveryClass::LeaseExpired => daemon::RecoveryClass::LeaseExpired,
    }
}

const fn operation_state_to_wire(state: OperationState) -> daemon::OperationState {
    match state {
        OperationState::Queued => daemon::OperationState::Queued,
        OperationState::Running => daemon::OperationState::Running,
        OperationState::Cancelling => daemon::OperationState::Cancelling,
        OperationState::Succeeded => daemon::OperationState::Succeeded,
        OperationState::Failed => daemon::OperationState::Failed,
        OperationState::Cancelled => daemon::OperationState::Cancelled,
        OperationState::Interrupted => daemon::OperationState::Interrupted,
    }
}

fn operation_error_to_public(
    error: &OperationError,
    operation: Option<OperationId>,
) -> PublicError {
    let (code, message, retryable) = match error {
        OperationError::NotFound => (ErrorCode::NotFound, "operation was not found", false),
        OperationError::AlreadyExists
        | OperationError::SubmissionConflict
        | OperationError::IllegalTransition { .. }
        | OperationError::CancellationWon
        | OperationError::InvalidTerminalError
        | OperationError::InvalidProgress
        | OperationError::InvalidStage
        | OperationError::LeaseOwnerMismatch
        | OperationError::InvalidLease => (
            ErrorCode::Conflict,
            "operation state conflicts with request",
            false,
        ),
        OperationError::InvalidClientInstanceId | OperationError::InvalidSubmission => (
            ErrorCode::InvalidArgument,
            "operation submission is invalid",
            false,
        ),
        OperationError::WriterBusy | OperationError::ConcurrentUpdate | OperationError::Busy => {
            (ErrorCode::Busy, "operation state is busy", true)
        }
        OperationError::DiagnosticTimedOut => {
            (ErrorCode::Busy, "operation diagnostic timed out", true)
        }
        OperationError::UnsupportedSqlite { .. }
        | OperationError::UnsupportedSqliteCompileOptions
        | OperationError::UnsupportedSqliteConfiguration
        | OperationError::CorruptState
        | OperationError::CorruptSchema
        | OperationError::ForeignCatalog
        | OperationError::MigrationChecksumMismatch
        | OperationError::UnsupportedLegacySchema
        | OperationError::UnsupportedSchemaVersion { .. }
        | OperationError::DeserializePublicError(_)
        | OperationError::PublicErrorTooLarge => (
            ErrorCode::IndexCorrupt,
            "operation journal is corrupt",
            false,
        ),
        OperationError::RevisionOverflow
        | OperationError::UnsupportedCancellationReason
        | OperationError::MutexPoisoned
        | OperationError::SerializePublicError(_)
        | OperationError::SystemClockBeforeEpoch
        | OperationError::TimestampOverflow
        | OperationError::InsecureLockFile
        | OperationError::WindowsSecurityPolicy
        | OperationError::Sqlite(_)
        | OperationError::LockIo(_) => (ErrorCode::Internal, "internal operation failed", false),
    };
    let mut builder = PublicError::builder(code, message);
    if retryable {
        builder = builder.retryable();
    }
    if let Some(operation) = operation {
        builder = builder
            .operation(operation)
            .next_action(NextAction::InspectOperation);
    }
    builder
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

impl ServiceError {
    /// Reports whether one orchestration failure makes the daemon unusable.
    #[must_use]
    pub const fn is_fatal_submission_failure(&self) -> bool {
        match self {
            Self::QueueFull
            | Self::ClientOperationLimit { .. }
            | Self::NotAccepting
            | Self::Public(_)
            | Self::Operations(
                OperationError::NotFound
                | OperationError::AlreadyExists
                | OperationError::SubmissionConflict
                | OperationError::InvalidClientInstanceId
                | OperationError::InvalidSubmission
                | OperationError::IllegalTransition { .. }
                | OperationError::CancellationWon
                | OperationError::InvalidTerminalError
                | OperationError::InvalidProgress
                | OperationError::InvalidStage
                | OperationError::LeaseOwnerMismatch
                | OperationError::InvalidLease
                | OperationError::ConcurrentUpdate
                | OperationError::Busy
                | OperationError::WriterBusy
                | OperationError::DiagnosticTimedOut,
            ) => false,
            Self::Ipc(_)
            | Self::InvalidNegotiatedClient
            | Self::UnsupportedPublicErrorVariant
            | Self::InvalidLimits
            | Self::ChannelClosed
            | Self::ClientConnectionLimit { .. }
            | Self::AdmissionStatePoisoned
            | Self::RequestTimedOut
            | Self::TaskFailed(_)
            | Self::ThreadSpawn(_)
            | Self::ThreadPanicked
            | Self::Operations(
                OperationError::UnsupportedSqlite { .. }
                | OperationError::UnsupportedSqliteCompileOptions
                | OperationError::UnsupportedSqliteConfiguration
                | OperationError::CorruptState
                | OperationError::CorruptSchema
                | OperationError::ForeignCatalog
                | OperationError::MigrationChecksumMismatch
                | OperationError::UnsupportedLegacySchema
                | OperationError::UnsupportedSchemaVersion { .. }
                | OperationError::DeserializePublicError(_)
                | OperationError::PublicErrorTooLarge
                | OperationError::RevisionOverflow
                | OperationError::UnsupportedCancellationReason
                | OperationError::MutexPoisoned
                | OperationError::SerializePublicError(_)
                | OperationError::SystemClockBeforeEpoch
                | OperationError::TimestampOverflow
                | OperationError::InsecureLockFile
                | OperationError::WindowsSecurityPolicy
                | OperationError::Sqlite(_)
                | OperationError::LockIo(_),
            )
            | Self::UnexpectedResponse
            | Self::Clock
            | Self::TimerAlreadyRegistered => true,
        }
    }

    fn to_public(&self) -> PublicError {
        match self {
            Self::QueueFull => queue_full(DEFAULT_OPERATION_QUEUE_LIMIT),
            Self::ClientOperationLimit { limit } => client_operation_limit(*limit),
            Self::NotAccepting => {
                PublicError::builder(ErrorCode::Busy, "daemon is not accepting operations")
                    .retryable()
                    .next_action(NextAction::Retry)
                    .build()
                    .unwrap_or_else(|_| {
                        unreachable!("closed public error templates are statically bounded")
                    })
            }
            Self::Operations(error) => operation_error_to_public(error, None),
            _ => internal_error(),
        }
    }
}

fn queue_full(limit: u32) -> PublicError {
    let queue_limit = rootlight_error::DetailKey::parse("queue_limit")
        .unwrap_or_else(|_| unreachable!("hard-coded detail key is valid"));
    PublicError::builder(ErrorCode::ResourceExhausted, "operation queue is full")
        .retryable()
        .detail(queue_limit, PublicValue::Unsigned(u64::from(limit)))
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn client_operation_limit(limit: u32) -> PublicError {
    let client_limit = rootlight_error::DetailKey::parse("client_operation_limit")
        .unwrap_or_else(|_| unreachable!("hard-coded detail key is valid"));
    PublicError::builder(
        ErrorCode::ResourceExhausted,
        "client operation quota is exhausted",
    )
    .retryable()
    .detail(client_limit, PublicValue::Unsigned(u64::from(limit)))
    .next_action(NextAction::Retry)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn client_connection_limit(limit: u32) -> PublicError {
    let client_limit = rootlight_error::DetailKey::parse("client_connection_limit")
        .unwrap_or_else(|_| unreachable!("hard-coded detail key is valid"));
    PublicError::builder(
        ErrorCode::ResourceExhausted,
        "client connection quota is exhausted",
    )
    .retryable()
    .detail(client_limit, PublicValue::Unsigned(u64::from(limit)))
    .next_action(NextAction::Retry)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn request_timed_out() -> PublicError {
    PublicError::builder(ErrorCode::Busy, "daemon request timed out")
        .retryable()
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn internal_error() -> PublicError {
    PublicError::builder(ErrorCode::Internal, "internal operation failed")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn operation_not_ready(operation: OperationId) -> PublicError {
    PublicError::builder(ErrorCode::Busy, "operation admission is still pending")
        .retryable()
        .operation(operation)
        .next_action(NextAction::InspectOperation)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn daemon_not_accepting(operation: OperationId) -> PublicError {
    PublicError::builder(ErrorCode::Busy, "daemon is not accepting operations")
        .retryable()
        .operation(operation)
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn invalid_argument(message: &'static str) -> PublicError {
    PublicError::builder(ErrorCode::InvalidArgument, message)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn lease_renewal_unsupported(operation: OperationId) -> PublicError {
    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "operation lease renewal is unsupported",
    )
    .operation(operation)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn permission_denied(message: &'static str) -> PublicError {
    PublicError::builder(ErrorCode::PermissionDenied, message)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
}

fn protocol_mismatch(message: &'static str) -> PublicError {
    PublicError::builder(ErrorCode::ProtocolMismatch, message)
        .next_action(NextAction::SelectSupportedVersion)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error templates are statically bounded"))
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

fn public_error_to_wire(error: &PublicError) -> common::PublicError {
    checked_public_error_to_wire(error).unwrap_or_else(|_| common::PublicError {
        code: common::ErrorCode::Internal as i32,
        message: "internal operation failed".to_owned(),
        retryable: false,
        retry_after_ms: None,
        repository: None,
        operation: None,
        generation: None,
        details: Default::default(),
        next_actions: Vec::new(),
    })
}

fn checked_public_error_to_wire(error: &PublicError) -> Result<common::PublicError, ServiceError> {
    let details = error
        .details()
        .iter()
        .map(|(key, value)| {
            public_value_to_wire(value).map(|value| (key.as_str().to_owned(), value))
        })
        .collect::<Result<_, _>>()?;
    let next_actions = error
        .next_actions()
        .iter()
        .map(next_action_to_wire)
        .collect::<Result<_, _>>()?;
    Ok(common::PublicError {
        code: error_code_to_wire(error.code())? as i32,
        message: error.message().to_owned(),
        retryable: error.retryable(),
        retry_after_ms: error.retry_after_ms(),
        repository: error.repository().map(|repository| common::RepositoryId {
            value: repository.as_bytes().to_vec(),
        }),
        operation: error.operation().map(|operation| common::OperationId {
            value: operation.as_bytes().to_vec(),
        }),
        generation: error.generation().map(|generation| common::GenerationId {
            value: generation.as_bytes().to_vec(),
        }),
        details,
        next_actions,
    })
}

const fn error_code_to_wire(code: ErrorCode) -> Result<common::ErrorCode, ServiceError> {
    match code {
        ErrorCode::InvalidArgument => Ok(common::ErrorCode::InvalidArgument),
        ErrorCode::NotFound => Ok(common::ErrorCode::NotFound),
        ErrorCode::Conflict => Ok(common::ErrorCode::Conflict),
        ErrorCode::StaleGeneration => Ok(common::ErrorCode::StaleGeneration),
        ErrorCode::UnsupportedCapability => Ok(common::ErrorCode::UnsupportedCapability),
        ErrorCode::IncompleteCoverage => Ok(common::ErrorCode::IncompleteCoverage),
        ErrorCode::BudgetExceeded => Ok(common::ErrorCode::BudgetExceeded),
        ErrorCode::ResourceExhausted => Ok(common::ErrorCode::ResourceExhausted),
        ErrorCode::Cancelled => Ok(common::ErrorCode::Cancelled),
        ErrorCode::AdapterFailed => Ok(common::ErrorCode::AdapterFailed),
        ErrorCode::IndexCorrupt => Ok(common::ErrorCode::IndexCorrupt),
        ErrorCode::MigrationRequired => Ok(common::ErrorCode::MigrationRequired),
        ErrorCode::PermissionDenied => Ok(common::ErrorCode::PermissionDenied),
        ErrorCode::ProtocolMismatch => Ok(common::ErrorCode::ProtocolMismatch),
        ErrorCode::Busy => Ok(common::ErrorCode::Busy),
        ErrorCode::Internal => Ok(common::ErrorCode::Internal),
        _ => Err(ServiceError::UnsupportedPublicErrorVariant),
    }
}

fn public_value_to_wire(value: &PublicValue) -> Result<common::PublicValue, ServiceError> {
    use common::public_value::Value;
    let value = match value {
        PublicValue::Boolean(value) => Value::Boolean(*value),
        PublicValue::Integer(value) => Value::Integer(*value),
        PublicValue::Unsigned(value) => Value::Unsigned(*value),
        PublicValue::Repository(value) => Value::Repository(common::RepositoryId {
            value: value.as_bytes().to_vec(),
        }),
        PublicValue::Generation(value) => Value::Generation(common::GenerationId {
            value: value.as_bytes().to_vec(),
        }),
        PublicValue::Operation(value) => Value::Operation(common::OperationId {
            value: value.as_bytes().to_vec(),
        }),
        PublicValue::Label(value) => Value::Label(value.as_str().to_owned()),
        _ => return Err(ServiceError::UnsupportedPublicErrorVariant),
    };
    Ok(common::PublicValue { value: Some(value) })
}

fn next_action_to_wire(action: &NextAction) -> Result<common::NextAction, ServiceError> {
    let (kind, field) = match action {
        NextAction::CorrectField { field } => (
            common::next_action::Kind::CorrectField,
            Some(field.as_str().to_owned()),
        ),
        NextAction::Retry => (common::next_action::Kind::Retry, None),
        NextAction::SelectSupportedVersion => {
            (common::next_action::Kind::SelectSupportedVersion, None)
        }
        NextAction::InspectOperation => (common::next_action::Kind::InspectOperation, None),
        NextAction::RebuildRepository => (common::next_action::Kind::RebuildRepository, None),
        NextAction::CollectSupportBundle => (common::next_action::Kind::CollectSupportBundle, None),
        _ => return Err(ServiceError::UnsupportedPublicErrorVariant),
    };
    Ok(common::NextAction {
        kind: kind as i32,
        field,
    })
}

/// Daemon service failures that cannot be represented as ordinary responses.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Local framed transport failed.
    #[error("daemon transport failed")]
    Ipc(#[from] IpcError),
    /// Negotiation accepted a client identity that could not be reconstructed.
    #[error("negotiated daemon client identity is invalid")]
    InvalidNegotiatedClient,
    /// A future public-error variant has no representation in this protocol minor.
    #[error("daemon public error variant is unsupported")]
    UnsupportedPublicErrorVariant,
    /// Daemon capacities or deadlines were zero.
    #[error("daemon resource limits are invalid")]
    InvalidLimits,
    /// A bounded daemon orchestration lane closed unexpectedly.
    #[error("daemon orchestration channel closed")]
    ChannelClosed,
    /// A bounded daemon orchestration lane is saturated.
    #[error("daemon orchestration queue is full")]
    QueueFull,
    /// One validated client-declared identity reached its nonterminal operation allowance.
    #[error("daemon client operation quota is exhausted")]
    ClientOperationLimit {
        /// Maximum admitted nonterminal operations for the client.
        limit: u32,
    },
    /// One validated client-declared identity reached its negotiated connection allowance.
    #[error("daemon client connection quota is exhausted")]
    ClientConnectionLimit {
        /// Maximum simultaneous negotiated connections for the client.
        limit: u32,
    },
    /// A synchronous client-admission ledger was poisoned.
    #[error("daemon operation admission state is unavailable")]
    AdmissionStatePoisoned,
    /// A daemon request exceeded its response deadline.
    #[error("daemon request timed out")]
    RequestTimedOut,
    /// A daemon background task terminated unexpectedly.
    #[error("daemon task failed")]
    TaskFailed(#[source] tokio::task::JoinError),
    /// The journal actor thread could not be created.
    #[error("daemon journal thread could not start")]
    ThreadSpawn(#[source] std::io::Error),
    /// The journal actor thread panicked.
    #[error("daemon journal thread panicked")]
    ThreadPanicked,
    /// The durable operation journal failed.
    #[error("daemon journal operation failed")]
    Operations(#[source] OperationError),
    /// The daemon is draining or faulted and rejects new work.
    #[error("daemon is not accepting operations")]
    NotAccepting,
    /// An internal actor returned a response for another command kind.
    #[error("daemon actor returned an unexpected response")]
    UnexpectedResponse,
    /// The system clock cannot provide a supported timestamp.
    #[error("daemon clock is invalid")]
    Clock,
    /// A process-local timer was registered twice.
    #[error("operation timer is already registered")]
    TimerAlreadyRegistered,
    /// A stable public error was returned by bounded orchestration.
    #[error("daemon request failed")]
    Public(Box<PublicError>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_client::Client;
    use rootlight_ipc::{
        AsyncLocalListener, Endpoint, LocalListener, connect_async, read_server_hello_async,
        write_client_hello_async,
    };
    use rootlight_operations::Progress;
    use std::{path::PathBuf, sync::mpsc, thread, time::Duration};
    use tempfile::{TempDir, tempdir};

    fn service() -> ControlService {
        ControlService::new(
            Arc::new(OperationJournal::open_in_memory().expect("journal opens")),
            [7; 16],
        )
    }

    fn prepared(submission: OperationSubmission) -> PreparedOperationSubmission {
        PreparedOperationSubmission::from_submission(submission)
            .expect("submission timing prepares")
    }

    fn prepared_at(
        submission: OperationSubmission,
        clock: AdmissionClockSample,
    ) -> PreparedOperationSubmission {
        let deadline = submission
            .deadline_unix_ms
            .map(|target| monotonic_target(clock, target).expect("deadline fits"));
        let lease_deadline = submission
            .lease_expires_unix_ms
            .map(|target| monotonic_target(clock, target).expect("lease fits"));
        PreparedOperationSubmission::new(submission, deadline, lease_deadline)
            .expect("submission timing prepares")
    }

    fn admission(
        submission: OperationSubmission,
    ) -> (
        OperationAdmission,
        tokio::sync::oneshot::Receiver<Result<OperationRecord, PublicError>>,
    ) {
        OperationAdmission::new(prepared(submission), Arc::new(AtomicBool::new(false)))
    }

    fn supported_hello(nonce: Vec<u8>) -> daemon::ClientHello {
        supported_hello_for(nonce, [9; 16])
    }

    fn supported_hello_for(nonce: Vec<u8>, client_instance_id: [u8; 16]) -> daemon::ClientHello {
        daemon::ClientHello {
            supported_protocols: Some(common::VersionRange {
                minimum: Some(common::ContractVersion {
                    major: 1,
                    minor: rootlight_protocol::MINIMUM_PROTOCOL_MINOR,
                }),
                maximum: Some(common::ContractVersion {
                    major: 1,
                    minor: PROTOCOL_MINOR,
                }),
            }),
            capabilities: vec!["health".to_owned()],
            expected_instance_nonce: nonce,
            client_instance_id: client_instance_id.to_vec(),
        }
    }

    fn private_tempdir() -> TempDir {
        let temporary = tempdir().expect("temporary directory is available");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temporary directory becomes private");
        }
        temporary
    }

    fn endpoint(temporary: &TempDir) -> Endpoint {
        endpoint_named(temporary, "rootlight")
    }

    fn endpoint_named(temporary: &TempDir, label: &str) -> Endpoint {
        #[cfg(unix)]
        let path = temporary.path().join(format!("{label}.sock"));
        #[cfg(windows)]
        let path = PathBuf::from(format!(
            r"\\.\pipe\rootlight-daemon-core-{}-{}-{label}",
            std::process::id(),
            temporary.path().display().to_string().len()
        ));
        Endpoint::new(path).expect("endpoint is valid")
    }

    #[test]
    fn negotiation_rejects_stale_nonce_and_unsupported_major() {
        let service = service();
        let stale = service.negotiate(&supported_hello(vec![6; 16]));
        assert!(stale.error.is_some());

        let mut invalid_client = supported_hello(vec![7; 16]);
        invalid_client.client_instance_id = vec![0; 16];
        assert!(service.negotiate(&invalid_client).error.is_some());

        let previous_minor = service.negotiate(&daemon::ClientHello {
            supported_protocols: Some(common::VersionRange {
                minimum: Some(common::ContractVersion { major: 1, minor: 0 }),
                maximum: Some(common::ContractVersion { major: 1, minor: 0 }),
            }),
            capabilities: vec!["health".to_owned()],
            expected_instance_nonce: vec![7; 16],
            client_instance_id: vec![9; 16],
        });
        assert_eq!(
            previous_minor
                .error
                .expect("obsolete minor is rejected")
                .code,
            common::ErrorCode::ProtocolMismatch as i32
        );
        assert!(previous_minor.selected_protocol.is_none());

        let future_range = service.negotiate(&daemon::ClientHello {
            supported_protocols: Some(common::VersionRange {
                minimum: Some(common::ContractVersion { major: 1, minor: 1 }),
                maximum: Some(common::ContractVersion { major: 1, minor: 9 }),
            }),
            capabilities: vec!["health".to_owned()],
            expected_instance_nonce: vec![7; 16],
            client_instance_id: vec![9; 16],
        });
        assert_eq!(
            future_range
                .selected_protocol
                .expect("overlapping range negotiates")
                .minor,
            PROTOCOL_MINOR
        );

        let mut unsupported = supported_hello(vec![7; 16]);
        unsupported.supported_protocols = Some(common::VersionRange {
            minimum: Some(common::ContractVersion { major: 2, minor: 0 }),
            maximum: Some(common::ContractVersion { major: 2, minor: 1 }),
        });
        let rejected = service.negotiate(&unsupported);
        assert_eq!(
            rejected.error.expect("negotiation fails").code,
            common::ErrorCode::ProtocolMismatch as i32
        );
    }

    #[test]
    fn diagnostics_and_support_bundle_are_source_free_and_wire_stable() {
        let service = service();
        service.state().set_catalog_status(HealthStatus::Healthy);
        let diagnostics = service.execute(ControlRequest::DiagnosticsQuick);
        let ControlResponse::DiagnosticsQuick(diagnostics) = diagnostics else {
            panic!("diagnostics response expected");
        };
        assert_eq!(diagnostics.schema_version, 1);
        assert_eq!(diagnostics.overall_status, HealthStatus::Healthy);
        assert_eq!(diagnostics.catalog.outcome, DiagnosticOutcome::Passed);
        assert!(diagnostics.catalog.error.is_none());

        let bundle = service.execute(ControlRequest::SupportBundle);
        let ControlResponse::SupportBundle(bundle) = bundle else {
            panic!("support bundle response expected");
        };
        assert_eq!(bundle.schema_version, 1);
        assert!(!bundle.contains_source);
        assert_eq!(
            bundle.archive_bytes,
            u64::try_from(bundle.archive.len()).expect("bounded archive fits u64")
        );
        assert!(bundle.archive.len() <= rootlight_observability::MAX_SUPPORT_ARCHIVE_BYTES);

        let wire = response_to_wire(ControlResponse::SupportBundle(bundle.clone()));
        let daemon::response_envelope::Response::SupportBundle(wire) = wire else {
            panic!("wire support bundle expected");
        };
        assert_eq!(wire.archive_bytes, bundle.archive_bytes);
        assert_eq!(wire.sha256, bundle.sha256);
        assert!(!wire.contains_source);
    }

    #[test]
    fn diagnostic_actor_enforces_one_total_admission() {
        let service = service();
        let actor = DiagnosticActor::start(service).expect("diagnostic actor starts");
        actor.handle.state.busy.store(true, Ordering::Release);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .expect("test deadline derives");

        assert!(matches!(
            actor.handle().request(false, deadline),
            Err(ServiceError::QueueFull)
        ));
        actor.handle.state.busy.store(false, Ordering::Release);
        actor
            .join_for_test()
            .expect("diagnostic actor joins after stop");
    }

    #[test]
    fn diagnostic_actor_stop_is_independent_of_service_clones() {
        let service = service();
        let actor = DiagnosticActor::start(service.clone()).expect("diagnostic actor starts");
        let _retained = [service.clone(), service];
        actor.stop();
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .expect("test deadline derives");

        assert!(matches!(
            actor.handle().request(false, deadline),
            Err(ServiceError::ChannelClosed)
        ));
        actor
            .join_for_test()
            .expect("diagnostic actor joins with retained service clones");
    }

    #[test]
    fn diagnostic_actor_discards_expired_commands_without_health_mutation() {
        let service = service();
        service.state().set_catalog_status(HealthStatus::Healthy);
        let state = Arc::new(DiagnosticActorState {
            stopping: AtomicBool::new(false),
            busy: AtomicBool::new(true),
        });
        let worker_state = Arc::clone(&state);
        let (sender, receiver) = mpsc::sync_channel(1);
        let (reply, response) = tokio::sync::oneshot::channel();
        sender
            .send(DiagnosticCommand::Execute {
                support_bundle: false,
                deadline: Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .expect("test deadline subtracts"),
                reply,
            })
            .expect("expired command queues");
        drop(sender);

        diagnostic_actor_loop(service.clone(), receiver, worker_state);
        let ControlResponse::Error(error) = response
            .blocking_recv()
            .expect("expired command returns a response")
        else {
            panic!("expired command must return an error");
        };
        assert_eq!(error.code(), ErrorCode::Busy);
        assert_eq!(service.health().catalog_status, HealthStatus::Healthy);
        assert!(!state.busy.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn diagnostic_actor_releases_capacity_after_a_timed_request() {
        let temporary = private_tempdir();
        let path = temporary.path().join("operations.sqlite");
        let journal = Arc::new(OperationJournal::open(&path).expect("catalog opens"));
        let service = ControlService::new(journal, [7; 16]).with_catalog_path(path);
        let (service, actor) = service
            .with_diagnostic_actor()
            .expect("diagnostic actor starts");

        let first = run_diagnostic_request(service.clone(), false, None).await;
        let daemon::response_envelope::Response::DiagnosticsQuick(first) = first else {
            panic!("first diagnostics response expected");
        };
        assert_eq!(first.results.len(), 1);
        assert_eq!(
            first.results[0].outcome,
            daemon::DiagnosticOutcome::Passed as i32
        );

        let next = run_diagnostic_request(service, false, None).await;
        let daemon::response_envelope::Response::DiagnosticsQuick(next) = next else {
            panic!("second diagnostics response expected");
        };
        assert_eq!(next.results.len(), 1);
        assert_eq!(
            next.results[0].outcome,
            daemon::DiagnosticOutcome::Passed as i32
        );
        actor
            .join_for_test()
            .expect("diagnostic actor joins after stop");
    }

    #[test]
    fn typed_and_wire_health_share_semantics() {
        let service = service();
        let typed = service.execute(ControlRequest::Health);
        let wire = service.dispatch(daemon::RequestEnvelope {
            request_id: 9,
            instance_nonce: vec![7; 16],
            timeout_ms: None,
            request: Some(daemon::request_envelope::Request::Health(
                daemon::HealthRequest {},
            )),
        });

        assert_eq!(
            typed,
            ControlResponse::Health(Health {
                ready: true,
                active_operations: 0,
                admitted_operations: 0,
                protocol_version: PROTOCOL_VERSION,
                lifecycle: DaemonLifecycle::Ready,
                accepting_operations: true,
                active_connections: 0,
                connection_limit: DEFAULT_CONNECTION_LIMIT,
                queued_operations: 0,
                running_operations: 0,
                operation_queue_limit: DEFAULT_OPERATION_QUEUE_LIMIT,
                journal_healthy: true,
                catalog_status: HealthStatus::Healthy,
                catalog_schema_version: rootlight_operations::OPERATION_SCHEMA_VERSION,
                generation_status: HealthStatus::NotConfigured,
                adapter_status: HealthStatus::NotConfigured,
                watcher_status: HealthStatus::NotConfigured,
                resource_pressure: ResourcePressure::Unknown,
                endpoint_status: HealthStatus::NotConfigured,
                endpoint_schema_version: 2,
            })
        );
        assert!(matches!(
            wire.response,
            Some(daemon::response_envelope::Response::Health(
                daemon::HealthResponse {
                    ready: true,
                    active_operations: 0,
                    admitted_operations: 0,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn health_tracks_lifecycle_and_connection_pressure() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let state = Arc::new(DaemonState::starting());
        let service = ControlService::with_state(
            journal,
            [7; 16],
            Arc::clone(&state),
            DaemonLimits::default(),
        );

        assert_eq!(service.health().lifecycle, DaemonLifecycle::Starting);
        assert!(!service.health().ready);
        state.connection_started();
        state.set_operation_counts(3, 2, 1);
        state.set_catalog_status(HealthStatus::Healthy);
        state.set_endpoint_status(HealthStatus::NotConfigured);
        state.set_lifecycle(DaemonLifecycle::Ready);
        let health = service.health();
        assert!(health.ready);
        assert_eq!(health.active_connections, 1);
        assert_eq!(health.active_operations, 3);
        assert_eq!(health.queued_operations, 2);
        assert_eq!(health.running_operations, 1);
        state.set_catalog_status(HealthStatus::Failed);
        assert!(!service.health().ready);
        assert!(!service.health().accepting_operations);
        state.set_catalog_status(HealthStatus::Healthy);
        state.set_lifecycle(DaemonLifecycle::Ready);
        state.set_endpoint_status(HealthStatus::Unavailable);
        assert!(!service.health().ready);
        state.connection_finished();
        state.set_lifecycle(DaemonLifecycle::Draining);
        assert!(!service.health().accepting_operations);
        assert!(!service.health().ready);
    }

    #[tokio::test]
    async fn journal_actor_preserves_idempotent_submission() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let submission = OperationSubmission::control_probe(OperationId::from_bytes([2; 16]));

        let first = handle
            .submit(submission)
            .await
            .expect("submission succeeds");
        let second = handle.submit(submission).await.expect("retry succeeds");
        assert!(first.inserted);
        assert!(!second.inserted);
        assert_eq!(first.operation, second.operation);
        actor.join().expect("actor joins");
    }

    #[test]
    fn journal_actor_stop_preempts_buffered_commands() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let (control_tx, control_rx) = mpsc::sync_channel(2);
        let (_normal_tx, normal_rx) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(true));
        let operation = OperationId::from_bytes([10; 16]);
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        control_tx
            .try_send(JournalCommand::Submit {
                submission: OperationSubmission::control_probe(operation),
                deadline_retry: DeadlineRetry::Exact,
                reply,
            })
            .expect("command buffers");

        journal_actor_loop(Arc::clone(&journal), control_rx, normal_rx, stopping);

        assert!(matches!(
            journal.status(operation),
            Err(OperationError::NotFound)
        ));
    }

    #[test]
    fn journal_actor_stop_rejects_new_commands_and_joins_with_full_lane() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 1, 1).expect("actor starts");
        let handle = actor.handle();
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        let command = JournalCommand::Submit {
            submission: OperationSubmission::control_probe(OperationId::from_bytes([11; 16])),
            deadline_retry: DeadlineRetry::Exact,
            reply,
        };
        let senders = handle.senders.lock().expect("sender lock is available");
        let sender = &senders.as_ref().expect("actor is accepting").control;
        let _ = sender.try_send(command);
        drop(senders);

        actor.join().expect("actor joins without an in-band stop");

        let (reply, _receiver) = tokio::sync::oneshot::channel();
        assert!(matches!(
            handle.try_send(JournalLane::Control, JournalCommand::Checkpoint { reply }),
            Err(ServiceError::ChannelClosed)
        ));
    }

    #[test]
    fn full_normal_lane_preserves_worker_authorization_for_retry() {
        let (control, _control_rx) = mpsc::sync_channel(1);
        let (normal, normal_rx) = mpsc::sync_channel(1);
        let handle = JournalActorHandle {
            senders: Arc::new(Mutex::new(Some(JournalSenders { control, normal }))),
            stopping: Arc::new(AtomicBool::new(false)),
        };
        let (checkpoint_reply, _checkpoint_response) = tokio::sync::oneshot::channel();
        handle
            .try_send(
                JournalLane::Normal,
                JournalCommand::Checkpoint {
                    reply: checkpoint_reply,
                },
            )
            .expect("normal lane fills");
        let operation = OperationId::from_bytes([39; 16]);
        let (started, _start_response) = mpsc::sync_channel(1);
        let command = JournalCommand::StartOperation { operation, started };
        let (error, command) = handle
            .try_send_preserving(JournalLane::Normal, command)
            .expect_err("full lane returns authorization");
        assert!(matches!(error, ServiceError::QueueFull));
        assert!(matches!(
            &*command,
            JournalCommand::StartOperation {
                operation: observed,
                ..
            } if *observed == operation
        ));
        assert!(matches!(
            normal_rx.try_recv(),
            Ok(JournalCommand::Checkpoint { .. })
        ));
        assert!(matches!(
            handle.try_send_preserving(JournalLane::Normal, *command),
            Ok(())
        ));
        assert!(matches!(
            normal_rx.try_recv(),
            Ok(JournalCommand::StartOperation {
                operation: observed,
                ..
            }) if observed == operation
        ));
    }

    #[test]
    fn scheduler_permits_release_their_own_counter_stage() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let owner = ClientInstanceId::new([1; 16]).expect("client identity is valid");
        let mut running = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            owner,
            2,
            2,
        )
        .expect("permit reserves");
        running.start();
        let queued = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            owner,
            2,
            2,
        )
        .expect("permit reserves");

        drop(queued);

        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 1);
        assert_eq!(state.queued_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.running_operations.load(Ordering::Acquire), 1);
        assert_eq!(
            client_admissions
                .lock()
                .expect("admission state is available")
                .admitted
                .get(&owner),
            Some(&1)
        );
        drop(running);
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.running_operations.load(Ordering::Acquire), 0);
        assert!(
            client_admissions
                .lock()
                .expect("admission state is available")
                .admitted
                .is_empty()
        );
    }

    #[test]
    fn persisting_permit_releases_worker_occupancy_before_admission() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let owner = ClientInstanceId::new([7; 16]).expect("client identity is valid");
        let mut permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            owner,
            1,
            1,
        )
        .expect("permit reserves");
        permit.start();

        permit.persist(false);

        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 1);
        assert_eq!(state.queued_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.running_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.persisting_operations.load(Ordering::Acquire), 1);
        assert_eq!(
            state.operation_counts(),
            OperationsSummary {
                queued: 0,
                running: 0,
                cancelling: 0,
            }
        );
        assert_eq!(
            client_admissions
                .lock()
                .expect("admission state is available")
                .admitted
                .get(&owner),
            Some(&1)
        );
        permit.finish();
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.cancelling_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.persisting_operations.load(Ordering::Acquire), 0);
        assert!(
            client_admissions
                .lock()
                .expect("admission state is available")
                .admitted
                .is_empty()
        );
    }

    #[test]
    fn cancelling_permit_reports_exact_cleanup_occupancy() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let owner = ClientInstanceId::new([6; 16]).expect("client identity is valid");
        let mut permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            owner,
            1,
            1,
        )
        .expect("permit reserves");
        permit.start();

        permit.persist(true);

        assert_eq!(state.running_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.cancelling_operations.load(Ordering::Acquire), 1);
        assert_eq!(state.persisting_operations.load(Ordering::Acquire), 0);
        assert_eq!(
            state.operation_counts(),
            OperationsSummary {
                queued: 0,
                running: 0,
                cancelling: 1,
            }
        );
        permit.finish();
        assert_eq!(state.cancelling_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
    }

    #[test]
    fn permit_release_survives_a_poisoned_client_admission_ledger() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let owner = ClientInstanceId::new([8; 16]).expect("client identity is valid");
        let permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            owner,
            1,
            1,
        )
        .expect("permit reserves");
        let poisoned = Arc::clone(&client_admissions);
        let _ = thread::spawn(move || {
            let _guard = poisoned.lock().expect("admission state is available");
            panic!("poison admission state");
        })
        .join();

        drop(permit);

        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.queued_operations.load(Ordering::Acquire), 0);
        assert!(
            client_admissions
                .lock()
                .expect_err("admission state remains poisoned")
                .into_inner()
                .admitted
                .is_empty()
        );
    }

    #[test]
    fn client_connection_admissions_isolate_clients_and_remove_empty_buckets() {
        let limits = DaemonLimits::new_with_client_limits(
            4,
            1,
            4,
            4,
            4,
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let admissions = ClientConnectionAdmissions::new(limits);
        let client_a = ClientInstanceId::new([4; 16]).expect("client identity is valid");
        let client_b = ClientInstanceId::new([5; 16]).expect("client identity is valid");
        let first = admissions
            .reserve(client_a)
            .expect("first connection reserves");

        assert!(matches!(
            admissions.reserve(client_a),
            Err(ServiceError::ClientConnectionLimit { limit: 1 })
        ));
        let second_client = admissions
            .reserve(client_b)
            .expect("another client remains admissible");
        drop(first);
        drop(second_client);

        assert!(
            admissions
                .active
                .lock()
                .expect("admission state is available")
                .is_empty()
        );
        let reconnected = admissions
            .reserve(client_a)
            .expect("released client can reconnect");
        drop(reconnected);
        assert!(
            admissions
                .active
                .lock()
                .expect("admission state is available")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn negotiated_connection_quota_isolates_clients_and_releases_on_disconnect() {
        let temporary = private_tempdir();
        let endpoint = endpoint_named(&temporary, "connection-quota");
        let listener =
            Arc::new(AsyncLocalListener::bind(endpoint.clone()).expect("async listener binds"));
        let limits = DaemonLimits::new_with_client_limits(
            3,
            1,
            4,
            4,
            4,
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let service = Arc::new(ControlService::with_state(
            journal,
            [7; 16],
            Arc::new(DaemonState::starting()),
            limits,
        ));
        let client_connections = Arc::new(ClientConnectionAdmissions::new(limits));
        let (submissions, _submission_rx) = tokio::sync::mpsc::channel(4);
        let commands = OrchestratorSenders::new(submissions);

        let first_handler = spawn_connection_handler(
            Arc::clone(&listener),
            Arc::clone(&service),
            actor.handle(),
            commands.clone(),
            Arc::clone(&client_connections),
        );
        let mut first = connect_async(&endpoint)
            .await
            .expect("first client connects");
        write_client_hello_async(
            FrameCodec::default(),
            &mut first,
            &supported_hello_for(vec![7; 16], [4; 16]),
        )
        .await
        .expect("first hello writes");
        let first_hello = read_server_hello_async(FrameCodec::default(), &mut first)
            .await
            .expect("first hello reads");
        assert!(first_hello.error.is_none());

        let rejected_handler = spawn_connection_handler(
            Arc::clone(&listener),
            Arc::clone(&service),
            actor.handle(),
            commands.clone(),
            Arc::clone(&client_connections),
        );
        let mut rejected = connect_async(&endpoint)
            .await
            .expect("second same-client connection opens");
        write_client_hello_async(
            FrameCodec::default(),
            &mut rejected,
            &supported_hello_for(vec![7; 16], [4; 16]),
        )
        .await
        .expect("second hello writes");
        let rejected_hello = read_server_hello_async(FrameCodec::default(), &mut rejected)
            .await
            .expect("quota rejection reads");
        let error = rejected_hello.error.expect("quota error is returned");
        assert_eq!(error.code, common::ErrorCode::ResourceExhausted as i32);
        assert_eq!(error.message, "client connection quota is exhausted");
        assert!(
            rejected_handler
                .await
                .expect("rejected handler joins")
                .is_ok()
        );

        let other_handler = spawn_connection_handler(
            Arc::clone(&listener),
            Arc::clone(&service),
            actor.handle(),
            commands.clone(),
            Arc::clone(&client_connections),
        );
        let mut other = connect_async(&endpoint)
            .await
            .expect("another client connects");
        write_client_hello_async(
            FrameCodec::default(),
            &mut other,
            &supported_hello_for(vec![7; 16], [5; 16]),
        )
        .await
        .expect("other hello writes");
        let other_hello = read_server_hello_async(FrameCodec::default(), &mut other)
            .await
            .expect("other hello reads");
        assert!(other_hello.error.is_none());

        drop(first);
        drop(other);
        assert!(first_handler.await.expect("first handler joins").is_err());
        assert!(other_handler.await.expect("other handler joins").is_err());

        let reconnected_handler = spawn_connection_handler(
            listener,
            service,
            actor.handle(),
            commands,
            Arc::clone(&client_connections),
        );
        let mut reconnected = connect_async(&endpoint)
            .await
            .expect("released client reconnects");
        write_client_hello_async(
            FrameCodec::default(),
            &mut reconnected,
            &supported_hello_for(vec![7; 16], [4; 16]),
        )
        .await
        .expect("reconnected hello writes");
        let reconnected_hello = read_server_hello_async(FrameCodec::default(), &mut reconnected)
            .await
            .expect("reconnected hello reads");
        assert!(reconnected_hello.error.is_none());
        drop(reconnected);
        assert!(
            reconnected_handler
                .await
                .expect("reconnected handler joins")
                .is_err()
        );
        assert!(
            client_connections
                .active
                .lock()
                .expect("admission state is available")
                .is_empty()
        );
        actor.join().expect("actor joins");
    }

    fn spawn_connection_handler(
        listener: Arc<AsyncLocalListener>,
        service: Arc<ControlService>,
        journal: JournalActorHandle,
        commands: OrchestratorSenders,
        client_connections: Arc<ClientConnectionAdmissions>,
    ) -> tokio::task::JoinHandle<Result<(), ServiceError>> {
        tokio::spawn(async move {
            let mut stream = listener
                .accept_timeout(Duration::from_secs(1))
                .await
                .expect("connection accepts");
            handle_connection_async(
                service,
                journal,
                commands,
                client_connections,
                FrameCodec::default(),
                &mut stream,
            )
            .await
        })
    }

    #[test]
    fn client_connection_quota_maps_to_stable_resource_exhaustion() {
        let error = client_connection_limit(2);
        let key = rootlight_error::DetailKey::parse("client_connection_limit")
            .expect("detail key is valid");

        assert_eq!(error.code(), ErrorCode::ResourceExhausted);
        assert!(error.retryable());
        assert_eq!(error.details().get(&key), Some(&PublicValue::Unsigned(2)));
    }

    #[test]
    fn daemon_limits_reject_invalid_client_operation_bounds() {
        assert!(matches!(
            DaemonLimits::new_with_client_limits(
                4,
                0,
                4,
                4,
                4,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            DaemonLimits::new_with_client_limits(
                4,
                5,
                4,
                4,
                4,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            DaemonLimits::new_with_client_operation_limit(
                4,
                4,
                4,
                0,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            DaemonLimits::new_with_client_operation_limit(
                4,
                4,
                4,
                5,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            Err(ServiceError::InvalidLimits)
        ));
    }

    #[test]
    fn paired_clock_preparation_preserves_relative_timeout_precision() {
        let monotonic = tokio::time::Instant::now();
        let prepared = PreparedOperationSubmission::new(
            OperationSubmission::new(
                OperationId::from_bytes([15; 16]),
                OperationKind::ControlProbe,
                PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
                ClientInstanceId::SYSTEM,
                true,
                Some(1_250),
                None,
            )
            .expect("submission is valid"),
            Some(monotonic + Duration::from_millis(250)),
            None,
        )
        .expect("prepared submission is valid");

        assert_eq!(prepared.submission.deadline_unix_ms, Some(1_250));
        assert_eq!(
            prepared.deadline,
            Some(monotonic + Duration::from_millis(250))
        );
    }

    #[test]
    fn paired_clock_sampling_spends_suspension_time_conservatively() {
        let monotonic_before_wall = tokio::time::Instant::now();
        let clock = admission_clock_sample(monotonic_before_wall, Duration::new(1, 250_500_000))
            .expect("clock sample fits");
        assert_eq!(clock.wall_unix_ms, 1_251);

        let relative = PreparedOperationSubmission::control_probe_at(
            OperationId::from_bytes([35; 16]),
            ClientInstanceId::SYSTEM,
            Duration::from_millis(100),
            clock,
        )
        .expect("relative timeout prepares");
        assert_eq!(relative.submission.deadline_unix_ms, Some(1_351));
        assert_eq!(
            relative.deadline,
            Some(monotonic_before_wall + Duration::from_millis(100))
        );
        assert_eq!(
            relative.deadline_retry,
            DeadlineRetry::ReanchoredRelative { timeout_ms: 100 }
        );

        let absolute = monotonic_target(clock, 1_500).expect("absolute deadline fits");
        assert_eq!(absolute, monotonic_before_wall + Duration::from_millis(249));
    }

    #[test]
    fn lease_renewal_boundaries_are_explicitly_unsupported() {
        let operation = OperationId::from_bytes([39; 16]);
        let owner = ClientInstanceId::new([39; 16]).expect("owner is valid");
        let decoded = request_from_wire(
            Some(daemon::request_envelope::Request::OperationLeaseRenew(
                daemon::OperationLeaseRenewRequest {
                    operation: Some(common::OperationId {
                        value: operation.as_bytes().to_vec(),
                    }),
                    lease_expires_unix_ms: 1,
                },
            )),
            owner,
            PROTOCOL_MINOR,
        );
        let Err(error) = decoded else {
            panic!("wire renewal must remain unsupported");
        };
        assert_eq!(error.code(), ErrorCode::UnsupportedCapability);

        let response = service().execute(ControlRequest::OperationLeaseRenew {
            operation,
            owner,
            expiry_unix_ms: 1,
        });
        let ControlResponse::Error(error) = response else {
            panic!("direct renewal must remain unsupported");
        };
        assert_eq!(error.code(), ErrorCode::UnsupportedCapability);
    }

    #[test]
    fn pending_admission_generations_cancel_and_cleanup_independently() {
        let service = service();
        let operation = OperationId::from_bytes([16; 16]);
        let mut first = service
            .register_pending_admission(operation)
            .expect("first admission registers");
        let second = service
            .register_pending_admission(operation)
            .expect("second admission registers");

        assert!(
            service
                .cancel_pending_admission(operation)
                .expect("pending cancellation succeeds")
        );
        assert!(first.cancelled().load(Ordering::Acquire));
        assert!(second.cancelled().load(Ordering::Acquire));

        assert!(
            first
                .handoff_to_durable()
                .expect("pending handoff succeeds")
        );
        {
            let registry = service
                .pending_admissions
                .lock()
                .expect("registry is available");
            assert_eq!(registry.by_operation[&operation].len(), 1);
        }
        drop(second);
        assert!(
            service
                .pending_admissions
                .lock()
                .expect("registry is available")
                .by_operation
                .is_empty()
        );

        let mut handed_off = service
            .register_pending_admission(operation)
            .expect("third admission registers");
        assert!(
            !handed_off
                .handoff_to_durable()
                .expect("uncancelled handoff succeeds")
        );
        assert!(
            !service
                .cancel_pending_admission(operation)
                .expect("post-handoff lookup succeeds")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_timeout_includes_the_journal_wait() {
        let service = service();
        let (control, control_rx) = mpsc::sync_channel(1);
        let (normal, _normal_rx) = mpsc::sync_channel(1);
        let journal = JournalActorHandle {
            senders: Arc::new(Mutex::new(Some(JournalSenders { control, normal }))),
            stopping: Arc::new(AtomicBool::new(false)),
        };
        let (submissions, _submission_rx) = tokio::sync::mpsc::channel(1);
        let commands = OrchestratorSenders::new(submissions);
        let operation = OperationId::from_bytes([38; 16]);
        let envelope = daemon::RequestEnvelope {
            request_id: 1,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(1),
            request: Some(daemon::request_envelope::Request::OperationCancel(
                daemon::OperationCancelRequest {
                    operation: Some(common::OperationId {
                        value: operation.as_bytes().to_vec(),
                    }),
                },
            )),
        };

        let response = tokio::time::timeout(
            Duration::from_millis(100),
            dispatch_async(
                &service,
                &journal,
                &commands,
                envelope,
                ClientInstanceId::new([9; 16]).expect("client identity is valid"),
                PROTOCOL_MINOR,
            ),
        )
        .await
        .expect("dispatch respects its shorter request timeout");
        let Some(daemon::response_envelope::Response::Error(error)) = response.response else {
            panic!("timeout must return a public error");
        };
        assert_eq!(error.code, common::ErrorCode::Busy as i32);
        assert_eq!(error.message, "daemon request timed out");
        assert!(matches!(
            control_rx.try_recv(),
            Ok(JournalCommand::Execute {
                request: ControlRequest::OperationCancel(observed),
                ..
            }) if observed == operation
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_schedule_is_inclusive_and_bounded() {
        let now = tokio::time::Instant::now();
        let mut timers = TimerSchedule::default();
        for seed in 1..=20 {
            timers
                .register(
                    ScheduledTimer {
                        operation: OperationId::from_bytes([seed; 16]),
                        reason: TimerReason::Deadline,
                    },
                    now + Duration::from_millis(100),
                )
                .expect("timer registers");
        }

        assert!(
            timers
                .due(now + Duration::from_millis(99), TIMER_BATCH_LIMIT)
                .is_empty()
        );
        let due = timers.due(now + Duration::from_millis(100), TIMER_BATCH_LIMIT);
        assert_eq!(due.len(), TIMER_BATCH_LIMIT);
    }

    #[tokio::test(start_paused = true)]
    async fn admission_delay_expires_before_worker_start() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator =
            DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), DaemonLimits::default())
                .expect("orchestrator starts");
        let now = tokio::time::Instant::now();
        let operation = OperationId::from_bytes([25; 16]);
        let submission = OperationSubmission::new(
            operation,
            OperationKind::ControlProbe,
            PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            ClientInstanceId::SYSTEM,
            true,
            Some(1_100),
            None,
        )
        .expect("submission is valid");
        let prepared = prepared_at(
            submission,
            AdmissionClockSample {
                wall_unix_ms: 1_000,
                monotonic: now,
            },
        );

        tokio::time::advance(Duration::from_millis(100)).await;
        let observed = orchestrator
            .schedule(prepared)
            .await
            .expect("expired admission persists");

        assert_eq!(observed.state, OperationState::Interrupted);
        assert_eq!(observed.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(state.running_operations.load(Ordering::Acquire), 0);
        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn worker_admission_failure_does_not_persist_queued_work() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator =
            DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), DaemonLimits::default())
                .expect("orchestrator starts");
        orchestrator.workers.close();
        let operation = OperationId::from_bytes([37; 16]);

        assert!(matches!(
            orchestrator
                .schedule(prepared(OperationSubmission::control_probe(operation)))
                .await,
            Err(ServiceError::ChannelClosed)
        ));
        assert!(matches!(
            journal.status(operation),
            Err(OperationError::NotFound)
        ));
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn admission_saturation_preserves_retry_and_conflict_semantics() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::new(
            4,
            4,
            1,
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        let first = OperationSubmission::control_probe(OperationId::from_bytes([17; 16]));
        let running = orchestrator
            .schedule(prepared(first))
            .await
            .expect("first operation schedules");
        assert_eq!(running.state, OperationState::Queued);

        let retried = orchestrator
            .schedule(prepared(first))
            .await
            .expect("identical retry bypasses saturated admission");
        assert_eq!(retried, running);

        let conflict = OperationSubmission {
            plan_hash: PlanHash::from_bytes([9; 32]),
            ..first
        };
        assert!(matches!(
            orchestrator.schedule(prepared(conflict)).await,
            Err(ServiceError::Operations(OperationError::SubmissionConflict))
        ));
        assert!(matches!(
            orchestrator
                .schedule(prepared(OperationSubmission::control_probe(
                    OperationId::from_bytes([18; 16]),
                )))
                .await,
            Err(ServiceError::QueueFull)
        ));

        let completion = orchestrator
            .complete_next()
            .await
            .expect("completion persists")
            .expect("completion exists");
        assert_eq!(completion.state, OperationState::Succeeded);
        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn client_operation_quota_preserves_isolation_retry_and_conflict() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::new_with_client_operation_limit(
            4,
            4,
            3,
            1,
            2,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        let owner_a = ClientInstanceId::new([1; 16]).expect("client identity is valid");
        let owner_b = ClientInstanceId::new([2; 16]).expect("client identity is valid");
        let first = OperationSubmission::new(
            OperationId::from_bytes([19; 16]),
            OperationKind::ControlProbe,
            PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            owner_a,
            true,
            None,
            None,
        )
        .expect("submission is valid");
        let running = orchestrator
            .schedule(prepared(first))
            .await
            .expect("first client operation schedules");

        let retried = orchestrator
            .schedule(prepared(first))
            .await
            .expect("identical retry bypasses client quota");
        assert_eq!(retried, running);
        let conflict = OperationSubmission {
            plan_hash: PlanHash::from_bytes([9; 32]),
            ..first
        };
        assert!(matches!(
            orchestrator.schedule(prepared(conflict)).await,
            Err(ServiceError::Operations(OperationError::SubmissionConflict))
        ));

        let owner_a_second = OperationSubmission::new(
            OperationId::from_bytes([20; 16]),
            OperationKind::ControlProbe,
            PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            owner_a,
            true,
            None,
            None,
        )
        .expect("submission is valid");
        assert!(matches!(
            orchestrator.schedule(prepared(owner_a_second)).await,
            Err(ServiceError::ClientOperationLimit { limit: 1 })
        ));

        let owner_b_submission = OperationSubmission::new(
            OperationId::from_bytes([21; 16]),
            OperationKind::ControlProbe,
            PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
            owner_b,
            true,
            None,
            None,
        )
        .expect("submission is valid");
        let owner_b_running = orchestrator
            .schedule(prepared(owner_b_submission))
            .await
            .expect("another client remains admissible");
        assert_eq!(owner_b_running.owner, owner_b);

        for _ in 0..2 {
            let completed = orchestrator
                .complete_next()
                .await
                .expect("completion persists")
                .expect("completion exists");
            assert_eq!(completed.state, OperationState::Succeeded);
        }
        assert!(orchestrator.is_idle());
        assert!(
            orchestrator
                .client_admissions
                .lock()
                .expect("admission state is available")
                .admitted
                .is_empty()
        );

        let owner_a_reused = orchestrator
            .schedule(prepared(owner_a_second))
            .await
            .expect("released owner quota admits new work");
        assert_eq!(owner_a_reused.owner, owner_a);
        let completed = orchestrator
            .complete_next()
            .await
            .expect("completion persists")
            .expect("completion exists");
        assert_eq!(completed.state, OperationState::Succeeded);
        assert!(orchestrator.is_idle());
        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[test]
    fn default_limits_bind_per_client_admission_to_global_bounds() {
        let limits = DaemonLimits::default();

        assert_eq!(
            limits.client_connection_limit,
            DEFAULT_CLIENT_CONNECTION_LIMIT
        );
        assert_eq!(
            limits.client_operation_limit,
            DEFAULT_CLIENT_OPERATION_LIMIT
        );
        assert_eq!(limits.operation_workers, DEFAULT_OPERATION_WORKERS);
        assert_eq!(limits.operation_workers, 4);
        assert!(limits.client_connection_limit <= limits.connection_limit);
        assert!(limits.client_operation_limit <= limits.operation_queue_limit);
    }

    #[test]
    fn client_operation_quota_maps_to_stable_resource_exhaustion() {
        let error = ServiceError::ClientOperationLimit { limit: 3 }.to_public();
        let key = rootlight_error::DetailKey::parse("client_operation_limit")
            .expect("detail key is valid");

        assert_eq!(error.code(), ErrorCode::ResourceExhausted);
        assert!(error.retryable());
        assert_eq!(error.details().get(&key), Some(&PublicValue::Unsigned(3)));
    }

    #[tokio::test]
    async fn worker_completion_preserves_durable_interruption_and_cancellation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();

        let interrupted = OperationId::from_bytes([12; 16]);
        journal.enqueue(interrupted).expect("operation enqueues");
        journal
            .transition(interrupted, OperationState::Running, None)
            .expect("operation starts");
        journal
            .interrupt_nonterminal(1)
            .expect("operation is interrupted");
        let observed = handle
            .finish_operation(interrupted, None)
            .await
            .expect("stale completion loads durable state");
        assert_eq!(observed.state, OperationState::Interrupted);

        let cancelled = OperationId::from_bytes([13; 16]);
        journal.enqueue(cancelled).expect("operation enqueues");
        let terminal = journal
            .request_cancellation(
                cancelled,
                rootlight_operations::CancellationReason::ClientRequest,
            )
            .expect("queued cancellation commits")
            .operation;
        assert_eq!(terminal.state, OperationState::Cancelled);
        let observed = handle
            .finish_operation(cancelled, None)
            .await
            .expect("stale completion loads durable state");
        assert_eq!(observed.state, OperationState::Cancelled);

        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn worker_deadline_reason_reaches_durable_interruption() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let operation = OperationId::from_bytes([16; 16]);
        journal.enqueue(operation).expect("operation enqueues");
        journal
            .transition(operation, OperationState::Running, None)
            .expect("operation starts");

        let observed = handle
            .interrupt_deadline_receiver(operation)
            .expect("deadline completion queues")
            .await
            .expect("deadline actor responds")
            .expect("deadline completion persists");

        assert_eq!(observed.state, OperationState::Interrupted);
        assert_eq!(observed.recovery_class, RecoveryClass::DeadlineElapsed);
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn synthetic_worker_observes_cancellation_after_execution_starts() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let mut pool = SyntheticWorkerPool::start(1, 1).expect("worker pool starts");
        let operation = OperationId::from_bytes([15; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 1, 1).expect("actor starts");
        journal.enqueue(operation).expect("operation enqueues");
        let cancellation = journal
            .cancellation_token(operation)
            .expect("cancellation token exists");
        let permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            client_admissions,
            ClientInstanceId::SYSTEM,
            1,
            1,
        )
        .expect("permit reserves");
        let (admitted_tx, admitted_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        pool.submit(WorkerJob {
            operation,
            admitted: admitted_rx,
            journal: actor.handle(),
            permit,
            started: Some(started_tx),
        })
        .expect("job submits");
        admitted_tx.send(()).expect("worker admission is durable");
        started_rx.recv().expect("worker starts");
        assert_eq!(
            journal.status(operation).expect("status loads").state,
            OperationState::Running
        );

        assert!(cancellation.cancel(rootlight_operations::CancellationReason::ClientRequest));
        let completion = pool.completion().await.expect("completion arrives");

        assert_eq!(completion.operation, operation);
        assert_eq!(
            completion.cancellation_reason,
            Some(rootlight_operations::CancellationReason::ClientRequest)
        );
        completion.permit.finish();
        pool.join().expect("worker joins");
        actor.join().expect("actor joins");
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn queued_worker_stays_durable_until_a_worker_dequeues_it() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let mut pool = SyntheticWorkerPool::start(1, 2).expect("worker pool starts");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let first = OperationId::from_bytes([40; 16]);
        let second = OperationId::from_bytes([41; 16]);
        let first_cancellation = journal.enqueue(first).expect("first operation enqueues");
        let second_cancellation = journal.enqueue(second).expect("second operation enqueues");

        let reserve = |operation, started| {
            let permit = SchedulerPermit::reserve(
                Arc::clone(&state),
                Arc::clone(&client_admissions),
                ClientInstanceId::SYSTEM,
                2,
                2,
            )
            .expect("permit reserves");
            let (admitted_tx, admitted_rx) = mpsc::sync_channel(1);
            pool.submit(WorkerJob {
                operation,
                admitted: admitted_rx,
                journal: actor.handle(),
                permit,
                started: Some(started),
            })
            .expect("job submits");
            admitted_tx.send(()).expect("worker admission is durable");
        };
        let (first_started_tx, first_started_rx) = mpsc::sync_channel(0);
        let (second_started_tx, second_started_rx) = mpsc::sync_channel(0);
        reserve(first, first_started_tx);
        reserve(second, second_started_tx);

        first_started_rx.recv().expect("first worker starts");
        assert_eq!(
            journal.status(first).expect("first status loads").state,
            OperationState::Running
        );
        assert_eq!(
            journal.status(second).expect("second status loads").state,
            OperationState::Queued
        );

        assert!(first_cancellation.cancel(rootlight_operations::CancellationReason::ClientRequest));
        let first_completion = pool.completion().await.expect("first completion arrives");
        first_completion.permit.finish();
        second_started_rx.recv().expect("second worker starts");
        assert_eq!(
            journal.status(second).expect("second status loads").state,
            OperationState::Running
        );

        assert!(
            second_cancellation.cancel(rootlight_operations::CancellationReason::ClientRequest)
        );
        let second_completion = pool.completion().await.expect("second completion arrives");
        second_completion.permit.finish();
        pool.join().expect("worker joins");
        actor.join().expect("actor joins");
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn orchestrator_runs_synthetic_operation_to_completion() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::new(
            4,
            4,
            4,
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        let (admission, response) = admission(OperationSubmission::control_probe(
            OperationId::from_bytes([3; 16]),
        ));

        let running = orchestrator
            .submit(admission)
            .await
            .expect("operation schedules");
        assert_eq!(running.state, OperationState::Queued);
        assert_eq!(
            response
                .await
                .expect("response arrives")
                .expect("response succeeds"),
            running
        );
        let completed = orchestrator
            .complete_next()
            .await
            .expect("completion persists")
            .expect("completion exists");
        assert_eq!(completed.state, OperationState::Succeeded);
        assert!(orchestrator.is_idle());
        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn shutdown_drains_pending_completion_permits_before_resetting_counts() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::new(
            4,
            4,
            2,
            1,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        let operation = OperationId::from_bytes([14; 16]);
        let (admission, _response) = admission(OperationSubmission::control_probe(operation));
        orchestrator
            .submit(admission)
            .await
            .expect("operation schedules");

        orchestrator
            .shutdown()
            .await
            .expect("orchestrator drains completion");
        drop(orchestrator);

        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.queued_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.running_operations.load(Ordering::Acquire), 0);
        assert_eq!(state.lifecycle(), DaemonLifecycle::Stopped);
        assert_eq!(
            journal.status(operation).expect("operation persists").state,
            OperationState::Interrupted
        );
        actor.join().expect("actor joins");
    }

    #[test]
    fn operation_submission_requires_minor_two_for_stable_timing_and_leases() {
        let owner = ClientInstanceId::new([9; 16]).expect("client identity is valid");
        let request = daemon::OperationSubmitRequest {
            operation: Some(common::OperationId { value: vec![8; 16] }),
            kind: daemon::OperationKind::ControlProbe as i32,
            plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
            detached: false,
            timeout_ms: None,
            deadline_unix_ms: Some(100),
            lease_expires_unix_ms: Some(200),
        };
        let error = operation_submission_from_wire(request.clone(), owner, 1)
            .expect_err("minor one cannot submit attached work");
        assert_eq!(error.code(), ErrorCode::ProtocolMismatch);

        let submission = operation_submission_from_wire(request, owner, 2)
            .expect("minor two accepts attached work");
        assert!(!submission.submission.detached);
        assert_eq!(submission.submission.deadline_unix_ms, Some(100));
        assert_eq!(submission.submission.lease_expires_unix_ms, Some(200));

        let ambiguous = daemon::OperationSubmitRequest {
            operation: Some(common::OperationId { value: vec![8; 16] }),
            kind: daemon::OperationKind::ControlProbe as i32,
            plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
            detached: true,
            timeout_ms: Some(10),
            deadline_unix_ms: Some(100),
            lease_expires_unix_ms: None,
        };
        let error = operation_submission_from_wire(ambiguous, owner, 2)
            .expect_err("relative and absolute deadlines conflict");
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert_eq!(error.message(), "operation deadline is ambiguous");
    }

    #[test]
    fn synchronous_mutation_rejection_preserves_envelope_validation_precedence() {
        let service = service();
        let request =
            daemon::request_envelope::Request::OperationSubmit(daemon::OperationSubmitRequest {
                operation: Some(common::OperationId { value: vec![8; 16] }),
                kind: daemon::OperationKind::ControlProbe as i32,
                plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
                detached: true,
                timeout_ms: None,
                deadline_unix_ms: None,
                lease_expires_unix_ms: None,
            });
        let timed_out = service.dispatch(daemon::RequestEnvelope {
            request_id: 15,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(0),
            request: Some(request.clone()),
        });
        let stale = service.dispatch(daemon::RequestEnvelope {
            request_id: 16,
            instance_nonce: vec![6; 16],
            timeout_ms: Some(1_000),
            request: Some(request),
        });

        assert!(matches!(
            timed_out.response,
            Some(daemon::response_envelope::Response::Error(common::PublicError {
                code,
                ..
            })) if code == common::ErrorCode::InvalidArgument as i32
        ));
        assert!(matches!(
            stale.response,
            Some(daemon::response_envelope::Response::Error(common::PublicError {
                code,
                ..
            })) if code == common::ErrorCode::PermissionDenied as i32
        ));
    }

    #[test]
    fn operation_submission_rejects_unspecified_metadata() {
        let service = service();
        let response = service.dispatch(daemon::RequestEnvelope {
            request_id: 17,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(1_000),
            request: Some(daemon::request_envelope::Request::OperationSubmit(
                daemon::OperationSubmitRequest {
                    operation: Some(common::OperationId { value: vec![8; 16] }),
                    kind: daemon::OperationKind::Unspecified as i32,
                    plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
                    detached: false,
                    timeout_ms: None,
                    deadline_unix_ms: None,
                    lease_expires_unix_ms: None,
                },
            )),
        });

        assert!(matches!(
            response.response,
            Some(daemon::response_envelope::Response::Error(common::PublicError {
                code,
                ..
            })) if code == common::ErrorCode::InvalidArgument as i32
        ));
    }

    #[test]
    fn synchronous_submission_requires_orchestration() {
        let service = service();
        let operation = OperationId::from_bytes([8; 16]);
        let submitted = service.execute(ControlRequest::OperationSubmit(
            OperationSubmission::control_probe(operation),
        ));

        let ControlResponse::Error(error) = submitted else {
            panic!("synchronous submission must be rejected");
        };
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(matches!(
            service.journal.status(operation),
            Err(OperationError::NotFound)
        ));
    }

    #[test]
    fn local_client_round_trip_preserves_public_errors() {
        let temporary = private_tempdir();
        let endpoint = endpoint(&temporary);
        let listener = LocalListener::bind(endpoint.clone()).expect("listener binds");
        let service = Arc::new(service());
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let server_service = Arc::clone(&service);
        let server = thread::spawn(move || {
            ready_tx.send(()).expect("test synchronization succeeds");
            for _ in 0..3 {
                let mut stream = listener
                    .accept_timeout(Duration::from_secs(1))
                    .expect("connection accepts");
                handle_connection(&server_service, FrameCodec::default(), &mut stream)
                    .expect("connection is served");
            }
        });
        ready_rx.recv().expect("server is ready");
        let client = Client::new(endpoint, [7; 16], [9; 16]);

        let health = client.health().expect("health succeeds");
        assert!(health.ready);
        let submitted = OperationId::from_bytes([4; 16]);
        let submission = client
            .operation_submit(submitted)
            .expect_err("synchronous submission is rejected");
        assert_eq!(
            submission
                .as_public_error()
                .expect("public error is retained")
                .code(),
            ErrorCode::InvalidArgument
        );
        let missing = client
            .operation_status(OperationId::from_bytes([9; 16]))
            .expect_err("missing operation fails");
        let public = missing.as_public_error().expect("public error is retained");
        assert_eq!(public.code(), ErrorCode::NotFound);
        assert_eq!(public.message(), "operation was not found");

        server.join().expect("server thread joins");
    }

    #[test]
    fn checked_public_error_conversion_preserves_known_fields() {
        let error = PublicError::builder(ErrorCode::Busy, "operation state is busy")
            .retryable()
            .detail(
                rootlight_error::DetailKey::parse("queue_limit").expect("key is valid"),
                PublicValue::Unsigned(256),
            )
            .next_action(NextAction::Retry)
            .build()
            .expect("public error builds");

        let wire = checked_public_error_to_wire(&error).expect("known variants encode");
        assert_eq!(wire.code, common::ErrorCode::Busy as i32);
        assert!(wire.retryable);
        assert_eq!(wire.details.len(), 1);
        assert_eq!(wire.next_actions.len(), 1);
    }

    #[test]
    fn cancellation_reaches_a_durable_terminal_state() {
        let service = service();
        let operation = OperationId::from_bytes([3; 16]);
        service
            .journal
            .enqueue(operation)
            .expect("operation enqueues");
        service
            .journal
            .transition(operation, OperationState::Running, None)
            .expect("operation starts");
        service
            .journal
            .update_progress(operation, Progress::new(1, 4).expect("progress is valid"))
            .expect("progress advances");

        assert!(matches!(
            service.execute(ControlRequest::OperationCancel(operation)),
            ControlResponse::OperationCancel { accepted: true, .. }
        ));
        let cancelled = service
            .journal
            .transition(operation, OperationState::Cancelled, None)
            .expect("cleanup completes");
        assert_eq!(cancelled.state, OperationState::Cancelled);
    }
}
