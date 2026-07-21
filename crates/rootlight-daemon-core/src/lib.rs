//! Typed daemon control service shared by local IPC and standalone callers.
//!
//! This crate validates protocol inputs, maps durable operation state, enforces
//! instance binding, and keeps health/status/cancel on a control path that does
//! not depend on future CPU-heavy indexing workers.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, PublicValue, SafeLabel};
use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use rootlight_ipc::{
    AsyncLocalStream, FrameCodec, IpcError, LocalStream, read_client_hello,
    read_client_hello_async, read_request, read_request_async, verify_peer,
    wait_for_peer_close_async, write_response, write_response_async, write_server_hello,
    write_server_hello_async,
};
use rootlight_observability::{
    Architecture as ObservabilityArchitecture, ControlMethod,
    DaemonLifecycle as ObservabilityDaemonLifecycle, DiagnosticsQuickSnapshot,
    ErrorCode as ObservabilityErrorCode, HealthSnapshot, OperatingSystem, OperationsSummary,
    ProtocolVersion as ObservabilityProtocolVersion, SpanKind, SupportBundleInput,
    SupportBundleSchema, Telemetry, TelemetryOutcome, TelemetryOutput,
    build_support_bundle_for_schema,
};
use rootlight_operations::{
    Cancellation, CancellationReason, ClientInstanceId, DeadlineRetry, OperationError,
    OperationJournal, OperationKind, OperationRecord, OperationStage, OperationState,
    OperationSubmission, PlanHash, Progress, RecoveryClass, SubmissionOutcome,
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
    "code.locate.v1",
    "diagnostics.quick",
    "health",
    "operation.cancel",
    "operation.lifecycle.v1",
    "operation.status",
    "operation.submit",
    "repository.index.v1",
    "source.read.v1",
    "symbol.explain.v1",
    "support.bundle.v1",
    "support.bundle.v2",
    "support.bundle.v3",
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
/// Hard maximum simultaneous negotiated connection count.
pub const MAX_CONNECTION_LIMIT: u32 = 4_096;
/// Hard maximum capacity of the high-priority control lane.
pub const MAX_CONTROL_QUEUE_LIMIT: usize = 4_096;
/// Hard maximum admitted nonterminal operation count.
pub const MAX_OPERATION_QUEUE_LIMIT: u32 = rootlight_operations::MAX_NONTERMINAL_OPERATIONS;
const MAX_OPERATION_QUEUE_CAPACITY: usize =
    rootlight_operations::MAX_OPERATION_ROWS - rootlight_operations::MAX_OPERATION_HISTORY;
/// Hard maximum synthetic operation worker count.
pub const MAX_OPERATION_WORKERS: usize = 64;
/// Hard maximum server-side request response time.
pub const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Hard maximum retained maintenance interval.
pub const MAX_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
/// Hard maximum graceful drain duration.
pub const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(60);
const WORKER_HANDSHAKE_RETRY_INTERVAL: Duration = Duration::from_millis(1);
const WORKER_HANDSHAKE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SHUTDOWN_INTERRUPT_BATCH: usize = 256;

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
const MAX_WIRE_PUBLIC_ERROR_MESSAGE_BYTES: usize = 1_024;
const MAX_WIRE_PUBLIC_ERROR_DETAILS: usize = 32;
const MAX_WIRE_PUBLIC_ERROR_ACTIONS: usize = 8;

/// Boxed future returned by the daemon-owned first-slice IPC implementation.
pub type FirstSliceIpcFuture =
    Pin<Box<dyn Future<Output = Result<FirstSliceIpcResponse, PublicError>> + Send + 'static>>;

/// Validated protocol request delegated to the daemon application.
#[derive(Debug, Clone)]
pub enum FirstSliceIpcRequest {
    /// Admit and execute the supported bounded repository index plan.
    RepositoryIndex(daemon::RepositoryIndexRequest),
    /// Read or cancel one repository index operation.
    RepositoryOperationStatus(daemon::RepositoryOperationStatusRequest),
    /// Execute one generation-pinned lexical lookup.
    CodeLocate(daemon::CodeLocateRequest),
    /// Explain one or more stable symbols.
    SymbolExplain(daemon::SymbolExplainRequest),
    /// Read exact immutable source references.
    SourceRead(daemon::SourceReadRequest),
    /// List repositories known to this daemon process.
    RepositoryList(daemon::RepositoryListRequest),
    /// Read one repository's active generation status.
    RepositoryStatus(daemon::RepositoryStatusRequest),
    /// Expand typed relation neighborhoods for stable symbols.
    SymbolRelationships(daemon::SymbolRelationshipsRequest),
    /// Trace bounded directed paths between stable symbols.
    FlowTrace(daemon::FlowTraceRequest),
    /// Detect bounded architecture cycles among stable symbols.
    ArchitectureCycles(daemon::ArchitectureCyclesRequest),
    /// Detect bounded dead-code candidates among stable symbols.
    CodeDead(daemon::CodeDeadRequest),
    /// Aggregate a bounded file-granularity architecture overview.
    ArchitectureOverview(daemon::ArchitectureOverviewRequest),
    /// Select bounded relevant tests for a seed set.
    TestsSelect(daemon::TestsSelectRequest),
    /// Map bounded change impact for an explicit change set.
    ChangeImpact(daemon::ChangeImpactRequest),
    /// Build a bounded ordered change plan for explicit targets.
    PlanChange(daemon::PlanChangeRequest),
    /// Compare two revisions or generations for bounded semantic changes.
    HistoryCompare(daemon::HistoryCompareRequest),
    /// Execute a bounded advanced query over a safe typed AST.
    QueryAdvanced(daemon::AdvancedQueryRequest),
}

/// Typed first-slice response returned by the daemon application.
#[derive(Debug)]
pub enum FirstSliceIpcResponse {
    /// Repository index admission and publication state.
    RepositoryIndex(daemon::RepositoryIndexResponse),
    /// Durable repository operation state.
    RepositoryOperationStatus(daemon::RepositoryOperationStatusResponse),
    /// Generation-pinned locate results.
    CodeLocate(daemon::CodeLocateResponse),
    /// Generation-pinned symbol explanations.
    SymbolExplain(daemon::SymbolExplainResponse),
    /// Verified immutable source chunks.
    SourceRead(daemon::SourceReadResponse),
    /// Bounded repository list.
    RepositoryList(daemon::RepositoryListResponse),
    /// One repository status.
    RepositoryStatus(daemon::RepositoryStatusResponse),
    /// Bounded typed relation neighborhoods.
    SymbolRelationships(daemon::SymbolRelationshipsResponse),
    /// Bounded directed paths between stable symbols.
    FlowTrace(daemon::FlowTraceResponse),
    /// Bounded architecture cycles among stable symbols.
    ArchitectureCycles(daemon::ArchitectureCyclesResponse),
    /// Bounded dead-code candidates among stable symbols.
    CodeDead(daemon::CodeDeadResponse),
    /// Bounded file-granularity architecture overview.
    ArchitectureOverview(daemon::ArchitectureOverviewResponse),
    /// Bounded test selection for a seed set.
    TestsSelect(daemon::TestsSelectResponse),
    /// Bounded change impact for an explicit change set.
    ChangeImpact(daemon::ChangeImpactResponse),
    /// Bounded ordered change plan for explicit targets.
    PlanChange(daemon::PlanChangeResponse),
    /// Bounded semantic comparison between two revisions or generations.
    HistoryCompare(daemon::HistoryCompareResponse),
    /// Bounded advanced query result over a safe typed AST.
    QueryAdvanced(daemon::AdvancedQueryResponse),
}

impl FirstSliceIpcResponse {
    fn into_wire(self) -> daemon::response_envelope::Response {
        match self {
            Self::RepositoryIndex(response) => {
                daemon::response_envelope::Response::RepositoryIndex(response)
            }
            Self::RepositoryOperationStatus(response) => {
                daemon::response_envelope::Response::RepositoryOperationStatus(response)
            }
            Self::CodeLocate(response) => daemon::response_envelope::Response::CodeLocate(response),
            Self::SymbolExplain(response) => {
                daemon::response_envelope::Response::SymbolExplain(response)
            }
            Self::SourceRead(response) => daemon::response_envelope::Response::SourceRead(response),
            Self::RepositoryList(response) => {
                daemon::response_envelope::Response::RepositoryList(response)
            }
            Self::RepositoryStatus(response) => {
                daemon::response_envelope::Response::RepositoryStatus(response)
            }
            Self::SymbolRelationships(response) => {
                daemon::response_envelope::Response::SymbolRelationships(response)
            }
            Self::FlowTrace(response) => daemon::response_envelope::Response::FlowTrace(response),
            Self::ArchitectureCycles(response) => {
                daemon::response_envelope::Response::ArchitectureCycles(response)
            }
            Self::CodeDead(response) => daemon::response_envelope::Response::CodeDead(response),
            Self::ArchitectureOverview(response) => {
                daemon::response_envelope::Response::ArchitectureOverview(response)
            }
            Self::TestsSelect(response) => {
                daemon::response_envelope::Response::TestsSelect(response)
            }
            Self::ChangeImpact(response) => {
                daemon::response_envelope::Response::ChangeImpact(response)
            }
            Self::PlanChange(response) => daemon::response_envelope::Response::PlanChange(response),
            Self::HistoryCompare(response) => {
                daemon::response_envelope::Response::HistoryCompare(response)
            }
            Self::QueryAdvanced(response) => {
                daemon::response_envelope::Response::AdvancedQuery(response)
            }
        }
    }
}

/// Authenticated, deadline-bound context for one delegated first-slice request.
#[derive(Debug, Clone)]
pub struct FirstSliceIpcContext {
    /// Authenticated client identity from the negotiated local connection.
    pub client_instance_id: ClientInstanceId,
    /// Protocol minor selected for this local connection.
    pub selected_protocol_minor: u32,
    /// Cooperative cancellation token carrying the bounded request deadline.
    pub cancellation: rootlight_operations::Cancellation,
    /// Absolute monotonic deadline enforced by the daemon transport.
    pub deadline: Instant,
    /// Admission state that linearizes peer cancellation with index publication.
    pub index_admission: Option<FirstSliceAdmission>,
}

const ADMISSION_PENDING: u8 = 0;
const ADMISSION_INSERTED: u8 = 1;
const ADMISSION_CANCELLED_BEFORE_INSERT: u8 = 2;
const ADMISSION_CANCELLED_AFTER_INSERT: u8 = 3;
const ADMISSION_PUBLICATION_CLAIMED: u8 = 4;

/// Process-local state that binds one index request to its publication race.
#[derive(Debug, Clone, Default)]
pub struct FirstSliceAdmission {
    state: Arc<AtomicU8>,
}

impl FirstSliceAdmission {
    /// Records that the handler inserted a new operation for this exact request.
    pub fn mark_inserted(&self) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let next = match observed {
                ADMISSION_PENDING => ADMISSION_INSERTED,
                ADMISSION_CANCELLED_BEFORE_INSERT => ADMISSION_CANCELLED_AFTER_INSERT,
                ADMISSION_INSERTED
                | ADMISSION_CANCELLED_AFTER_INSERT
                | ADMISSION_PUBLICATION_CLAIMED => return,
                _ => return,
            };
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => observed = actual,
            }
        }
    }

    /// Reports whether this exact request inserted the operation it named.
    #[must_use]
    pub fn was_inserted(&self) -> bool {
        matches!(
            self.state.load(Ordering::Acquire),
            ADMISSION_INSERTED | ADMISSION_CANCELLED_AFTER_INSERT | ADMISSION_PUBLICATION_CLAIMED
        )
    }

    /// Prevents publication if peer abandonment or invalid trailing data wins first.
    pub fn cancel_publication(&self) {
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let next = match observed {
                ADMISSION_PENDING => ADMISSION_CANCELLED_BEFORE_INSERT,
                ADMISSION_INSERTED => ADMISSION_CANCELLED_AFTER_INSERT,
                ADMISSION_CANCELLED_BEFORE_INSERT | ADMISSION_CANCELLED_AFTER_INSERT => return,
                ADMISSION_PUBLICATION_CLAIMED => return,
                _ => return,
            };
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => observed = actual,
            }
        }
    }

    fn claim_publication(&self) -> PublicationAdmission {
        match self.state.compare_exchange(
            ADMISSION_INSERTED,
            ADMISSION_PUBLICATION_CLAIMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => PublicationAdmission::Claimed,
            Err(ADMISSION_CANCELLED_AFTER_INSERT) => PublicationAdmission::Cancelled,
            Err(_) => PublicationAdmission::NotInserted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationAdmission {
    Claimed,
    Cancelled,
    NotInserted,
}

/// Narrow application extension for first-slice protocol methods.
pub trait FirstSliceIpcHandler: Send + Sync + 'static {
    /// Executes one already validated first-slice request.
    fn dispatch(
        &self,
        request: FirstSliceIpcRequest,
        context: FirstSliceIpcContext,
    ) -> FirstSliceIpcFuture;
}

/// Source-free default used by control-only embeddings.
#[derive(Debug, Default)]
pub struct UnavailableFirstSliceIpcHandler;

impl FirstSliceIpcHandler for UnavailableFirstSliceIpcHandler {
    fn dispatch(
        &self,
        _request: FirstSliceIpcRequest,
        _context: FirstSliceIpcContext,
    ) -> FirstSliceIpcFuture {
        Box::pin(async { Err(first_slice_unavailable()) })
    }
}

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
    connection_limit: u32,
    /// Maximum simultaneous negotiated connections for one validated client-declared identity.
    client_connection_limit: u32,
    /// Capacity of the high-priority control lane.
    control_queue_limit: usize,
    /// Maximum admitted nonterminal operations.
    operation_queue_limit: u32,
    /// Maximum admitted nonterminal operations for one validated client-declared identity.
    client_operation_limit: u32,
    /// Exact number of synthetic operation workers.
    operation_workers: usize,
    /// Maximum response time accepted from a request envelope.
    request_timeout: Duration,
    /// Retained compatibility interval from the former polling scheduler.
    maintenance_interval: Duration,
    /// Maximum graceful drain duration.
    shutdown_grace: Duration,
}

impl DaemonLimits {
    /// Creates checked daemon resource bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration
    /// falls outside the documented hard bounds.
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
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration
    /// falls outside the documented hard bounds, or when the client operation
    /// limit exceeds the global operation limit.
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
    /// Returns [`ServiceError::InvalidLimits`] when any capacity or duration
    /// falls outside the documented hard bounds, or when a client limit exceeds
    /// its corresponding global limit.
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
            || connection_limit > MAX_CONNECTION_LIMIT
            || client_connection_limit == 0
            || client_connection_limit > connection_limit
            || control_queue_limit == 0
            || control_queue_limit > MAX_CONTROL_QUEUE_LIMIT
            || operation_queue_limit == 0
            || operation_queue_limit > MAX_OPERATION_QUEUE_LIMIT
            || client_operation_limit == 0
            || client_operation_limit > operation_queue_limit
            || operation_workers == 0
            || operation_workers > MAX_OPERATION_WORKERS
            || request_timeout.is_zero()
            || duration_exceeds(request_timeout, MAX_REQUEST_TIMEOUT)
            || maintenance_interval.is_zero()
            || duration_exceeds(maintenance_interval, MAX_MAINTENANCE_INTERVAL)
            || shutdown_grace.is_zero()
            || duration_exceeds(shutdown_grace, MAX_SHUTDOWN_GRACE)
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

    /// Returns the validated global connection limit.
    #[must_use]
    pub const fn connection_limit(&self) -> u32 {
        self.connection_limit
    }

    /// Returns the validated per-client connection limit.
    #[must_use]
    pub const fn client_connection_limit(&self) -> u32 {
        self.client_connection_limit
    }

    /// Returns the validated control-lane capacity.
    #[must_use]
    pub const fn control_queue_limit(&self) -> usize {
        self.control_queue_limit
    }

    /// Returns the validated global nonterminal operation limit.
    #[must_use]
    pub const fn operation_queue_limit(&self) -> u32 {
        self.operation_queue_limit
    }

    /// Returns the validated per-client nonterminal operation limit.
    #[must_use]
    pub const fn client_operation_limit(&self) -> u32 {
        self.client_operation_limit
    }

    /// Returns the validated synthetic worker count.
    #[must_use]
    pub const fn operation_workers(&self) -> usize {
        self.operation_workers
    }

    /// Returns the validated request timeout.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Returns the validated retained maintenance interval.
    #[must_use]
    pub const fn maintenance_interval(&self) -> Duration {
        self.maintenance_interval
    }

    /// Returns the validated graceful shutdown duration.
    #[must_use]
    pub const fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }
}

const fn duration_exceeds(value: Duration, maximum: Duration) -> bool {
    value.as_secs() > maximum.as_secs()
        || value.as_secs() == maximum.as_secs() && value.subsec_nanos() > maximum.subsec_nanos()
}

fn shutdown_interrupt_rounds(maximum_nonterminal: u32) -> Result<usize, ServiceError> {
    let maximum = usize::try_from(maximum_nonterminal).map_err(|_| ServiceError::InvalidLimits)?;
    let rounded = maximum
        .checked_add(
            SHUTDOWN_INTERRUPT_BATCH
                .checked_sub(1)
                .ok_or(ServiceError::InvalidLimits)?,
        )
        .ok_or(ServiceError::InvalidLimits)?;
    let mutation_rounds = rounded / SHUTDOWN_INTERRUPT_BATCH;
    mutation_rounds
        .checked_add(1)
        .ok_or(ServiceError::InvalidLimits)
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
    telemetry: Arc<Telemetry>,
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
        Self::starting_with_telemetry(Arc::new(Telemetry::new(TelemetryOutput::RetainedOnly)))
    }

    /// Creates the initial state with one explicitly configured telemetry recorder.
    #[must_use]
    pub fn starting_with_telemetry(telemetry: Arc<Telemetry>) -> Self {
        telemetry.record_lifecycle(ObservabilityDaemonLifecycle::Starting);
        Self {
            telemetry,
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

    /// Returns the shared bounded telemetry recorder.
    #[must_use]
    pub fn telemetry(&self) -> Arc<Telemetry> {
        Arc::clone(&self.telemetry)
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
        let previous =
            DaemonLifecycle::from_u8(self.lifecycle.swap(lifecycle.as_u8(), Ordering::AcqRel));
        if previous != lifecycle {
            self.telemetry
                .record_lifecycle(observability_daemon_lifecycle(lifecycle));
        }
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

const MUTATION_PENDING: u8 = 0;
const MUTATION_EXECUTING: u8 = 1;
const MUTATION_ABANDONED: u8 = 2;

#[derive(Debug, Clone)]
struct MutationClaim {
    deadline: Instant,
    state: Arc<AtomicU8>,
}

impl MutationClaim {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            state: Arc::new(AtomicU8::new(MUTATION_PENDING)),
        }
    }

    fn begin(&self) -> bool {
        if self
            .state
            .compare_exchange(
                MUTATION_PENDING,
                MUTATION_EXECUTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if Instant::now() >= self.deadline {
            self.state.store(MUTATION_ABANDONED, Ordering::Release);
            return false;
        }
        true
    }

    fn abandon(&self) -> bool {
        self.state
            .compare_exchange(
                MUTATION_PENDING,
                MUTATION_ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

enum JournalCommand {
    Execute {
        request: ControlRequest,
        claim: Option<MutationClaim>,
        reply: tokio::sync::oneshot::Sender<JournalReply>,
    },
    Submit {
        submission: OperationSubmission,
        claim: Option<MutationClaim>,
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
        deadline: WorkerDeadline,
        started: SyncSender<WorkerStart>,
        acknowledged: Receiver<WorkerStartAcknowledgement>,
    },
    ActivateOperation {
        operation: OperationId,
        deadline: Instant,
        claim: Option<MutationClaim>,
        reply: tokio::sync::oneshot::Sender<
            Result<(OperationRecord, rootlight_operations::Cancellation), OperationError>,
        >,
    },
    CompletePublication {
        operation: OperationId,
        admission: Option<FirstSliceAdmission>,
        claim: Option<MutationClaim>,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    FinishOperation {
        operation: OperationId,
        cancellation_reason: Option<rootlight_operations::CancellationReason>,
        claim: Option<MutationClaim>,
        reply: tokio::sync::oneshot::Sender<Result<OperationRecord, OperationError>>,
    },
    FailOperation {
        operation: OperationId,
        error: PublicError,
        claim: Option<MutationClaim>,
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
    #[cfg(test)]
    Block {
        started: SyncSender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    Barrier {
        entered: SyncSender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    DeliverStart {
        operation: OperationId,
        deadline: WorkerDeadline,
        started: SyncSender<WorkerStart>,
        acknowledged: Receiver<WorkerStartAcknowledgement>,
        result: Box<WorkerStart>,
    },
}

#[derive(Debug)]
struct JournalSenders {
    control: SyncSender<JournalCommand>,
    normal: SyncSender<JournalCommand>,
}

#[derive(Debug)]
enum JournalActorState {
    Accepting(JournalSenders),
    Draining,
}

/// Bounded two-lane handle to one journal-owning thread.
#[derive(Debug, Clone)]
pub struct JournalActorHandle {
    state: Arc<Mutex<JournalActorState>>,
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

    /// Executes one mutating control request with abandonment-safe admission.
    ///
    /// # Errors
    ///
    /// Returns a typed timeout only when the actor has not begun the command,
    /// otherwise awaits the already-claimed bounded journal mutation.
    pub async fn control_until(
        &self,
        request: ControlRequest,
        deadline: Instant,
    ) -> Result<ControlResponse, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Control,
            JournalCommand::Execute {
                request,
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
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
                claim: None,
                deadline_retry,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Submits immutable metadata before one absolute lifecycle deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, or journal failure.
    pub async fn submit_until(
        &self,
        submission: OperationSubmission,
        deadline: Instant,
    ) -> Result<SubmissionOutcome, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::Submit {
                submission,
                claim: Some(claim.clone()),
                deadline_retry: DeadlineRetry::Exact,
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
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
    fn start_operation_blocking(
        &self,
        operation: OperationId,
        deadline: &WorkerDeadline,
    ) -> WorkerStart {
        // Capacity one lets each peer install its deadline wait after sending,
        // without making scheduler progress on the other thread a prerequisite.
        let (started, receiver) = mpsc::sync_channel(1);
        let (acknowledged, acknowledgement) = mpsc::sync_channel(1);
        let mut command = JournalCommand::StartOperation {
            operation,
            deadline: deadline.clone(),
            started,
            acknowledged: acknowledgement,
        };
        let mut queue_saturated = false;
        loop {
            let Some(remaining) = deadline.remaining() else {
                return Err(if queue_saturated {
                    ServiceError::QueueFull
                } else {
                    ServiceError::RequestTimedOut
                });
            };
            match self.try_send_preserving(JournalLane::Normal, command) {
                Ok(()) => break,
                Err((ServiceError::QueueFull, returned)) => {
                    queue_saturated = true;
                    command = *returned;
                    thread::sleep(remaining.min(WORKER_HANDSHAKE_RETRY_INTERVAL));
                }
                Err((error, _)) => return Err(error),
            }
        }
        deadline.pause_before_start_receive();
        loop {
            let Some(remaining) = deadline.remaining() else {
                return Err(ServiceError::RequestTimedOut);
            };
            match receiver.recv_timeout(remaining.min(WORKER_HANDSHAKE_POLL_INTERVAL)) {
                Ok(start) => {
                    if !matches!(
                        &start,
                        Ok((record, Some(_))) if record.state == OperationState::Running
                    ) {
                        return start;
                    }
                    deadline.pause_after_start_receipt();
                    let accepted = deadline.remaining().is_some();
                    deadline.pause_before_start_acknowledgement();
                    if !accepted {
                        let _ = acknowledged.try_send(WorkerStartAcknowledgement::Expired);
                        return Err(ServiceError::RequestTimedOut);
                    }
                    let (authorized, authorization) = mpsc::sync_channel(1);
                    if acknowledged
                        .try_send(WorkerStartAcknowledgement::Accepted(authorized))
                        .is_err()
                    {
                        return Err(if deadline.remaining().is_none() {
                            ServiceError::RequestTimedOut
                        } else {
                            ServiceError::ChannelClosed
                        });
                    }
                    return wait_for_worker_start_authorization(start, authorization, deadline);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(if deadline.remaining().is_none() {
                        ServiceError::RequestTimedOut
                    } else {
                        ServiceError::ChannelClosed
                    });
                }
            }
        }
    }

    /// Atomically activates queued first-slice work and returns its cancellation token.
    ///
    /// Keeping both steps inside one actor command prevents control-lane pressure from
    /// leaving durable running work without a first-slice worker owner.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, or journal failure.
    pub async fn activate_operation(
        &self,
        operation: OperationId,
    ) -> Result<(OperationRecord, rootlight_operations::Cancellation), ServiceError> {
        let deadline = Instant::now()
            .checked_add(DEFAULT_REQUEST_TIMEOUT)
            .ok_or(ServiceError::InvalidLimits)?;
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::ActivateOperation {
                operation,
                deadline,
                claim: None,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Activates one queued operation before an absolute lifecycle deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, or journal failure.
    pub async fn activate_operation_until(
        &self,
        operation: OperationId,
        deadline: Instant,
    ) -> Result<(OperationRecord, rootlight_operations::Cancellation), ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::ActivateOperation {
                operation,
                deadline,
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
    }

    /// Atomically persists successful publication and closes cancellation admission.
    ///
    /// A cancellation serialized before this command wins. Success, final
    /// progress, and cleanup are one journal transaction, so no later lifecycle
    /// deadline is needed to terminalize a committed publication.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, cancellation-race, or journal failure.
    pub async fn complete_publication(
        &self,
        operation: OperationId,
    ) -> Result<OperationRecord, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::CompletePublication {
                operation,
                admission: None,
                claim: None,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Commits publication before an absolute lifecycle deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, cancellation-race, or journal
    /// failure.
    pub async fn complete_publication_until(
        &self,
        operation: OperationId,
        deadline: Instant,
    ) -> Result<OperationRecord, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::CompletePublication {
                operation,
                admission: None,
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
    }

    /// Commits publication only if peer cancellation has not linearized first.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, cancellation-race, admission, or
    /// journal failure.
    pub async fn complete_publication_with_admission_until(
        &self,
        operation: OperationId,
        admission: FirstSliceAdmission,
        deadline: Instant,
    ) -> Result<OperationRecord, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::CompletePublication {
                operation,
                admission: Some(admission),
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
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

    /// Persists completion or cancellation before an absolute lifecycle deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, or journal failure.
    pub async fn finish_operation_until(
        &self,
        operation: OperationId,
        cancellation_reason: Option<rootlight_operations::CancellationReason>,
        deadline: Instant,
    ) -> Result<OperationRecord, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::FinishOperation {
                operation,
                cancellation_reason,
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
    }

    /// Persists a checked terminal failure unless cancellation already won.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, actor, journal, or lifecycle failure.
    pub async fn fail_operation(
        &self,
        operation: OperationId,
        error: PublicError,
    ) -> Result<OperationRecord, ServiceError> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::FailOperation {
                operation,
                error,
                claim: None,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }

    /// Persists checked failure before an absolute lifecycle deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed queue, timeout, actor, journal, or lifecycle failure.
    pub async fn fail_operation_until(
        &self,
        operation: OperationId,
        error: PublicError,
        deadline: Instant,
    ) -> Result<OperationRecord, ServiceError> {
        let claim = MutationClaim::new(deadline);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.try_send(
            JournalLane::Normal,
            JournalCommand::FailOperation {
                operation,
                error,
                claim: Some(claim.clone()),
                reply,
            },
        )?;
        await_claimed_mutation(receiver, claim).await
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
                claim: None,
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

    fn begin_drain(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        *state = JournalActorState::Draining;
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
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return Err((ServiceError::ChannelClosed, Box::new(command))),
        };
        let JournalActorState::Accepting(senders) = &*state else {
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
        self.try_send(
            lane,
            JournalCommand::Execute {
                request,
                claim: None,
                reply,
            },
        )?;
        receiver
            .await
            .map_err(|_| ServiceError::ChannelClosed)?
            .map_err(ServiceError::Operations)
    }
}

async fn await_claimed_mutation<T>(
    mut receiver: tokio::sync::oneshot::Receiver<Result<T, OperationError>>,
    claim: MutationClaim,
) -> Result<T, ServiceError> {
    // Dropping a request future is another cancellation boundary. The guard
    // abandons only still-pending work; once the actor claims execution, the
    // compare-exchange fails and the durable mutation runs to its reply.
    let guard = MutationClaimGuard {
        claim: claim.clone(),
    };
    tokio::select! {
        response = &mut receiver => map_claimed_mutation_response(response),
        () = tokio::time::sleep_until(tokio::time::Instant::from_std(guard.claim.deadline)) => {
            if guard.claim.abandon() {
                Err(ServiceError::RequestTimedOut)
            } else {
                map_claimed_mutation_response(receiver.await)
            }
        }
    }
}

fn map_claimed_mutation_response<T>(
    response: Result<Result<T, OperationError>, tokio::sync::oneshot::error::RecvError>,
) -> Result<T, ServiceError> {
    match response {
        Ok(Err(OperationError::MutationTimedOut)) => Err(ServiceError::RequestTimedOut),
        Ok(result) => result.map_err(ServiceError::Operations),
        Err(_) => Err(ServiceError::ChannelClosed),
    }
}

struct MutationClaimGuard {
    claim: MutationClaim,
}

impl Drop for MutationClaimGuard {
    fn drop(&mut self) {
        let _ = self.claim.abandon();
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
    /// Returns [`ServiceError::InvalidLimits`] for a queue capacity outside the
    /// daemon hard bounds, or [`ServiceError::ThreadSpawn`] when the journal
    /// thread cannot start.
    pub fn start(
        journal: Arc<OperationJournal>,
        control_capacity: usize,
        normal_capacity: usize,
    ) -> Result<Self, ServiceError> {
        if control_capacity == 0
            || control_capacity > MAX_CONTROL_QUEUE_LIMIT
            || normal_capacity == 0
            || normal_capacity > MAX_OPERATION_QUEUE_CAPACITY
        {
            return Err(ServiceError::InvalidLimits);
        }
        let (control_tx, control_rx) = mpsc::sync_channel(control_capacity);
        let (normal_tx, normal_rx) = mpsc::sync_channel(normal_capacity);
        let thread = thread::Builder::new()
            .name("rootlight-journal".to_owned())
            .spawn(move || journal_actor_loop(journal, control_rx, normal_rx))
            .map_err(ServiceError::ThreadSpawn)?;
        Ok(Self {
            handle: JournalActorHandle {
                state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                    control: control_tx,
                    normal: normal_tx,
                }))),
            },
            join: Some(thread),
        })
    }

    /// Returns the cloneable bounded actor handle.
    #[must_use]
    pub fn handle(&self) -> JournalActorHandle {
        self.handle.clone()
    }

    /// Closes admission, drains both bounded lanes, and joins the journal thread.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ThreadPanicked`] when the actor panicked.
    pub fn join(mut self) -> Result<(), ServiceError> {
        self.handle.begin_drain();
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.join().map_err(|_| ServiceError::ThreadPanicked)
    }

    /// Stops the actor and awaits its thread within one absolute shutdown deadline.
    ///
    /// The blocking OS-thread join is owned by a detached coordinator. If the
    /// deadline elapses, consuming `self` leaves no join handle for `Drop` to
    /// block on a second time.
    ///
    /// # Errors
    ///
    /// Returns a typed timeout, coordinator-spawn, or actor-panic failure.
    pub async fn join_until(mut self, deadline: tokio::time::Instant) -> Result<(), ServiceError> {
        self.handle.begin_drain();
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        let (completed, completion) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("rootlight-journal-join".to_owned())
            .spawn(move || {
                let result = join.join().map_err(|_| ServiceError::ThreadPanicked);
                let _ = completed.send(result);
            })
            .map_err(ServiceError::ThreadSpawn)?;
        tokio::time::timeout_at(deadline, completion)
            .await
            .map_err(|_| ServiceError::RequestTimedOut)?
            .map_err(|_| ServiceError::ThreadPanicked)?
    }
}

impl Drop for JournalActor {
    fn drop(&mut self) {
        self.handle.begin_drain();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn journal_actor_loop(
    journal: Arc<OperationJournal>,
    control: Receiver<JournalCommand>,
    normal: Receiver<JournalCommand>,
) {
    const CONTROL_BURST: usize = 16;
    let mut control_open = true;
    let mut normal_open = true;
    loop {
        let mut handled = false;
        for _ in 0..CONTROL_BURST {
            if !control_open {
                break;
            }
            match control.try_recv() {
                Ok(command) => {
                    handled = true;
                    if execute_journal_command(&journal, command).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    control_open = false;
                    break;
                }
            }
        }
        if normal_open {
            match normal.try_recv() {
                Ok(command) => {
                    handled = true;
                    if execute_journal_command(&journal, command).is_err() {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => normal_open = false,
            }
        }
        if handled {
            continue;
        }
        if !control_open && !normal_open {
            return;
        }
        if control_open {
            match control.recv_timeout(Duration::from_millis(10)) {
                Ok(command) => {
                    if execute_journal_command(&journal, command).is_err() {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => control_open = false,
            }
        } else if normal_open {
            match normal.recv_timeout(Duration::from_millis(10)) {
                Ok(command) => {
                    if execute_journal_command(&journal, command).is_err() {
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => normal_open = false,
            }
        }
    }
}

fn start_operation_before(
    journal: &OperationJournal,
    operation: OperationId,
    deadline: &WorkerDeadline,
) -> WorkerStart {
    let current = journal
        .status(operation)
        .map_err(ServiceError::Operations)?;
    if current.state.is_terminal() || current.cancellation_requested {
        return Ok((current, None));
    }
    if deadline.remaining().is_none() {
        return Err(ServiceError::RequestTimedOut);
    }
    let cancellation = journal
        .cancellation_token(operation)
        .map_err(ServiceError::Operations)?;
    journal
        .start_execution_before(operation, deadline.expires_at())
        .map(|started| (started, Some(cancellation)))
        .or_else(|error| match error {
            OperationError::StartDeadlineElapsed => Err(error),
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
        .map_err(|error| {
            if matches!(error, OperationError::StartDeadlineElapsed) {
                ServiceError::RequestTimedOut
            } else {
                ServiceError::Operations(error)
            }
        })
}

fn execute_journal_command(
    journal: &OperationJournal,
    command: JournalCommand,
) -> Result<(), OperationError> {
    match command {
        #[cfg(test)]
        JournalCommand::Barrier { entered, release } => {
            if entered.send(()).is_ok() {
                let _ = release.recv();
            }
        }
        #[cfg(test)]
        JournalCommand::DeliverStart {
            operation,
            deadline,
            started,
            acknowledged,
            result,
        } => {
            deliver_worker_start(
                journal,
                operation,
                &deadline,
                started,
                acknowledged,
                *result,
            )?;
        }
        JournalCommand::Execute {
            request,
            claim,
            reply,
        } => {
            let _ = reply.send(execute_claimed(claim.as_ref(), || {
                execute_journal_request(journal, request)
            }));
        }
        JournalCommand::Submit {
            submission,
            claim,
            deadline_retry,
            reply,
        } => {
            let _ = reply.send(execute_claimed(claim.as_ref(), || {
                journal.submit_with_deadline_retry(submission, deadline_retry)
            }));
        }
        JournalCommand::RetryStatus {
            submission,
            deadline_retry,
            reply,
        } => {
            let _ =
                reply.send(journal.retry_status_with_deadline_retry(submission, deadline_retry));
        }
        JournalCommand::StartOperation {
            operation,
            deadline,
            started,
            acknowledged,
        } => {
            let result = start_operation_before(journal, operation, &deadline);
            deliver_worker_start(journal, operation, &deadline, started, acknowledged, result)?;
        }
        JournalCommand::ActivateOperation {
            operation,
            deadline,
            claim,
            reply,
        } => {
            let result = execute_claimed(claim.as_ref(), || {
                journal
                    .start_execution_before(operation, deadline)
                    .and_then(|record| {
                        journal
                            .cancellation_token(operation)
                            .map(|cancellation| (record, cancellation))
                    })
            });
            let _ = reply.send(result);
        }
        JournalCommand::CompletePublication {
            operation,
            admission,
            claim,
            reply,
        } => {
            let result = execute_claimed(claim.as_ref(), || {
                if let Some(admission) = admission {
                    match admission.claim_publication() {
                        PublicationAdmission::Claimed => {}
                        PublicationAdmission::Cancelled => {
                            journal.request_cancellation(
                                operation,
                                CancellationReason::ClientRequest,
                            )?;
                        }
                        PublicationAdmission::NotInserted => {
                            return Err(OperationError::CorruptState);
                        }
                    }
                }
                journal.complete_repository_publication(operation)
            });
            let _ = reply.send(result);
        }
        JournalCommand::FinishOperation {
            operation,
            cancellation_reason,
            claim,
            reply,
        } => {
            let result = execute_claimed(claim.as_ref(), || {
                let current = journal.status(operation);
                current.and_then(|record| {
                    if matches!(
                        record.state,
                        OperationState::Interrupted | OperationState::Cancelled
                    ) {
                        return Ok(record);
                    }
                    if cancellation_reason
                        == Some(rootlight_operations::CancellationReason::DeadlineExceeded)
                    {
                        return journal.interrupt_deadline(operation);
                    }
                    if let Some(reason) = cancellation_reason {
                        match record.state {
                            OperationState::Running => journal
                                .request_cancellation(operation, reason)
                                .map(|outcome| outcome.operation)
                                .and_then(|_| {
                                    journal.update_stage(operation, OperationStage::Cleanup)
                                })
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
                })
            });
            let _ = reply.send(result);
        }
        JournalCommand::FailOperation {
            operation,
            error,
            claim,
            reply,
        } => {
            let result = execute_claimed(claim.as_ref(), || {
                journal.status(operation).and_then(|record| {
                    if record.state.is_terminal() {
                        return Ok(record);
                    }
                    if record.cancellation_requested || record.state == OperationState::Cancelling {
                        journal
                            .update_stage(operation, OperationStage::Cleanup)
                            .or_else(|stage_error| {
                                if matches!(stage_error, OperationError::InvalidStage) {
                                    Ok(record.clone())
                                } else {
                                    Err(stage_error)
                                }
                            })
                            .and_then(|_| {
                                journal.transition(operation, OperationState::Cancelled, None)
                            })
                    } else if record.state == OperationState::Running {
                        let staged = if record.stage == OperationStage::Cleanup {
                            Ok(record)
                        } else {
                            journal.update_stage(operation, OperationStage::Cleanup)
                        };
                        staged.and_then(|_| {
                            journal.transition(operation, OperationState::Failed, Some(&error))
                        })
                    } else {
                        Err(OperationError::InvalidStage)
                    }
                })
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
        #[cfg(test)]
        JournalCommand::Block { started, release } => {
            let _ = started.send(());
            let _ = release.recv();
        }
    }
    Ok(())
}

fn execute_claimed<T>(
    claim: Option<&MutationClaim>,
    execute: impl FnOnce() -> Result<T, OperationError>,
) -> Result<T, OperationError> {
    if claim.is_some_and(|claim| !claim.begin()) {
        Err(OperationError::MutationTimedOut)
    } else {
        execute()
    }
}

fn wait_for_worker_start_authorization(
    start: WorkerStart,
    authorization: Receiver<WorkerStartAuthorization>,
    deadline: &WorkerDeadline,
) -> WorkerStart {
    loop {
        match authorization.try_recv() {
            Ok(WorkerStartAuthorization::Authorized) => return start,
            Ok(WorkerStartAuthorization::Expired) => {
                return Err(ServiceError::RequestTimedOut);
            }
            Err(TryRecvError::Disconnected) => {
                return Err(if deadline.remaining().is_none() {
                    ServiceError::RequestTimedOut
                } else {
                    ServiceError::ChannelClosed
                });
            }
            Err(TryRecvError::Empty) => {}
        }
        let Some(remaining) = deadline.remaining() else {
            return Err(ServiceError::RequestTimedOut);
        };
        match authorization.recv_timeout(remaining.min(WORKER_HANDSHAKE_POLL_INTERVAL)) {
            Ok(WorkerStartAuthorization::Authorized) => return start,
            Ok(WorkerStartAuthorization::Expired) => {
                return Err(ServiceError::RequestTimedOut);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(if deadline.remaining().is_none() {
                    ServiceError::RequestTimedOut
                } else {
                    ServiceError::ChannelClosed
                });
            }
        }
    }
}

fn receive_worker_start_acknowledgement(
    acknowledged: &Receiver<WorkerStartAcknowledgement>,
    deadline: &WorkerDeadline,
) -> Result<WorkerStartAcknowledgement, mpsc::RecvTimeoutError> {
    loop {
        match acknowledged.try_recv() {
            Ok(acknowledgement) => return Ok(acknowledgement),
            Err(TryRecvError::Disconnected) => {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
            Err(TryRecvError::Empty) => {}
        }
        let Some(remaining) = deadline.remaining() else {
            return Err(mpsc::RecvTimeoutError::Timeout);
        };
        match acknowledged.recv_timeout(remaining.min(WORKER_HANDSHAKE_POLL_INTERVAL)) {
            Ok(acknowledgement) => return Ok(acknowledgement),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
        }
    }
}

fn compensate_unacknowledged_worker_start(
    journal: &OperationJournal,
    operation: OperationId,
    deadline: &WorkerDeadline,
) -> Result<(), OperationError> {
    if deadline.remaining().is_none() {
        journal.interrupt_deadline(operation)?;
    } else {
        journal.interrupt_unacknowledged_start(operation)?;
    }
    Ok(())
}

fn deliver_worker_start(
    journal: &OperationJournal,
    operation: OperationId,
    deadline: &WorkerDeadline,
    started: SyncSender<WorkerStart>,
    acknowledged: Receiver<WorkerStartAcknowledgement>,
    result: WorkerStart,
) -> Result<(), OperationError> {
    if matches!(
        &result,
        Err(ServiceError::Operations(
            OperationError::CommittedStartCompensationFailed
        ))
    ) {
        return Err(OperationError::CommittedStartCompensationFailed);
    }
    let durable_start = matches!(
        &result,
        Ok((record, Some(_))) if record.state == OperationState::Running
    );
    if started.try_send(result).is_err() {
        if durable_start {
            compensate_unacknowledged_worker_start(journal, operation, deadline)?;
        }
        return Ok(());
    }
    if !durable_start {
        return Ok(());
    }
    match receive_worker_start_acknowledgement(&acknowledged, deadline) {
        Ok(WorkerStartAcknowledgement::Accepted(authorized)) => {
            // This actor-owned check is the authorization linearization point:
            // durable deadline compensation must precede any worker execution.
            if deadline.remaining().is_none() {
                journal.interrupt_deadline(operation)?;
                let _ = authorized.try_send(WorkerStartAuthorization::Expired);
            } else if authorized
                .try_send(WorkerStartAuthorization::Authorized)
                .is_err()
            {
                compensate_unacknowledged_worker_start(journal, operation, deadline)?;
            }
        }
        Ok(WorkerStartAcknowledgement::Expired) | Err(mpsc::RecvTimeoutError::Timeout) => {
            journal.interrupt_deadline(operation)?;
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            compensate_unacknowledged_worker_start(journal, operation, deadline)?;
        }
    }
    Ok(())
}

fn execute_journal_request(
    journal: &OperationJournal,
    request: ControlRequest,
) -> Result<ControlResponse, OperationError> {
    match request {
        ControlRequest::Health
        | ControlRequest::DiagnosticsQuick
        | ControlRequest::SupportBundle(_) => Ok(ControlResponse::Error(invalid_argument(
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
    /// Generation storage status; not configured by the daemon control plane.
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
    SupportBundle(SupportBundleSchema),
    /// Submit one durable operation for admission.
    OperationSubmit(OperationSubmission),
    /// Read one durable operation status.
    OperationStatus(OperationId),
    /// Legacy lease-renewal request retained only for source compatibility.
    ///
    /// P1 does not support lease renewal. Every daemon and standalone execution
    /// path returns an `UnsupportedCapability` public error for this variant.
    OperationLeaseRenew {
        /// Stable operation identifier from the legacy request payload.
        operation: OperationId,
        /// Authenticated owner retained for legacy payload compatibility.
        owner: ClientInstanceId,
        /// Requested expiry retained for legacy payload compatibility.
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
    /// Legacy response shape retained for source and wire compatibility.
    ///
    /// Current services never produce this variant because P1 lease renewal is
    /// unsupported.
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
            limit: limits.client_connection_limit(),
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
enum WorkerStartAcknowledgement {
    Accepted(SyncSender<WorkerStartAuthorization>),
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStartAuthorization {
    Authorized,
    Expired,
}

#[cfg(test)]
#[derive(Debug)]
struct WorkerStartReceiptHook {
    entered: SyncSender<()>,
    release: Mutex<Receiver<()>>,
}

#[cfg(test)]
impl WorkerStartReceiptHook {
    fn pause(&self) {
        if self.entered.send(()).is_err() {
            return;
        }
        let release = match self.release.lock() {
            Ok(release) => release,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Test-side teardown closes the channel so a failed assertion cannot
        // strand a worker at this coordination point.
        let _ = release.recv();
    }
}

#[cfg(test)]
struct ControlledStartReceive {
    deadline: WorkerDeadline,
    force_expired: Arc<AtomicBool>,
    entered: Receiver<()>,
    release: SyncSender<()>,
}

#[cfg(test)]
struct ControlledStartReceipt {
    deadline: WorkerDeadline,
    force_expired: Arc<AtomicBool>,
    entered: Receiver<()>,
    release: SyncSender<()>,
}

#[cfg(test)]
struct ControlledStartAcknowledgement {
    deadline: WorkerDeadline,
    force_expired: Arc<AtomicBool>,
    entered: Receiver<()>,
    release: SyncSender<()>,
}

/// One monotonic budget shared by admission, actor enqueue, and actor execution.
///
/// Keeping the deadline in the command prevents a start authorization that
/// outlived its worker from transitioning durable state when the actor catches up.
#[derive(Debug, Clone)]
struct WorkerDeadline {
    expires_at: Instant,
    #[cfg(test)]
    forced_expired: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    remaining_checks: Option<Arc<AtomicU32>>,
    #[cfg(test)]
    start_receive_hook: Option<Arc<WorkerStartReceiptHook>>,
    #[cfg(test)]
    start_receipt_hook: Option<Arc<WorkerStartReceiptHook>>,
    #[cfg(test)]
    start_acknowledgement_hook: Option<Arc<WorkerStartReceiptHook>>,
}

impl WorkerDeadline {
    fn from_timeout(timeout: Duration) -> Result<Self, ServiceError> {
        let expires_at = Instant::now()
            .checked_add(timeout)
            .ok_or(ServiceError::InvalidLimits)?;
        Ok(Self {
            expires_at,
            #[cfg(test)]
            forced_expired: None,
            #[cfg(test)]
            remaining_checks: None,
            #[cfg(test)]
            start_receive_hook: None,
            #[cfg(test)]
            start_receipt_hook: None,
            #[cfg(test)]
            start_acknowledgement_hook: None,
        })
    }

    fn remaining(&self) -> Option<Duration> {
        #[cfg(test)]
        if self
            .forced_expired
            .as_ref()
            .is_some_and(|expired| expired.load(Ordering::Acquire))
        {
            return None;
        }
        #[cfg(test)]
        if let Some(remaining_checks) = self.remaining_checks.as_ref() {
            let available = remaining_checks
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |checks| {
                    checks.checked_sub(1)
                })
                .ok()?;
            debug_assert!(available > 0);
        }
        self.expires_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    const fn expires_at(&self) -> Instant {
        self.expires_at
    }

    fn pause_before_start_receive(&self) {
        #[cfg(test)]
        if let Some(hook) = self.start_receive_hook.as_ref() {
            hook.pause();
        }
    }

    fn pause_after_start_receipt(&self) {
        #[cfg(test)]
        if let Some(hook) = self.start_receipt_hook.as_ref() {
            hook.pause();
        }
    }

    fn pause_before_start_acknowledgement(&self) {
        #[cfg(test)]
        if let Some(hook) = self.start_acknowledgement_hook.as_ref() {
            hook.pause();
        }
    }

    #[cfg(test)]
    fn controlled(timeout: Duration) -> Result<(Self, Arc<AtomicBool>), ServiceError> {
        let mut deadline = Self::from_timeout(timeout)?;
        let forced_expired = Arc::new(AtomicBool::new(false));
        deadline.forced_expired = Some(Arc::clone(&forced_expired));
        Ok((deadline, forced_expired))
    }

    #[cfg(test)]
    fn with_remaining_checks(timeout: Duration, checks: u32) -> Result<Self, ServiceError> {
        let mut deadline = Self::from_timeout(timeout)?;
        deadline.remaining_checks = Some(Arc::new(AtomicU32::new(checks)));
        Ok(deadline)
    }

    #[cfg(test)]
    fn controlled_before_start_receive(
        timeout: Duration,
    ) -> Result<ControlledStartReceive, ServiceError> {
        let (mut deadline, forced_expired) = Self::controlled(timeout)?;
        let (entered_sender, entered) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        deadline.start_receive_hook = Some(Arc::new(WorkerStartReceiptHook {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        }));
        Ok(ControlledStartReceive {
            deadline,
            force_expired: forced_expired,
            entered,
            release,
        })
    }

    #[cfg(test)]
    fn controlled_at_start_receipt(
        timeout: Duration,
    ) -> Result<ControlledStartReceipt, ServiceError> {
        let (mut deadline, forced_expired) = Self::controlled(timeout)?;
        let (entered_sender, entered) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        deadline.start_receipt_hook = Some(Arc::new(WorkerStartReceiptHook {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        }));
        Ok(ControlledStartReceipt {
            deadline,
            force_expired: forced_expired,
            entered,
            release,
        })
    }

    #[cfg(test)]
    fn controlled_before_start_acknowledgement(
        timeout: Duration,
    ) -> Result<ControlledStartAcknowledgement, ServiceError> {
        let (mut deadline, forced_expired) = Self::controlled(timeout)?;
        let (entered_sender, entered) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        deadline.start_acknowledgement_hook = Some(Arc::new(WorkerStartReceiptHook {
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        }));
        Ok(ControlledStartAcknowledgement {
            deadline,
            force_expired: forced_expired,
            entered,
            release,
        })
    }
}

#[derive(Debug)]
struct WorkerJob {
    operation: OperationId,
    admitted: Receiver<()>,
    handshake_timeout: Duration,
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
    /// Returns [`ServiceError::InvalidLimits`] for bounds outside the daemon
    /// hard caps or a join-handle allocation failure, and
    /// [`ServiceError::ThreadSpawn`] when a worker cannot start.
    pub fn start(workers: usize, queue_limit: usize) -> Result<Self, ServiceError> {
        if workers == 0
            || workers > MAX_OPERATION_WORKERS
            || queue_limit == 0
            || queue_limit > MAX_OPERATION_QUEUE_CAPACITY
        {
            return Err(ServiceError::InvalidLimits);
        }
        let (sender, receiver) = mpsc::sync_channel(queue_limit);
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let (completion_tx, completions) = tokio::sync::mpsc::channel(queue_limit);
        let mut joins = Vec::new();
        joins
            .try_reserve_exact(workers)
            .map_err(|_| ServiceError::InvalidLimits)?;
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let worker_completion_tx = completion_tx.clone();
            let worker = thread::Builder::new()
                .name(format!("rootlight-worker-{index}"))
                .spawn(move || synthetic_worker_loop(receiver, worker_completion_tx));
            match worker {
                Ok(worker) => joins.push(worker),
                Err(source) => {
                    drop(sender);
                    drop(completion_tx);
                    for worker in joins {
                        let _ = worker.join();
                    }
                    return Err(ServiceError::ThreadSpawn(source));
                }
            }
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

    async fn join_until(&mut self, deadline: tokio::time::Instant) -> Result<(), ServiceError> {
        self.close();
        // Wake workers blocked on a full completion channel before awaiting
        // their OS threads. Buffered completions remain available for permit
        // finalization after the join.
        self.completions.close();
        if self.workers.is_empty() {
            return Ok(());
        }
        let workers = self.workers.drain(..).collect::<Vec<_>>();
        let (completed, completion) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("rootlight-workers-join".to_owned())
            .spawn(move || {
                let mut panicked = false;
                for worker in workers {
                    panicked |= worker.join().is_err();
                }
                let result = if panicked {
                    Err(ServiceError::ThreadPanicked)
                } else {
                    Ok(())
                };
                let _ = completed.send(result);
            })
            .map_err(ServiceError::ThreadSpawn)?;
        tokio::time::timeout_at(deadline, completion)
            .await
            .map_err(|_| ServiceError::RequestTimedOut)?
            .map_err(|_| ServiceError::ThreadPanicked)?
    }
}

impl Drop for SyntheticWorkerPool {
    fn drop(&mut self) {
        self.close();
        // Process shutdown owns a single absolute deadline. Dropping the pool
        // must never start a fresh unbounded wait after that budget expires.
        self.workers.clear();
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
        // Queue capacity bounds residence; the handoff budget starts when a worker owns the job.
        let handshake_deadline = match WorkerDeadline::from_timeout(job.handshake_timeout) {
            Ok(deadline) => deadline,
            Err(error) => {
                if !deliver_worker_completion(
                    &completion,
                    WorkerCompletion {
                        operation: job.operation,
                        start: Err(error),
                        cancellation_reason: None,
                        permit: job.permit,
                    },
                ) {
                    return;
                }
                continue;
            }
        };
        if !wait_for_worker_admission(&job.admitted, &handshake_deadline) {
            continue;
        }
        let start = job
            .journal
            .start_operation_blocking(job.operation, &handshake_deadline);
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
        if !deliver_worker_completion(
            &completion,
            WorkerCompletion {
                operation: job.operation,
                start,
                cancellation_reason,
                permit: job.permit,
            },
        ) {
            return;
        }
    }
}

fn wait_for_worker_admission(admitted: &Receiver<()>, deadline: &WorkerDeadline) -> bool {
    loop {
        let Some(remaining) = deadline.remaining() else {
            return false;
        };
        match admitted.recv_timeout(remaining.min(WORKER_HANDSHAKE_POLL_INTERVAL)) {
            Ok(()) => return true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
}

fn deliver_worker_completion(
    sender: &tokio::sync::mpsc::Sender<WorkerCompletion>,
    completion: WorkerCompletion,
) -> bool {
    match sender.try_send(completion) {
        Ok(()) => true,
        Err(
            tokio::sync::mpsc::error::TrySendError::Closed(_)
            | tokio::sync::mpsc::error::TrySendError::Full(_),
        ) => false,
    }
}

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

    fn take_next_due(
        &mut self,
        now: tokio::time::Instant,
    ) -> Option<(tokio::time::Instant, ScheduledTimer)> {
        let (deadline, timer) = self.by_deadline.first().copied()?;
        if deadline > now {
            return None;
        }
        self.remove(timer.id());
        Some((deadline, timer))
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
    TimerDelivery,
    Completion,
}

#[derive(Debug)]
struct PendingTimerInterrupt {
    scheduled_at: tokio::time::Instant,
    timer: ScheduledTimer,
    delivery_deadline: tokio::time::Instant,
    reply: tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>,
    result: Option<Result<OperationRecord, ServiceError>>,
}

/// Bounded daemon scheduling and monotonic-timer coordinator.
#[derive(Debug)]
pub struct DaemonOrchestrator {
    journal: JournalActorHandle,
    workers: SyntheticWorkerPool,
    pending_completion: Option<PendingWorkerCompletion>,
    pending_timer: Option<PendingTimerInterrupt>,
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
        let queue_limit = usize::try_from(limits.operation_queue_limit())
            .map_err(|_| ServiceError::InvalidLimits)?;
        Ok(Self {
            journal,
            workers: SyntheticWorkerPool::start(limits.operation_workers(), queue_limit)?,
            pending_completion: None,
            pending_timer: None,
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
            self.limits.operation_queue_limit(),
            self.limits.client_operation_limit(),
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
            handshake_timeout: self.limits.request_timeout(),
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
            let observed = self.interrupt_timer_bounded(timer).await?;
            self.timers.remove(timer.id());
            if observed.state.is_terminal() || observed.cancellation_requested {
                self.timers.remove_operation(operation);
                self.active_operations.remove(&operation);
                return Ok(Some(observed));
            }
        }
        Ok(None)
    }

    fn interrupt_timer_receiver(
        &self,
        timer: ScheduledTimer,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<OperationRecord, OperationError>>, ServiceError>
    {
        match timer.reason {
            TimerReason::Deadline => self.journal.interrupt_deadline_receiver(timer.operation),
            TimerReason::Lease {
                expected_expiry_unix_ms,
            } => self
                .journal
                .interrupt_lease_receiver(timer.operation, expected_expiry_unix_ms),
        }
    }

    fn fail_timer_delivery(&self, error: ServiceError) -> ServiceError {
        self.state.set_journal_healthy(false);
        error
    }

    async fn interrupt_timer_bounded(
        &self,
        timer: ScheduledTimer,
    ) -> Result<OperationRecord, ServiceError> {
        let receiver = self
            .interrupt_timer_receiver(timer)
            .map_err(|error| self.fail_timer_delivery(error))?;
        match tokio::time::timeout(self.limits.request_timeout(), receiver).await {
            Ok(Ok(Ok(operation))) => Ok(operation),
            Ok(Ok(Err(error))) => Err(self.fail_timer_delivery(ServiceError::Operations(error))),
            Ok(Err(_)) => Err(self.fail_timer_delivery(ServiceError::ChannelClosed)),
            Err(_) => Err(self.fail_timer_delivery(ServiceError::TimerDeliveryTimedOut)),
        }
    }

    fn start_due_timer_delivery(&mut self, now: tokio::time::Instant) -> Result<(), ServiceError> {
        if self.pending_timer.is_some() {
            return Err(ServiceError::UnexpectedResponse);
        }
        let Some((scheduled_at, timer)) = self.timers.take_next_due(now) else {
            return Ok(());
        };
        let reply = match self.interrupt_timer_receiver(timer) {
            Ok(reply) => reply,
            Err(error) => {
                self.timers
                    .register(timer, scheduled_at)
                    .map_err(|_| self.fail_timer_delivery(ServiceError::UnexpectedResponse))?;
                return Err(self.fail_timer_delivery(error));
            }
        };
        self.pending_timer = Some(PendingTimerInterrupt {
            scheduled_at,
            timer,
            delivery_deadline: now + self.limits.request_timeout(),
            reply,
            result: None,
        });
        Ok(())
    }

    async fn await_pending_timer_delivery(&mut self) -> Result<(), ServiceError> {
        let pending = self
            .pending_timer
            .as_mut()
            .ok_or(ServiceError::UnexpectedResponse)?;
        if pending.result.is_some() {
            return Ok(());
        }
        let result = tokio::select! {
            biased;
            result = &mut pending.reply => match result {
                Ok(Ok(operation)) => Ok(operation),
                Ok(Err(error)) => Err(ServiceError::Operations(error)),
                Err(_) => Err(ServiceError::ChannelClosed),
            },
            () = tokio::time::sleep_until(pending.delivery_deadline) => {
                Err(ServiceError::TimerDeliveryTimedOut)
            }
        };
        pending.result = Some(result);
        Ok(())
    }

    fn process_pending_timer_delivery(&mut self) -> Result<Option<OperationRecord>, ServiceError> {
        let pending = self
            .pending_timer
            .take()
            .ok_or(ServiceError::UnexpectedResponse)?;
        let result = pending.result.ok_or(ServiceError::UnexpectedResponse);
        match result {
            Ok(Ok(operation)) => {
                if operation.state.is_terminal() || operation.cancellation_requested {
                    self.timers.remove_operation(pending.timer.operation);
                    self.active_operations.remove(&pending.timer.operation);
                }
                Ok(Some(operation))
            }
            Ok(Err(error)) | Err(error) => {
                if self
                    .timers
                    .register(pending.timer, pending.scheduled_at)
                    .is_err()
                {
                    return Err(self.fail_timer_delivery(ServiceError::UnexpectedResponse));
                }
                Err(self.fail_timer_delivery(error))
            }
        }
    }

    /// Reports whether no synthetic worker result or durable timer delivery is pending.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending_completion.is_none()
            && self.pending_timer.is_none()
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
        if self.pending_timer.is_some() {
            self.await_pending_timer_delivery().await?;
            return Ok(OrchestratorEvent {
                kind: OrchestratorEventKind::TimerDelivery,
            });
        }
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
                self.start_due_timer_delivery(tokio::time::Instant::now())?;
                Ok(None)
            }
            OrchestratorEventKind::TimerDelivery => self.process_pending_timer_delivery(),
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
        let deadline = tokio::time::Instant::now()
            .checked_add(self.limits.shutdown_grace())
            .ok_or(ServiceError::InvalidLimits)?;
        self.shutdown_until(deadline).await
    }

    /// Stops admission, interrupts remaining work, and checkpoints the journal
    /// within one absolute deadline.
    ///
    /// # Errors
    ///
    /// Returns a typed actor, journal, timer-delivery, timeout, or worker join
    /// failure.
    pub async fn shutdown_until(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), ServiceError> {
        let maximum_rounds =
            shutdown_interrupt_rounds(rootlight_operations::MAX_NONTERMINAL_OPERATIONS)?;
        self.state.set_lifecycle(DaemonLifecycle::Draining);
        self.workers.close();
        let mut failure = None;
        if self.pending_timer.is_some() {
            match tokio::time::timeout_at(deadline, self.await_pending_timer_delivery()).await {
                Ok(Ok(())) => {
                    if let Err(error) = self.process_pending_timer_delivery() {
                        failure = Some(error);
                    }
                }
                Ok(Err(error)) => {
                    failure = Some(error);
                }
                Err(_) => {
                    if let Some(pending) = self.pending_timer.as_mut() {
                        pending.result = Some(Err(ServiceError::RequestTimedOut));
                    }
                    let error = self
                        .process_pending_timer_delivery()
                        .err()
                        .unwrap_or(ServiceError::RequestTimedOut);
                    failure = Some(error);
                }
            }
        }
        let mut interruption_complete = false;
        for _ in 0..maximum_rounds {
            let request_deadline = deadline.min(
                tokio::time::Instant::now()
                    .checked_add(self.limits.request_timeout())
                    .ok_or(ServiceError::InvalidLimits)?,
            );
            let interrupt = tokio::time::timeout_at(
                request_deadline,
                self.journal.interrupt(SHUTDOWN_INTERRUPT_BATCH),
            )
            .await;
            match interrupt {
                Ok(Ok(0)) => {
                    interruption_complete = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                    break;
                }
                Err(_) => {
                    failure.get_or_insert(ServiceError::RequestTimedOut);
                    break;
                }
            }
        }
        if !interruption_complete && failure.is_none() {
            failure = Some(ServiceError::UnexpectedResponse);
        }
        if let Err(error) = self.workers.join_until(deadline).await {
            failure.get_or_insert(error);
        }
        if let Some(completion) = self.pending_completion.take() {
            completion.completion.permit.finish();
        }
        while let Ok(completion) = self.workers.completions.try_recv() {
            completion.permit.finish();
        }
        let checkpoint_deadline = deadline.min(
            tokio::time::Instant::now()
                .checked_add(self.limits.request_timeout())
                .ok_or(ServiceError::InvalidLimits)?,
        );
        match tokio::time::timeout_at(checkpoint_deadline, self.journal.checkpoint()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                failure.get_or_insert(error);
            }
            Err(_) => {
                failure.get_or_insert(ServiceError::RequestTimedOut);
            }
        }
        match failure {
            Some(error) => Err(self.fail_timer_delivery(error)),
            None => {
                self.timers = TimerSchedule::default();
                self.active_operations.clear();
                self.state.set_operation_counts(0, 0, 0);
                self.state.set_lifecycle(DaemonLifecycle::Stopped);
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DiagnosticKind {
    Quick,
    SupportBundle(SupportBundleSchema),
}

enum DiagnosticCommand {
    Execute {
        kind: DiagnosticKind,
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
        kind: DiagnosticKind,
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
                kind,
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
            kind,
            deadline,
            reply,
        } = command;
        if state.stopping.load(Ordering::Acquire) || Instant::now() >= deadline {
            state.busy.store(false, Ordering::Release);
            let _ = reply.send(ControlResponse::Error(request_timed_out()));
            continue;
        }
        let response = match kind {
            DiagnosticKind::Quick => service.diagnostics_quick_until(deadline),
            DiagnosticKind::SupportBundle(schema) => service.support_bundle_until(schema, deadline),
        };
        state.busy.store(false, Ordering::Release);
        let _ = reply.send(response);
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
    #[cfg(test)]
    cancellation_handoff_hook: Option<CancellationHandoffTestHook>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct CancellationHandoffTestHook {
    initial_not_found: Arc<tokio::sync::Barrier>,
    resume: Arc<tokio::sync::Barrier>,
}

#[cfg(test)]
const CANCELLATION_HANDOFF_TEST_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(test)]
impl CancellationHandoffTestHook {
    async fn pause_after_initial_not_found(&self) {
        tokio::time::timeout(
            CANCELLATION_HANDOFF_TEST_TIMEOUT,
            self.initial_not_found.wait(),
        )
        .await
        .expect("cancellation handoff test did not observe the initial NotFound");
        tokio::time::timeout(CANCELLATION_HANDOFF_TEST_TIMEOUT, self.resume.wait())
            .await
            .expect("cancellation handoff test did not resume the dispatch");
    }
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
            #[cfg(test)]
            cancellation_handoff_hook: None,
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
                    .filter(|value| match **value {
                        "diagnostics.quick" | "support.bundle.v1" => selected_minor >= 3,
                        "support.bundle.v2" => selected_minor >= 4,
                        "support.bundle.v3" => selected_minor >= 5,
                        "code.locate.v1"
                        | "repository.index.v1"
                        | "source.read.v1"
                        | "symbol.explain.v1" => selected_minor >= 5,
                        _ => true,
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
            connection_limit: self.limits.connection_limit(),
            queued_operations,
            running_operations,
            operation_queue_limit: self.limits.operation_queue_limit(),
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

    fn support_bundle(&self, schema: SupportBundleSchema, timeout: Duration) -> ControlResponse {
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return ControlResponse::Error(request_timed_out());
        };
        self.support_bundle_until(schema, deadline)
    }

    fn support_bundle_until(
        &self,
        schema: SupportBundleSchema,
        deadline: Instant,
    ) -> ControlResponse {
        let diagnostics = match self.diagnostics_quick_until(deadline) {
            ControlResponse::DiagnosticsQuick(diagnostics) => diagnostics,
            ControlResponse::Error(error) => return ControlResponse::Error(error),
            _ => unreachable!("diagnostics helper returns diagnostics or error"),
        };
        if Instant::now() >= deadline {
            return ControlResponse::Error(request_timed_out());
        }
        let health = self.health();
        let schema_version = match schema {
            SupportBundleSchema::V1 => rootlight_observability::SUPPORT_BUNDLE_SCHEMA_VERSION,
            SupportBundleSchema::V2 => {
                rootlight_observability::PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION
            }
            SupportBundleSchema::V3 => {
                rootlight_observability::CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION
            }
        };
        let input = SupportBundleInput {
            protocol_version: match schema {
                SupportBundleSchema::V1 => ObservabilityProtocolVersion::V1_3,
                SupportBundleSchema::V2 => ObservabilityProtocolVersion::V1_4,
                SupportBundleSchema::V3 => ObservabilityProtocolVersion::V1_5,
            },
            operating_system: observability_operating_system(),
            architecture: observability_architecture(),
            health: health_snapshot(&health),
            diagnostics: diagnostics_snapshot(&diagnostics),
            operations: self.state.operation_counts(),
            telemetry: (schema != SupportBundleSchema::V1).then(|| self.state.telemetry.snapshot()),
        };
        match build_support_bundle_for_schema(&input, schema) {
            Ok(bundle) if Instant::now() < deadline => {
                ControlResponse::SupportBundle(SupportBundle {
                    schema_version,
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
        self.diagnostics_quick_path(path, self.limits.request_timeout())
    }

    /// Executes one typed control request.
    #[must_use]
    pub fn execute(&self, request: ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::Health => ControlResponse::Health(self.health()),
            ControlRequest::DiagnosticsQuick => {
                self.diagnostics_quick(self.limits.request_timeout())
            }
            ControlRequest::SupportBundle(schema) => {
                self.support_bundle(schema, self.limits.request_timeout())
            }
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
    handle_connection_async_with_first_slice(
        service,
        journal,
        commands,
        client_connections,
        Arc::new(UnavailableFirstSliceIpcHandler),
        codec,
        stream,
    )
    .await
}

/// Serves one negotiated request with a daemon-application first-slice extension.
///
/// # Errors
///
/// Returns [`ServiceError`] for transport, queue, timeout, or actor failures.
pub async fn handle_connection_async_with_first_slice(
    service: Arc<ControlService>,
    journal: JournalActorHandle,
    commands: OrchestratorSenders,
    client_connections: Arc<ClientConnectionAdmissions>,
    first_slice: Arc<dyn FirstSliceIpcHandler>,
    codec: FrameCodec,
    stream: &mut AsyncLocalStream,
) -> Result<(), ServiceError> {
    let negotiation = service.state.telemetry.start_span(SpanKind::IpcNegotiation);
    let hello = read_client_hello_async(codec, stream).await?;
    let selected_protocol = match validate_client_hello(&hello, service.instance_nonce) {
        Ok(selected_protocol) => selected_protocol,
        Err(error) => {
            let error_code = observability_error_code(error.code());
            let response = service.server_hello(None, Some(error.as_ref()));
            write_server_hello_async(codec, stream, &response).await?;
            negotiation.finish(TelemetryOutcome::Rejected, Some(error_code));
            return Ok(());
        }
    };
    let client_instance_id = parse_client_instance_id(&hello.client_instance_id)
        .map_err(|_| ServiceError::InvalidNegotiatedClient)?;
    let _client_permit = match client_connections.reserve(client_instance_id) {
        Ok(permit) => permit,
        Err(ServiceError::ClientConnectionLimit { limit }) => {
            let error = client_connection_limit(limit);
            let error_code = observability_error_code(error.code());
            let response = service.server_hello(None, Some(&error));
            write_server_hello_async(codec, stream, &response).await?;
            negotiation.finish(TelemetryOutcome::Rejected, Some(error_code));
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let selected_protocol_minor = selected_protocol.minor;
    let response = service.server_hello(Some(selected_protocol), None);
    write_server_hello_async(codec, stream, &response).await?;
    negotiation.finish(TelemetryOutcome::Succeeded, None);
    let envelope = read_request_async(codec, stream).await?;
    let survives_disconnect = request_survives_client_disconnect(&envelope);
    let transport_cancel = index_transport_cancel_request(&envelope);
    let index_admission = transport_cancel
        .as_ref()
        .map(|cancellation| cancellation.admission.clone());
    let timing = RequestTiming::start(&service, &envelope);
    let cancellation = timing
        .deadline
        .map_or_else(Cancellation::new, Cancellation::with_deadline);
    let dispatch_result = dispatch_while_peer_connected(
        dispatch_async(
            &service,
            &journal,
            &commands,
            first_slice.as_ref(),
            envelope,
            AsyncDispatchContext {
                client_instance_id,
                selected_protocol_minor,
                cancellation: cancellation.clone(),
                timing,
                index_admission: index_admission.clone(),
            },
        ),
        stream,
        &cancellation,
        survives_disconnect,
        index_admission.as_ref(),
    )
    .await;
    let cancel_admitted_operation =
        dispatch_result.is_err() || matches!(dispatch_result, Ok(None)) && !survives_disconnect;
    if cancel_admitted_operation {
        cancel_peer_abandoned_index(
            first_slice.as_ref(),
            transport_cancel,
            client_instance_id,
            selected_protocol_minor,
            timing.deadline,
        )
        .await;
    }
    let response = dispatch_result?;
    if let Some(response) = response {
        write_response_async(codec, stream, &response).await?;
    }
    Ok(())
}

fn request_survives_client_disconnect(envelope: &daemon::RequestEnvelope) -> bool {
    matches!(
        envelope.request.as_ref(),
        Some(daemon::request_envelope::Request::RepositoryIndex(request)) if request.detached
    )
}

fn index_transport_cancel_request(
    envelope: &daemon::RequestEnvelope,
) -> Option<IndexTransportCancellation> {
    let Some(daemon::request_envelope::Request::RepositoryIndex(request)) =
        envelope.request.as_ref()
    else {
        return None;
    };
    Some(IndexTransportCancellation {
        request: FirstSliceIpcRequest::RepositoryOperationStatus(
            daemon::RepositoryOperationStatusRequest {
                schema_version: request.schema_version,
                operation: request.operation.clone(),
                action: daemon::RepositoryOperationAction::RepositoryOperationCancel as i32,
                wait_ms: None,
                after_revision: None,
            },
        ),
        admission: FirstSliceAdmission::default(),
    })
}

struct IndexTransportCancellation {
    request: FirstSliceIpcRequest,
    admission: FirstSliceAdmission,
}

#[derive(Debug, Clone, Copy)]
struct RequestTiming {
    started: Instant,
    deadline: Option<Instant>,
}

struct AsyncDispatchContext {
    client_instance_id: ClientInstanceId,
    selected_protocol_minor: u32,
    cancellation: Cancellation,
    timing: RequestTiming,
    index_admission: Option<FirstSliceAdmission>,
}

impl RequestTiming {
    fn start(service: &ControlService, envelope: &daemon::RequestEnvelope) -> Self {
        let started = Instant::now();
        let timeout =
            envelope
                .timeout_ms
                .map_or(service.limits.request_timeout(), |milliseconds| {
                    Duration::from_millis(u64::from(milliseconds))
                        .min(service.limits.request_timeout())
                });
        Self {
            started,
            deadline: started.checked_add(timeout),
        }
    }
}

async fn cancel_peer_abandoned_index(
    handler: &dyn FirstSliceIpcHandler,
    cancellation: Option<IndexTransportCancellation>,
    client_instance_id: ClientInstanceId,
    selected_protocol_minor: u32,
    deadline: Option<Instant>,
) {
    let (Some(cancellation), Some(deadline)) = (cancellation, deadline) else {
        return;
    };
    if !cancellation.admission.was_inserted() {
        return;
    }
    let context = FirstSliceIpcContext {
        client_instance_id,
        selected_protocol_minor,
        cancellation: Cancellation::with_deadline(deadline),
        deadline,
        index_admission: None,
    };
    // This independent control-lane request shortens cancellation latency.
    // The admission state remains authoritative if this best-effort dispatch
    // misses its deadline or the bounded control lane is unavailable.
    let _ = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        handler.dispatch(cancellation.request, context),
    )
    .await;
}

async fn dispatch_while_peer_connected<F>(
    dispatch: F,
    stream: &mut AsyncLocalStream,
    cancellation: &Cancellation,
    survives_disconnect: bool,
    admission: Option<&FirstSliceAdmission>,
) -> Result<Option<daemon::ResponseEnvelope>, IpcError>
where
    F: Future<Output = daemon::ResponseEnvelope>,
{
    tokio::pin!(dispatch);
    let peer_close = wait_for_peer_close_async(stream);
    tokio::pin!(peer_close);
    tokio::select! {
        // Polling the peer first rejects buffered protocol data even when the
        // handler already has a response. Observing the token before recording
        // client cancellation preserves an elapsed deadline as first reason.
        biased;
        peer = &mut peer_close => {
            match peer {
                Ok(()) => {
                    if !survives_disconnect && cancellation.reason().is_none() {
                        if let Some(admission) = admission {
                            admission.cancel_publication();
                        }
                        let _ = cancellation.cancel(CancellationReason::ClientRequest);
                    }
                    Ok(None)
                }
                Err(error) => {
                    if cancellation.reason().is_none() {
                        if let Some(admission) = admission {
                            admission.cancel_publication();
                        }
                        let _ = cancellation.cancel(CancellationReason::ClientRequest);
                    }
                    Err(error)
                }
            }
        },
        response = &mut dispatch => Ok(Some(response)),
    }
}

async fn dispatch_async(
    service: &ControlService,
    journal: &JournalActorHandle,
    commands: &OrchestratorSenders,
    first_slice: &dyn FirstSliceIpcHandler,
    envelope: daemon::RequestEnvelope,
    context: AsyncDispatchContext,
) -> daemon::ResponseEnvelope {
    let request_id = envelope.request_id;
    let method = control_method_from_wire(envelope.request.as_ref());
    let started = context.timing.started;
    let request_deadline = context.timing.deadline;
    let span = service
        .state
        .telemetry
        .start_span(SpanKind::IpcRequest { method });
    let response = if envelope.timeout_ms == Some(0) {
        daemon::response_envelope::Response::Error(public_error_to_wire(&invalid_argument(
            "daemon request timeout is invalid",
        )))
    } else if !nonce_matches(&envelope.instance_nonce, service.instance_nonce) {
        daemon::response_envelope::Response::Error(public_error_to_wire(&permission_denied(
            "daemon instance nonce does not match",
        )))
    } else if request_deadline.is_none() {
        daemon::response_envelope::Response::Error(public_error_to_wire(&request_timed_out()))
    } else {
        let request_deadline =
            request_deadline.unwrap_or_else(|| unreachable!("deadline was checked above"));
        match first_slice_request_from_wire(envelope.request) {
            Ok(Some(request)) => {
                dispatch_first_slice(
                    first_slice,
                    request,
                    context.client_instance_id,
                    context.selected_protocol_minor,
                    request_deadline,
                    context.cancellation,
                    context.index_admission,
                )
                .await
            }
            Ok(None) => daemon::response_envelope::Response::Error(public_error_to_wire(
                &invalid_argument("daemon request is missing"),
            )),
            Err(request) => {
                match request_from_wire(
                    Some(request),
                    context.client_instance_id,
                    context.selected_protocol_minor,
                ) {
                    Ok(DecodedRequest::Control(ControlRequest::Health)) => {
                        response_to_wire(ControlResponse::Health(service.health()))
                    }
                    Ok(DecodedRequest::Control(ControlRequest::DiagnosticsQuick)) => {
                        run_diagnostic_request(
                            service.clone(),
                            DiagnosticKind::Quick,
                            envelope.timeout_ms,
                        )
                        .await
                    }
                    Ok(DecodedRequest::Control(ControlRequest::SupportBundle(schema))) => {
                        run_diagnostic_request(
                            service.clone(),
                            DiagnosticKind::SupportBundle(schema),
                            envelope.timeout_ms,
                        )
                        .await
                    }
                    Ok(DecodedRequest::Submission(prepared))
                        if !service.state.accepting_operations.load(Ordering::Acquire) =>
                    {
                        response_to_wire(ControlResponse::Error(daemon_not_accepting(
                            prepared.operation(),
                        )))
                    }
                    Ok(DecodedRequest::Submission(prepared)) => {
                        let operation = prepared.operation();
                        let response = async {
                            let pending = service.register_pending_admission(operation)?;
                            let (admission, receiver) =
                                OperationAdmission::registered(prepared, pending);
                            match commands.submissions.try_send(admission) {
                                Ok(()) => {}
                                Err(tokio::sync::mpsc::error::TrySendError::Full(admission)) => {
                                    let (submission, _, _, deadline_retry) =
                                        admission.prepared.into_parts();
                                    return match journal
                                        .retry_status_with_deadline_retry(
                                            submission,
                                            deadline_retry,
                                        )
                                        .await
                                    {
                                        Ok(operation) => {
                                            Ok(ControlResponse::OperationSubmit(operation))
                                        }
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
                        await_journal_response_until(
                            response,
                            request_deadline,
                            service.limits.operation_queue_limit(),
                        )
                        .await
                    }
                    Ok(DecodedRequest::Control(ControlRequest::OperationCancel(operation))) => {
                        let response = async {
                            match journal
                                .control_until(
                                    ControlRequest::OperationCancel(operation),
                                    request_deadline,
                                )
                                .await
                            {
                                Err(ServiceError::Operations(OperationError::NotFound)) => {
                                    #[cfg(test)]
                                    if let Some(hook) = &service.cancellation_handoff_hook {
                                        // The barriers expose both sides of the
                                        // durable-admission handoff without sleeps.
                                        hook.pause_after_initial_not_found().await;
                                    }
                                    let pending = service.cancel_pending_admission(operation)?;
                                    match journal
                                        .control_until(
                                            ControlRequest::OperationCancel(operation),
                                            request_deadline,
                                        )
                                        .await
                                    {
                                        Err(ServiceError::Operations(OperationError::NotFound))
                                            if pending =>
                                        {
                                            Ok(ControlResponse::Error(operation_not_ready(
                                                operation,
                                            )))
                                        }
                                        result => result,
                                    }
                                }
                                result => result,
                            }
                        };
                        await_claimed_journal_response(
                            response,
                            service.limits.operation_queue_limit(),
                        )
                        .await
                    }
                    Ok(DecodedRequest::Control(request)) => {
                        await_journal_response_until(
                            journal.control(request),
                            request_deadline,
                            service.limits.operation_queue_limit(),
                        )
                        .await
                    }
                    Err(error) => {
                        daemon::response_envelope::Response::Error(public_error_to_wire(&error))
                    }
                }
            }
        }
    };
    let (outcome, error_code) = telemetry_outcome_from_wire(&response);
    service
        .state
        .telemetry
        .record_request(method, outcome, started.elapsed(), error_code);
    span.finish(outcome, error_code);
    daemon::ResponseEnvelope {
        request_id,
        response: Some(response),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the unrecognized request is returned by value to the caller"
)]
fn first_slice_request_from_wire(
    request: Option<daemon::request_envelope::Request>,
) -> Result<Option<FirstSliceIpcRequest>, daemon::request_envelope::Request> {
    match request {
        Some(daemon::request_envelope::Request::RepositoryIndex(request)) => {
            Ok(Some(FirstSliceIpcRequest::RepositoryIndex(request)))
        }
        Some(daemon::request_envelope::Request::RepositoryOperationStatus(request)) => Ok(Some(
            FirstSliceIpcRequest::RepositoryOperationStatus(request),
        )),
        Some(daemon::request_envelope::Request::CodeLocate(request)) => {
            Ok(Some(FirstSliceIpcRequest::CodeLocate(request)))
        }
        Some(daemon::request_envelope::Request::SymbolExplain(request)) => {
            Ok(Some(FirstSliceIpcRequest::SymbolExplain(request)))
        }
        Some(daemon::request_envelope::Request::SourceRead(request)) => {
            Ok(Some(FirstSliceIpcRequest::SourceRead(request)))
        }
        Some(daemon::request_envelope::Request::RepositoryList(request)) => {
            Ok(Some(FirstSliceIpcRequest::RepositoryList(request)))
        }
        Some(daemon::request_envelope::Request::RepositoryStatus(request)) => {
            Ok(Some(FirstSliceIpcRequest::RepositoryStatus(request)))
        }
        Some(daemon::request_envelope::Request::SymbolRelationships(request)) => {
            Ok(Some(FirstSliceIpcRequest::SymbolRelationships(request)))
        }
        Some(daemon::request_envelope::Request::FlowTrace(request)) => {
            Ok(Some(FirstSliceIpcRequest::FlowTrace(request)))
        }
        Some(daemon::request_envelope::Request::ArchitectureCycles(request)) => {
            Ok(Some(FirstSliceIpcRequest::ArchitectureCycles(request)))
        }
        Some(daemon::request_envelope::Request::CodeDead(request)) => {
            Ok(Some(FirstSliceIpcRequest::CodeDead(request)))
        }
        Some(daemon::request_envelope::Request::ArchitectureOverview(request)) => {
            Ok(Some(FirstSliceIpcRequest::ArchitectureOverview(request)))
        }
        Some(daemon::request_envelope::Request::TestsSelect(request)) => {
            Ok(Some(FirstSliceIpcRequest::TestsSelect(request)))
        }
        Some(daemon::request_envelope::Request::ChangeImpact(request)) => {
            Ok(Some(FirstSliceIpcRequest::ChangeImpact(request)))
        }
        Some(daemon::request_envelope::Request::PlanChange(request)) => {
            Ok(Some(FirstSliceIpcRequest::PlanChange(request)))
        }
        Some(daemon::request_envelope::Request::HistoryCompare(request)) => {
            Ok(Some(FirstSliceIpcRequest::HistoryCompare(request)))
        }
        Some(daemon::request_envelope::Request::AdvancedQuery(request)) => {
            Ok(Some(FirstSliceIpcRequest::QueryAdvanced(request)))
        }
        Some(request) => Err(request),
        None => Ok(None),
    }
}

async fn dispatch_first_slice(
    handler: &dyn FirstSliceIpcHandler,
    request: FirstSliceIpcRequest,
    client_instance_id: ClientInstanceId,
    selected_protocol_minor: u32,
    deadline: Instant,
    cancellation: Cancellation,
    index_admission: Option<FirstSliceAdmission>,
) -> daemon::response_envelope::Response {
    if selected_protocol_minor < 5 {
        return daemon::response_envelope::Response::Error(public_error_to_wire(
            &protocol_mismatch("first-slice requests need protocol minor five"),
        ));
    }
    if let Err(error) = validate_first_slice_request(&request) {
        return daemon::response_envelope::Response::Error(public_error_to_wire(error.as_ref()));
    }
    if !cancellation.has_deadline()
        && cancellation.extend_deadline(deadline).is_err()
        && cancellation.reason().is_none()
    {
        return daemon::response_envelope::Response::Error(public_error_to_wire(&internal_error()));
    }
    let context = FirstSliceIpcContext {
        client_instance_id,
        selected_protocol_minor,
        cancellation: cancellation.clone(),
        deadline,
        index_admission,
    };
    // Retain the already-bounded request so the daemon can reject an internal
    // handler response that is well typed but belongs to another identity.
    let correlation_request = request.clone();
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        handler.dispatch(request, context),
    )
    .await
    {
        Ok(Ok(response)) => correlated_first_slice_response(&correlation_request, response),
        Ok(Err(error)) => daemon::response_envelope::Response::Error(public_error_to_wire(&error)),
        Err(_) => {
            let _ = cancellation.cancel(CancellationReason::DeadlineExceeded);
            daemon::response_envelope::Response::Error(public_error_to_wire(&request_timed_out()))
        }
    }
}

fn correlated_first_slice_response(
    request: &FirstSliceIpcRequest,
    response: FirstSliceIpcResponse,
) -> daemon::response_envelope::Response {
    if first_slice_response_correlates(request, &response) {
        response.into_wire()
    } else {
        daemon::response_envelope::Response::Error(public_error_to_wire(&internal_error()))
    }
}

fn first_slice_response_correlates(
    request: &FirstSliceIpcRequest,
    response: &FirstSliceIpcResponse,
) -> bool {
    match (request, response) {
        (
            FirstSliceIpcRequest::RepositoryIndex(request),
            FirstSliceIpcResponse::RepositoryIndex(response),
        ) => {
            first_slice_schema_matches(response.schema_version.as_ref())
                && wire_id_has_len(response.repository.as_ref().map(|id| &id.value), 16)
                && wire_id_equals(
                    response.operation.as_ref().map(|id| &id.value),
                    request.operation.as_ref().map(|id| &id.value),
                )
                && response.state == daemon::OperationState::Succeeded as i32
                && optional_wire_id_has_len(
                    response.parent_generation.as_ref().map(|id| &id.value),
                    20,
                )
                && wire_id_has_len(
                    response.published_generation.as_ref().map(|id| &id.value),
                    20,
                )
                && !wire_id_equals(
                    response.parent_generation.as_ref().map(|id| &id.value),
                    response.published_generation.as_ref().map(|id| &id.value),
                )
                && response.indexed_files <= response.discovered_inputs
        }
        (
            FirstSliceIpcRequest::RepositoryOperationStatus(request),
            FirstSliceIpcResponse::RepositoryOperationStatus(response),
        ) => {
            let Some(operation) = response.operation.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && wire_id_equals(
                    operation.operation.as_ref().map(|id| &id.value),
                    request.operation.as_ref().map(|id| &id.value),
                )
                && valid_repository_operation_status(operation)
                && match daemon::OperationState::try_from(operation.state) {
                    Ok(daemon::OperationState::Succeeded) => {
                        wire_id_has_len(
                            response.published_generation.as_ref().map(|id| &id.value),
                            20,
                        ) && response.retry_after_ms.is_none()
                    }
                    Ok(
                        daemon::OperationState::Queued
                        | daemon::OperationState::Running
                        | daemon::OperationState::Cancelling,
                    ) => response.published_generation.is_none(),
                    Ok(
                        daemon::OperationState::Failed
                        | daemon::OperationState::Interrupted
                        | daemon::OperationState::Cancelled,
                    ) => {
                        response.published_generation.is_none() && response.retry_after_ms.is_none()
                    }
                    Ok(daemon::OperationState::Unspecified) | Err(_) => false,
                }
        }
        (
            FirstSliceIpcRequest::CodeLocate(request),
            FirstSliceIpcResponse::CodeLocate(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && response.hits.len()
                    <= usize::try_from(request.maximum_results).unwrap_or(usize::MAX)
                && u64::try_from(response.hits.len()).is_ok_and(|returned_results| {
                    response.matched_candidates >= returned_results
                        && (response.truncated || response.matched_candidates == returned_results)
                        && context
                            .usage
                            .as_ref()
                            .is_some_and(|usage| usage.results >= returned_results)
                })
                && response.hits.iter().all(|hit| {
                    wire_id_has_len(hit.symbol.as_ref().map(|id| &id.value), 20)
                        && wire_id_has_len(hit.file.as_ref().map(|id| &id.value), 20)
                        && valid_analysis_tier(hit.tier)
                        && hit.score <= 1_000
                        && hit.source.as_ref().is_none_or(|source| {
                            source_ref_correlates(source, context)
                                && wire_id_equals(
                                    source.file.as_ref().map(|id| &id.value),
                                    hit.file.as_ref().map(|id| &id.value),
                                )
                        })
                })
        }
        (
            FirstSliceIpcRequest::SymbolExplain(request),
            FirstSliceIpcResponse::SymbolExplain(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && response.symbols.len() + response.unresolved_symbols.len()
                    <= request.symbols.len()
                && (response.truncated
                    || response.symbols.len() + response.unresolved_symbols.len()
                        == request.symbols.len())
                && wire_ids_form_subsequence(
                    response
                        .symbols
                        .iter()
                        .filter_map(|explanation| explanation.symbol.as_ref()),
                    &request.symbols,
                )
                && wire_ids_form_subsequence(response.unresolved_symbols.iter(), &request.symbols)
                && response
                    .symbols
                    .iter()
                    .enumerate()
                    .all(|(index, explanation)| {
                        explanation.confidence <= 1_000
                            && wire_id_has_len(explanation.symbol.as_ref().map(|id| &id.value), 20)
                            && explanation.definition.as_ref().is_some_and(|definition| {
                                source_ref_correlates(definition, context)
                            })
                            && response.symbols[..index].iter().all(|prior| {
                                !wire_id_equals(
                                    prior.symbol.as_ref().map(|id| &id.value),
                                    explanation.symbol.as_ref().map(|id| &id.value),
                                )
                            })
                            && request.symbols.iter().any(|requested| {
                                wire_id_equals(
                                    explanation.symbol.as_ref().map(|id| &id.value),
                                    Some(&requested.value),
                                )
                            })
                    })
                && response
                    .unresolved_symbols
                    .iter()
                    .enumerate()
                    .all(|(index, unresolved)| {
                        wire_id_has_len(Some(&unresolved.value), 20)
                            && response.unresolved_symbols[..index]
                                .iter()
                                .all(|prior| prior.value != unresolved.value)
                            && response.symbols.iter().all(|resolved| {
                                !wire_id_equals(
                                    resolved.symbol.as_ref().map(|id| &id.value),
                                    Some(&unresolved.value),
                                )
                            })
                            && request
                                .symbols
                                .iter()
                                .any(|requested| requested.value == unresolved.value)
                    })
        }
        (
            FirstSliceIpcRequest::SourceRead(request),
            FirstSliceIpcResponse::SourceRead(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && response.chunks.len() <= request.references.len()
                && (response.truncated || response.chunks.len() == request.references.len())
                && response
                    .chunks
                    .iter()
                    .zip(&request.references)
                    .all(|(chunk, requested)| {
                        let Some(source) = chunk.source.as_ref() else {
                            return false;
                        };
                        source_ref_correlates(source, context)
                            && source == requested
                            && chunk.start_byte <= source.start_byte
                            && chunk.end_byte >= source.end_byte
                            && chunk.start_line > 0
                            && chunk.start_line <= chunk.end_line
                            && source
                                .start_line
                                .is_none_or(|line| chunk.start_line <= line)
                            && source.end_line.is_none_or(|line| chunk.end_line >= line)
                            && wire_id_equals(
                                chunk.content_hash.as_ref().map(|hash| &hash.value),
                                source.content_hash.as_ref().map(|hash| &hash.value),
                            )
                            && u64::try_from(chunk.content.len()).ok()
                                == chunk.end_byte.checked_sub(chunk.start_byte)
                    })
                && response.chunks.iter().try_fold(0_u64, |total, chunk| {
                    total.checked_add(u64::try_from(chunk.content.len()).ok()?)
                }) == Some(response.total_source_bytes)
                && context.usage.as_ref().is_some_and(|usage| {
                    usage.source_bytes == response.total_source_bytes
                        && Some(usage.results) == u64::try_from(response.chunks.len()).ok()
                })
        }
        (
            FirstSliceIpcRequest::RepositoryList(request),
            FirstSliceIpcResponse::RepositoryList(response),
        ) => {
            response.repositories.len()
                <= usize::try_from(request.max_results.unwrap_or(u32::MAX)).unwrap_or(usize::MAX)
                && response.repositories.iter().all(|entry| {
                    wire_id_has_len(entry.repository.as_ref().map(|id| &id.value), 16)
                        && wire_id_has_len(entry.active_generation.as_ref().map(|id| &id.value), 20)
                        && !entry.languages.is_empty()
                        && !entry.structural_freshness.is_empty()
                        && !entry.semantic_freshness.is_empty()
                        && !entry.state.is_empty()
                })
        }
        (
            FirstSliceIpcRequest::RepositoryStatus(request),
            FirstSliceIpcResponse::RepositoryStatus(response),
        ) => {
            wire_id_equals(
                response.repository.as_ref().map(|id| &id.value),
                request.repository.as_ref().map(|id| &id.value),
            ) && wire_id_has_len(response.active_generation.as_ref().map(|id| &id.value), 20)
                && optional_wire_id_has_len(
                    response.parent_generation.as_ref().map(|id| &id.value),
                    20,
                )
                && !wire_id_equals(
                    response.parent_generation.as_ref().map(|id| &id.value),
                    response.active_generation.as_ref().map(|id| &id.value),
                )
                && !response.structural_freshness.is_empty()
                && !response.semantic_freshness.is_empty()
                && !response.state.is_empty()
                && response.coverage.iter().all(|entry| {
                    !entry.language.is_empty()
                        && !entry.tier.is_empty()
                        && !entry.status.is_empty()
                        && entry.indexed_files <= entry.discovered_files
                })
        }
        (
            FirstSliceIpcRequest::SymbolRelationships(request),
            FirstSliceIpcResponse::SymbolRelationships(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let returned_items = response
                .groups
                .iter()
                .try_fold(0_u64, |total, group| {
                    total.checked_add(u64::try_from(group.items.len()).ok()?)
                })
                .unwrap_or(u64::MAX);
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && !request.seeds.is_empty()
                && request
                    .seeds
                    .iter()
                    .all(|seed| wire_id_has_len(Some(&seed.value), 20))
                && response.returned_edges == returned_items
                && response.returned_edges <= response.total_edges
                && response.exact != response.truncated
                && (response.truncated || response.returned_edges == response.total_edges)
                && response.groups.iter().all(|group| {
                    wire_id_has_len(group.seed.as_ref().map(|id| &id.value), 20)
                        && !group.relation.is_empty()
                        && !group.direction.is_empty()
                        && u64::try_from(group.items.len())
                            .is_ok_and(|returned| group.total_count >= returned)
                        && group.items.iter().all(|item| {
                            wire_id_has_len(item.symbol.as_ref().map(|id| &id.value), 20)
                                && item.confidence <= 1_000
                                && item
                                    .source_refs
                                    .iter()
                                    .all(|source| source_ref_correlates(source, context))
                        })
                })
        }
        (FirstSliceIpcRequest::FlowTrace(request), FirstSliceIpcResponse::FlowTrace(response)) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(frontier) = response.frontier.as_ref() else {
                return false;
            };
            let Some(projection) = response.projection.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && request
                    .from
                    .as_ref()
                    .is_some_and(|from| wire_id_has_len(Some(&from.value), 20))
                && !request.relations.is_empty()
                && request.relations.len() <= 16
                && projection.min_confidence <= 1_000
                && projection.min_confidence == request.min_confidence.unwrap_or(0)
                && !projection.relations.is_empty()
                && projection.relations.len() <= 16
                && frontier.reached_nodes >= 1
                && frontier.unresolved_boundaries <= frontier.reached_nodes
                && response.paths.len() <= 100
                && response.paths.iter().all(|path| {
                    (2..=9).contains(&path.nodes.len())
                        && (1..=8).contains(&path.edges.len())
                        && path.nodes.len() == path.edges.len() + 1
                        && path.confidence <= 1_000
                        && path
                            .nodes
                            .iter()
                            .all(|node| wire_id_has_len(Some(&node.value), 20))
                        && path.nodes.first().is_some_and(|node| {
                            request
                                .from
                                .as_ref()
                                .is_some_and(|from| from.value == node.value)
                        })
                        && request.to.as_ref().is_none_or(|to| {
                            path.nodes.last().is_some_and(|node| node.value == to.value)
                        })
                        && path.edges.iter().all(|edge| {
                            !edge.kind.is_empty()
                                && edge.kind.len() <= 32
                                && edge.confidence <= 1_000
                                && edge
                                    .source_refs
                                    .iter()
                                    .all(|source| source_ref_correlates(source, context))
                        })
                })
        }
        (
            FirstSliceIpcRequest::ArchitectureCycles(request),
            FirstSliceIpcResponse::ArchitectureCycles(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(projection) = response.projection.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && !request.relations.is_empty()
                && request.relations.len() <= 8
                && projection.min_confidence <= 1_000
                && !projection.relations.is_empty()
                && projection.relations.len() <= 8
                && response.components.len() <= 200
                && response.cycles.len() <= 200
                && response.break_candidates.len() <= 200
                && response.components.iter().all(|component| {
                    component.size >= 1
                        && usize::try_from(component.size)
                            .is_ok_and(|size| component.members.len() == size)
                        && component
                            .members
                            .iter()
                            .all(|member| wire_id_has_len(Some(&member.value), 20))
                })
                && response.cycles.iter().all(|cycle| {
                    cycle.nodes.len() >= 2
                        && cycle.confidence <= 1_000
                        && cycle.nodes.first() == cycle.nodes.last()
                        && cycle
                            .nodes
                            .iter()
                            .all(|node| wire_id_has_len(Some(&node.value), 20))
                        && cycle.edge_evidence.len() <= 64
                        && cycle
                            .edge_evidence
                            .iter()
                            .all(|source| source_ref_correlates(source, context))
                })
                && response.break_candidates.iter().all(|candidate| {
                    wire_id_has_len(candidate.from.as_ref().map(|id| &id.value), 20)
                        && wire_id_has_len(candidate.to.as_ref().map(|id| &id.value), 20)
                        && !candidate.kind.is_empty()
                        && candidate.kind.len() <= 32
                        && candidate.break_cost <= 1_000
                        && candidate.source_refs.len() <= 8
                        && candidate
                            .source_refs
                            .iter()
                            .all(|source| source_ref_correlates(source, context))
                })
        }
        (FirstSliceIpcRequest::CodeDead(request), FirstSliceIpcResponse::CodeDead(response)) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(entry_points) = response.entry_points.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && request
                    .entry_point_policy
                    .as_ref()
                    .is_none_or(|policy| !policy.is_empty() && policy.len() <= 32)
                && request
                    .min_confidence
                    .is_none_or(|confidence| confidence <= 1_000)
                && request
                    .max_candidates
                    .is_none_or(|max| (1..=500).contains(&max))
                && !entry_points.policy.is_empty()
                && entry_points.policy.len() <= 32
                && response.candidates.len() <= 500
                && response.blind_spots.len() <= 32
                && response.false_positive_controls.len() <= 32
                && response.candidates.iter().all(|candidate| {
                    wire_id_has_len(candidate.symbol_id.as_ref().map(|id| &id.value), 20)
                        && !candidate.classification.is_empty()
                        && candidate.classification.len() <= 32
                        && candidate.confidence <= 1_000
                        && !candidate.why.is_empty()
                        && candidate.why.len() <= 16
                        && candidate.suppressions_checked.len() <= 16
                        && candidate.source_refs.len() <= 8
                        && candidate
                            .source_refs
                            .iter()
                            .all(|source| source_ref_correlates(source, context))
                })
                && response
                    .blind_spots
                    .iter()
                    .all(|spot| !spot.category.is_empty() && spot.category.len() <= 256)
                && response
                    .false_positive_controls
                    .iter()
                    .all(|rule| !rule.rule.is_empty() && rule.rule.len() <= 256)
        }
        (
            FirstSliceIpcRequest::ArchitectureOverview(request),
            FirstSliceIpcResponse::ArchitectureOverview(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && request
                    .views
                    .iter()
                    .all(|view| !view.is_empty() && view.len() <= 32)
                && request
                    .max_components
                    .is_none_or(|max| (1..=250).contains(&max))
                && request
                    .min_confidence
                    .is_none_or(|confidence| confidence <= 1_000)
                && response.components.len() <= 250
                && response.connections.len() <= 1_000
                && response.hotspots.len() <= 250
                && response.views.len() <= 8
                && response.components.iter().all(|component| {
                    !component.id.is_empty()
                        && component.id.len() <= 512
                        && !component.kind.is_empty()
                        && component.kind.len() <= 64
                        && !component.name.is_empty()
                        && component.name.len() <= 1_024
                        && component.responsibility_evidence.len() <= 16
                        && component.confidence <= 1_000
                })
                && response.connections.iter().all(|connection| {
                    !connection.from.is_empty()
                        && connection.from.len() <= 512
                        && !connection.to.is_empty()
                        && connection.to.len() <= 512
                        && !connection.kind.is_empty()
                        && connection.kind.len() <= 32
                        && connection.confidence <= 1_000
                })
                && response.hotspots.iter().all(|hotspot| {
                    !hotspot.component_id.is_empty()
                        && hotspot.component_id.len() <= 512
                        && hotspot.score <= 1_000
                })
                && response.views.iter().all(|view| {
                    !view.view.is_empty()
                        && view.view.len() <= 32
                        && !view.algorithm_version.is_empty()
                        && view.algorithm_version.len() <= 128
                })
        }
        (
            FirstSliceIpcRequest::TestsSelect(request),
            FirstSliceIpcResponse::TestsSelect(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && response.coverage_strategy.is_some()
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && !request.seeds.is_empty()
                && request.seeds.len() <= 64
                && request.seeds.iter().all(|seed| seed.value.len() == 20)
                && request
                    .test_kinds
                    .iter()
                    .all(|kind| !kind.is_empty() && kind.len() <= 32)
                && request.max_tests.is_none_or(|max| (1..=500).contains(&max))
                && response.tests.len() <= 500
                && response.gaps.len() <= 128
                && response.tests.iter().all(|test| {
                    !test.test_id.is_empty()
                        && test.test_id.len() <= 512
                        && !test.kind.is_empty()
                        && test.kind.len() <= 32
                        && test
                            .path
                            .as_ref()
                            .is_none_or(|path| !path.is_empty() && path.len() <= 8_192)
                        && test.score <= 1_000
                        && !test.why.is_empty()
                        && test.why.len() <= 8
                        && test
                            .why
                            .iter()
                            .all(|reason| !reason.is_empty() && reason.len() <= 128)
                        && test
                            .command_hint
                            .as_ref()
                            .is_none_or(|hint| !hint.is_empty() && hint.len() <= 1_024)
                })
                && response.gaps.iter().all(|gap| {
                    !gap.scope.is_empty()
                        && gap.scope.len() <= 512
                        && !gap.reason.is_empty()
                        && gap.reason.len() <= 128
                })
        }
        (
            FirstSliceIpcRequest::ChangeImpact(request),
            FirstSliceIpcResponse::ChangeImpact(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(risk_summary) = response.risk_summary.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && (!request.changed_symbols.is_empty() || !request.changed_paths.is_empty())
                && request.changed_symbols.len() <= 256
                && request
                    .changed_symbols
                    .iter()
                    .all(|symbol| symbol.value.len() == 20)
                && request.changed_paths.len() <= 1_000
                && request
                    .changed_paths
                    .iter()
                    .all(|path| !path.is_empty() && path.len() <= 8_192)
                && request
                    .max_depth
                    .is_none_or(|depth| (1..=8).contains(&depth))
                && request
                    .min_confidence
                    .is_none_or(|confidence| confidence <= 1_000)
                && request
                    .max_dependents
                    .is_none_or(|max| (1..=500).contains(&max))
                && !response.resolved_changes.is_empty()
                && response.resolved_changes.len() <= 1_256
                && response.impacted.len() <= 1_256
                && response.tests.len() <= 500
                && response.resolved_changes.iter().all(|change| {
                    optional_wire_id_has_len(change.symbol_id.as_ref().map(|id| &id.value), 20)
                        && optional_wire_id_has_len(change.file_id.as_ref().map(|id| &id.value), 20)
                        && !change.classification.is_empty()
                        && change.classification.len() <= 32
                        && change
                            .kind
                            .as_ref()
                            .is_none_or(|kind| !kind.is_empty() && kind.len() <= 32)
                })
                && response.impacted.iter().all(|group| {
                    group.dependents.len() <= 500
                        && group.dependents.iter().all(|entry| {
                            entry
                                .symbol_id
                                .as_ref()
                                .is_some_and(|symbol| symbol.value.len() == 20)
                                && !entry.kind.is_empty()
                                && entry.kind.len() <= 32
                                && (1..=8).contains(&entry.distance)
                                && entry.confidence <= 1_000
                                && !entry.via.is_empty()
                                && entry.via.len() <= 16
                                && entry.via.iter().all(|predicate| {
                                    !predicate.is_empty() && predicate.len() <= 128
                                })
                        })
                })
                && response.tests.iter().all(|test| {
                    !test.test_id.is_empty()
                        && test.test_id.len() <= 512
                        && test.relevance <= 1_000
                        && !test.why.is_empty()
                        && test.why.len() <= 8
                        && test
                            .why
                            .iter()
                            .all(|reason| !reason.is_empty() && reason.len() <= 128)
                })
                && !risk_summary.level.is_empty()
                && risk_summary.level.len() <= 32
                && risk_summary.reasons.len() <= 16
                && risk_summary
                    .reasons
                    .iter()
                    .all(|reason| !reason.is_empty() && reason.len() <= 128)
                && !risk_summary.coverage.is_empty()
                && risk_summary.coverage.len() <= 32
                && risk_summary.fanout <= 100_000
        }
        (
            FirstSliceIpcRequest::PlanChange(request),
            FirstSliceIpcResponse::PlanChange(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(affected_scope) = response.affected_scope.as_ref() else {
                return false;
            };
            let Some(context_pack) = response.context_pack_request.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && (!request.target_symbols.is_empty() || !request.target_files.is_empty())
                && request.target_symbols.len() <= 64
                && request
                    .target_symbols
                    .iter()
                    .all(|symbol| symbol.value.len() == 20)
                && request.target_files.len() <= 64
                && request
                    .target_files
                    .iter()
                    .all(|file| file.value.len() == 20)
                && !request.objective.is_empty()
                && request.objective.len() <= 32
                && !request.objective_text.is_empty()
                && request.objective_text.len() <= 4_096
                && request
                    .max_steps
                    .is_none_or(|steps| (1..=100).contains(&steps))
                && !response.plan.is_empty()
                && response.plan.len() <= 100
                && response.plan.iter().all(|step| {
                    (1..=100).contains(&step.step)
                        && !step.action.is_empty()
                        && step.action.len() <= 1_024
                        && step.targets.len() <= 32
                        && step.targets.iter().all(|symbol| symbol.value.len() == 20)
                        && step.depends_on.len() <= 32
                        && step.depends_on.iter().all(|dep| (1..=100).contains(dep))
                        && step.risks.len() <= 8
                        && step
                            .risks
                            .iter()
                            .all(|risk| !risk.is_empty() && risk.len() <= 128)
                        && step
                            .verification
                            .as_ref()
                            .is_none_or(|hint| !hint.is_empty() && hint.len() <= 1_024)
                })
                && affected_scope.affected_symbols <= 100_000
                && affected_scope.affected_files <= 100_000
                && !affected_scope.risk_level.is_empty()
                && affected_scope.risk_level.len() <= 32
                && response.test_plan.len() <= 500
                && response.test_plan.iter().all(|test| {
                    !test.test_id.is_empty()
                        && test.test_id.len() <= 512
                        && test.relevance <= 1_000
                        && !test.why.is_empty()
                        && test.why.len() <= 8
                        && test
                            .why
                            .iter()
                            .all(|reason| !reason.is_empty() && reason.len() <= 128)
                })
                && response.open_decisions.len() <= 16
                && response.open_decisions.iter().all(|decision| {
                    !decision.question.is_empty()
                        && decision.question.len() <= 512
                        && !decision.recommended_default.is_empty()
                        && decision.recommended_default.len() <= 512
                })
                && context_pack.symbols.len() <= 64
                && context_pack
                    .symbols
                    .iter()
                    .all(|symbol| symbol.value.len() == 20)
                && context_pack.files.len() <= 64
                && context_pack.files.iter().all(|file| file.value.len() == 20)
        }
        (
            FirstSliceIpcRequest::HistoryCompare(request),
            FirstSliceIpcResponse::HistoryCompare(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            let Some(matched_states) = response.matched_states.as_ref() else {
                return false;
            };
            let Some(architecture_delta) = response.architecture_delta.as_ref() else {
                return false;
            };
            let Some(head_selector) = revision_generation_selector(request.head.as_ref()) else {
                return false;
            };
            let Some(request_base) = revision_generation_id(request.base.as_ref()) else {
                return false;
            };
            let Some(base_generation) = matched_states.base_generation.as_ref() else {
                return false;
            };
            let Some(head_generation) = matched_states.head_generation.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    Some(&head_selector),
                )
                && wire_id_equals(
                    Some(&head_generation.value),
                    context.generation.as_ref().map(|id| &id.value),
                )
                && wire_id_equals(Some(&base_generation.value), Some(&request_base.value))
                && wire_id_has_len(Some(&base_generation.value), 20)
                && wire_id_has_len(Some(&head_generation.value), 20)
                && !matched_states.coverage.is_empty()
                && matched_states.coverage.len() <= 32
                && request.change_kinds.len() <= 8
                && request
                    .change_kinds
                    .iter()
                    .all(|kind| !kind.is_empty() && kind.len() <= 32)
                && request
                    .max_results
                    .is_none_or(|results| (1..=1_000).contains(&results))
                && response.changes.len() <= 1_000
                && response.changes.iter().all(|change| {
                    !change.kind.is_empty()
                        && change.kind.len() <= 32
                        && change
                            .symbol_id
                            .as_ref()
                            .is_some_and(|symbol| symbol.value.len() == 20)
                        && !change.entity_kind.is_empty()
                        && change.entity_kind.len() <= 32
                        && change.significance <= 1_000
                })
                && architecture_delta.new_cross_service_edges <= 10_000
                && architecture_delta.removed_cross_service_edges <= 10_000
                && architecture_delta.new_boundaries <= 10_000
                && architecture_delta.removed_boundaries <= 10_000
                && response.breaking_candidates.len() <= 256
                && response.breaking_candidates.iter().all(|candidate| {
                    candidate
                        .symbol_id
                        .as_ref()
                        .is_some_and(|symbol| symbol.value.len() == 20)
                        && candidate.consumer_count <= 100_000
                        && !candidate.reason.is_empty()
                        && candidate.reason.len() <= 128
                })
                && response.lineage.len() <= 1_000
                && response.lineage.iter().all(|lineage| {
                    lineage
                        .base_symbol_id
                        .as_ref()
                        .is_some_and(|symbol| symbol.value.len() == 20)
                        && lineage
                            .head_symbol_id
                            .as_ref()
                            .is_some_and(|symbol| symbol.value.len() == 20)
                        && lineage.confidence <= 1_000
                })
        }
        (
            FirstSliceIpcRequest::QueryAdvanced(request),
            FirstSliceIpcResponse::QueryAdvanced(response),
        ) => {
            let Some(context) = response.context.as_ref() else {
                return false;
            };
            first_slice_schema_matches(response.schema_version.as_ref())
                && query_context_correlates(
                    context,
                    request.repository.as_ref(),
                    request.generation.as_ref(),
                )
                && !request.query_ast.is_empty()
                && request.query_ast.len() <= 65_536
                && request
                    .max_results
                    .is_none_or(|results| (1..=1_000).contains(&results))
                && request
                    .max_depth
                    .is_none_or(|depth| (1..=5).contains(&depth))
                && (1..=64).contains(&response.columns.len())
                && response.columns.iter().all(|column| {
                    !column.name.is_empty()
                        && column.name.len() <= 256
                        && !column.column_type.is_empty()
                        && column.column_type.len() <= 32
                })
                && response.rows.len() <= 1_000
                && response
                    .rows
                    .iter()
                    .all(|row| !row.is_empty() && row.len() <= 65_536)
                && !response.completeness.is_empty()
                && response.completeness.len() <= 32
                && response.plan.as_ref().is_none_or(|plan| {
                    plan.estimated_cost <= 10_000_000
                        && plan.operators.len() <= 64
                        && plan
                            .operators
                            .iter()
                            .all(|operator| !operator.is_empty() && operator.len() <= 128)
                        && plan.applied_limits.len() <= 16
                        && plan
                            .applied_limits
                            .iter()
                            .all(|limit| !limit.is_empty() && limit.len() <= 256)
                })
        }
        _ => false,
    }
}

fn first_slice_schema_matches(version: Option<&common::ContractVersion>) -> bool {
    version.is_some_and(|version| version.major == 1 && version.minor == 0)
}

fn wire_id_has_len(value: Option<&Vec<u8>>, expected: usize) -> bool {
    value.is_some_and(|value| value.len() == expected)
}

fn optional_wire_id_has_len(value: Option<&Vec<u8>>, expected: usize) -> bool {
    value.is_none_or(|value| value.len() == expected)
}

fn wire_id_equals(left: Option<&Vec<u8>>, right: Option<&Vec<u8>>) -> bool {
    left.is_some() && left == right
}

/// Builds a generation selector from a revision selector naming a generation.
///
/// Git revision selectors return `None` because the first-slice daemon maps no
/// git ref to a retained generation.
fn revision_generation_selector(
    selector: Option<&daemon::FirstSliceRevisionSelector>,
) -> Option<daemon::GenerationSelector> {
    match selector.and_then(|selector| selector.selector.as_ref())? {
        daemon::first_slice_revision_selector::Selector::Generation(generation) => {
            Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Generation(
                    generation.clone(),
                )),
            })
        }
        daemon::first_slice_revision_selector::Selector::Git(_) => None,
    }
}

/// Returns the generation identity named by a revision selector, if any.
fn revision_generation_id(
    selector: Option<&daemon::FirstSliceRevisionSelector>,
) -> Option<&common::GenerationId> {
    match selector.and_then(|selector| selector.selector.as_ref())? {
        daemon::first_slice_revision_selector::Selector::Generation(generation) => Some(generation),
        daemon::first_slice_revision_selector::Selector::Git(_) => None,
    }
}

fn valid_repository_operation_status(operation: &daemon::OperationStatus) -> bool {
    let Ok(state) = daemon::OperationState::try_from(operation.state) else {
        return false;
    };
    let Ok(stage) = daemon::OperationStage::try_from(operation.stage) else {
        return false;
    };
    let Ok(recovery) = daemon::RecoveryClass::try_from(operation.recovery_class) else {
        return false;
    };
    let error_is_valid = operation
        .error
        .as_ref()
        .is_none_or(|error| checked_public_error_from_wire(error).is_some());

    wire_id_has_len(operation.operation.as_ref().map(|id| &id.value), 16)
        && matches!(
            state,
            daemon::OperationState::Queued
                | daemon::OperationState::Running
                | daemon::OperationState::Cancelling
                | daemon::OperationState::Succeeded
                | daemon::OperationState::Failed
                | daemon::OperationState::Interrupted
                | daemon::OperationState::Cancelled
        )
        && operation.kind == daemon::OperationKind::RepositoryIndex as i32
        && matches!(
            stage,
            daemon::OperationStage::Accepted
                | daemon::OperationStage::Executing
                | daemon::OperationStage::Cleanup
        )
        && operation.plan_hash.len() == 32
        && matches!(
            recovery,
            daemon::RecoveryClass::NotApplicable
                | daemon::RecoveryClass::InterruptedByRestart
                | daemon::RecoveryClass::DeadlineElapsed
                | daemon::RecoveryClass::LeaseExpired
        )
        && error_is_valid
        && (state == daemon::OperationState::Failed) == operation.error.is_some()
        && operation.error.as_ref().is_none_or(|error| {
            error.operation.as_ref().is_none_or(|error_operation| {
                wire_id_equals(
                    Some(&error_operation.value),
                    operation
                        .operation
                        .as_ref()
                        .map(|operation| &operation.value),
                )
            })
        })
        && (state == daemon::OperationState::Interrupted)
            != (recovery == daemon::RecoveryClass::NotApplicable)
        && operation.deadline_unix_ms != Some(0)
        && operation.lease_expires_unix_ms != Some(0)
        && operation.detached == operation.lease_expires_unix_ms.is_none()
        && (operation.total_units == 0 || operation.completed_units <= operation.total_units)
}

fn checked_public_error_from_wire(error: &common::PublicError) -> Option<PublicError> {
    if error.message.len() > MAX_WIRE_PUBLIC_ERROR_MESSAGE_BYTES
        || error.details.len() > MAX_WIRE_PUBLIC_ERROR_DETAILS
        || error.next_actions.len() > MAX_WIRE_PUBLIC_ERROR_ACTIONS
        || error.retry_after_ms.is_some() && !error.retryable
    {
        return None;
    }
    let code = match common::ErrorCode::try_from(error.code).ok()? {
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
        common::ErrorCode::Unspecified => return None,
    };
    let mut builder = PublicError::builder_with_message(code, error.message.clone());
    if let Some(delay) = error.retry_after_ms {
        builder = builder.retry_after(Duration::from_millis(delay));
    } else if error.retryable {
        builder = builder.retryable();
    }
    if let Some(repository) = error.repository.as_ref() {
        builder = builder.repository(RepositoryId::from_bytes(
            repository.value.as_slice().try_into().ok()?,
        ));
    }
    if let Some(operation) = error.operation.as_ref() {
        builder = builder.operation(OperationId::from_bytes(
            operation.value.as_slice().try_into().ok()?,
        ));
    }
    if let Some(generation) = error.generation.as_ref() {
        builder = builder.generation(GenerationId::from_bytes(
            generation.value.as_slice().try_into().ok()?,
        ));
    }
    for (key, value) in &error.details {
        builder = builder.detail(
            DetailKey::parse(key).ok()?,
            checked_public_value_from_wire(value)?,
        );
    }
    for action in &error.next_actions {
        builder = builder.next_action(checked_next_action_from_wire(action)?);
    }
    builder.build().ok()
}

fn checked_public_value_from_wire(value: &common::PublicValue) -> Option<PublicValue> {
    use common::public_value::Value;

    match value.value.as_ref()? {
        Value::Boolean(value) => Some(PublicValue::Boolean(*value)),
        Value::Integer(value) => Some(PublicValue::Integer(*value)),
        Value::Unsigned(value) => Some(PublicValue::Unsigned(*value)),
        Value::Repository(value) => Some(PublicValue::Repository(RepositoryId::from_bytes(
            value.value.as_slice().try_into().ok()?,
        ))),
        Value::Generation(value) => Some(PublicValue::Generation(GenerationId::from_bytes(
            value.value.as_slice().try_into().ok()?,
        ))),
        Value::Operation(value) => Some(PublicValue::Operation(OperationId::from_bytes(
            value.value.as_slice().try_into().ok()?,
        ))),
        Value::Label(value) => Some(PublicValue::Label(SafeLabel::parse(value).ok()?)),
    }
}

fn checked_next_action_from_wire(action: &common::NextAction) -> Option<NextAction> {
    match common::next_action::Kind::try_from(action.kind).ok()? {
        common::next_action::Kind::CorrectField => Some(NextAction::CorrectField {
            field: DetailKey::parse(action.field.as_deref()?).ok()?,
        }),
        common::next_action::Kind::Retry if action.field.is_none() => Some(NextAction::Retry),
        common::next_action::Kind::SelectSupportedVersion if action.field.is_none() => {
            Some(NextAction::SelectSupportedVersion)
        }
        common::next_action::Kind::InspectOperation if action.field.is_none() => {
            Some(NextAction::InspectOperation)
        }
        common::next_action::Kind::RebuildRepository if action.field.is_none() => {
            Some(NextAction::RebuildRepository)
        }
        common::next_action::Kind::CollectSupportBundle if action.field.is_none() => {
            Some(NextAction::CollectSupportBundle)
        }
        common::next_action::Kind::Unspecified
        | common::next_action::Kind::Retry
        | common::next_action::Kind::SelectSupportedVersion
        | common::next_action::Kind::InspectOperation
        | common::next_action::Kind::RebuildRepository
        | common::next_action::Kind::CollectSupportBundle => None,
    }
}

fn query_context_correlates(
    context: &daemon::FirstSliceQueryContext,
    repository: Option<&common::RepositoryId>,
    selector: Option<&daemon::GenerationSelector>,
) -> bool {
    wire_id_equals(
        context.repository.as_ref().map(|id| &id.value),
        repository.map(|id| &id.value),
    ) && wire_id_has_len(context.generation.as_ref().map(|id| &id.value), 20)
        && optional_wire_id_has_len(context.parent_generation.as_ref().map(|id| &id.value), 20)
        && !wire_id_equals(
            context.parent_generation.as_ref().map(|id| &id.value),
            context.generation.as_ref().map(|id| &id.value),
        )
        && match selector.and_then(|selector| selector.selector.as_ref()) {
            Some(daemon::generation_selector::Selector::Active(true)) => context.active_generation,
            Some(daemon::generation_selector::Selector::Generation(generation)) => wire_id_equals(
                context.generation.as_ref().map(|id| &id.value),
                Some(&generation.value),
            ),
            _ => false,
        }
        && valid_analysis_tier(context.tier)
        && matches!(
            daemon::FirstSliceCoverageStatus::try_from(context.coverage_status),
            Ok(daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete
                | daemon::FirstSliceCoverageStatus::FirstSliceCoverageBounded
                | daemon::FirstSliceCoverageStatus::FirstSliceCoverageSampled
                | daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown)
        )
        && context.usage.is_some()
}

fn valid_analysis_tier(tier: i32) -> bool {
    matches!(
        daemon::FirstSliceAnalysisTier::try_from(tier),
        Ok(daemon::FirstSliceAnalysisTier::FirstSliceTierA
            | daemon::FirstSliceAnalysisTier::FirstSliceTierB
            | daemon::FirstSliceAnalysisTier::FirstSliceTierC
            | daemon::FirstSliceAnalysisTier::FirstSliceTierD)
    )
}

fn source_ref_correlates(
    source: &daemon::FirstSliceSourceRef,
    context: &daemon::FirstSliceQueryContext,
) -> bool {
    validate_source_reference(source).is_ok()
        && wire_id_equals(
            source.repository.as_ref().map(|id| &id.value),
            context.repository.as_ref().map(|id| &id.value),
        )
        && wire_id_equals(
            source.generation.as_ref().map(|id| &id.value),
            context.generation.as_ref().map(|id| &id.value),
        )
}

fn wire_ids_form_subsequence<'a>(
    candidates: impl Iterator<Item = &'a common::SymbolId>,
    requested: &[common::SymbolId],
) -> bool {
    let mut start = 0;
    for candidate in candidates {
        let Some(offset) = requested[start..]
            .iter()
            .position(|item| item.value == candidate.value)
        else {
            return false;
        };
        start += offset + 1;
    }
    true
}

fn validate_first_slice_request(request: &FirstSliceIpcRequest) -> Result<(), Box<PublicError>> {
    match request {
        FirstSliceIpcRequest::RepositoryIndex(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(request.operation.as_ref().map(|id| id.value.as_slice()), 16)?;
            if request.root.is_empty()
                || request.root.len() > 4096
                || request.root.as_bytes().contains(&0)
            {
                return Err(Box::new(invalid_argument("repository root is invalid")));
            }
        }
        FirstSliceIpcRequest::RepositoryOperationStatus(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(request.operation.as_ref().map(|id| id.value.as_slice()), 16)?;
            if !matches!(
                daemon::RepositoryOperationAction::try_from(request.action),
                Ok(daemon::RepositoryOperationAction::RepositoryOperationGet
                    | daemon::RepositoryOperationAction::RepositoryOperationCancel)
            ) || request.wait_ms.is_some_and(|wait| wait > 30_000)
            {
                return Err(Box::new(invalid_argument(
                    "repository operation request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::CodeLocate(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.query.is_empty()
                || request.query.len() > 2048
                || !(1..=200).contains(&request.maximum_results)
                || !matches!(
                    daemon::FirstSliceLocateMode::try_from(request.mode),
                    Ok(daemon::FirstSliceLocateMode::FirstSliceLocateExact
                        | daemon::FirstSliceLocateMode::FirstSliceLocatePrefix
                        | daemon::FirstSliceLocateMode::FirstSliceLocateText
                        | daemon::FirstSliceLocateMode::FirstSliceLocateSafeRegex
                        | daemon::FirstSliceLocateMode::FirstSliceLocateGlob)
                )
            {
                return Err(Box::new(invalid_argument("code locate request is invalid")));
            }
        }
        FirstSliceIpcRequest::SymbolExplain(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.symbols.is_empty() || request.symbols.len() > 16 {
                return Err(Box::new(invalid_argument(
                    "symbol explanation request is invalid",
                )));
            }
            let mut observed = std::collections::BTreeSet::new();
            for symbol in &request.symbols {
                require_wire_id(Some(symbol.value.as_slice()), 20)?;
                if !observed.insert(symbol.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "symbol identifiers must be distinct",
                    )));
                }
            }
        }
        FirstSliceIpcRequest::SourceRead(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            let repository = request
                .repository
                .as_ref()
                .map(|id| id.value.as_slice())
                .ok_or_else(|| Box::new(invalid_argument("source repository is invalid")))?;
            require_wire_id(Some(repository), 16)?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.references.is_empty() || request.references.len() > 32 {
                return Err(Box::new(invalid_argument("source references are invalid")));
            }
            let explicit_generation = explicit_generation_bytes(request.generation.as_ref());
            let mut observed_generation: Option<&[u8]> = None;
            let mut observed = std::collections::BTreeSet::new();
            for reference in &request.references {
                validate_source_reference(reference)?;
                let reference_repository = reference
                    .repository
                    .as_ref()
                    .map(|id| id.value.as_slice())
                    .ok_or_else(|| Box::new(invalid_argument("source repository is invalid")))?;
                let reference_generation = reference
                    .generation
                    .as_ref()
                    .map(|id| id.value.as_slice())
                    .ok_or_else(|| Box::new(invalid_argument("source generation is invalid")))?;
                if reference_repository != repository
                    || explicit_generation.is_some_and(|value| value != reference_generation)
                    || observed_generation.is_some_and(|value| value != reference_generation)
                {
                    return Err(Box::new(invalid_argument(
                        "source reference correlation is invalid",
                    )));
                }
                observed_generation = Some(reference_generation);
                let key = (
                    reference_generation,
                    reference.file.as_ref().map(|id| id.value.as_slice()),
                    reference.start_byte,
                    reference.end_byte,
                );
                if !observed.insert(key) {
                    return Err(Box::new(invalid_argument(
                        "source references must be distinct",
                    )));
                }
            }
        }
        FirstSliceIpcRequest::RepositoryList(request) => {
            if request
                .max_results
                .is_some_and(|max| !(1..=1000).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "repository list request is invalid",
                )));
            }
            if request
                .query
                .as_ref()
                .is_some_and(|query| query.len() > 2048 || query.as_bytes().contains(&0))
            {
                return Err(Box::new(invalid_argument(
                    "repository list request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::RepositoryStatus(request) => {
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
        }
        FirstSliceIpcRequest::SymbolRelationships(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.seeds.is_empty() || request.seeds.len() > 64 {
                return Err(Box::new(invalid_argument(
                    "symbol relationships request is invalid",
                )));
            }
            let mut observed = std::collections::BTreeSet::new();
            for seed in &request.seeds {
                require_wire_id(Some(seed.value.as_slice()), 20)?;
                if !observed.insert(seed.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "seed identifiers must be distinct",
                    )));
                }
            }
            if request.relations.is_empty() || request.relations.len() > 16 {
                return Err(Box::new(invalid_argument(
                    "symbol relationships request is invalid",
                )));
            }
            for relation in &request.relations {
                if relation.is_empty() || relation.len() > 32 || relation.as_bytes().contains(&0) {
                    return Err(Box::new(invalid_argument(
                        "symbol relationships request is invalid",
                    )));
                }
            }
            if request.direction.as_ref().is_some_and(|direction| {
                direction.is_empty() || direction.len() > 16 || direction.as_bytes().contains(&0)
            }) {
                return Err(Box::new(invalid_argument(
                    "symbol relationships request is invalid",
                )));
            }
            if request
                .min_confidence
                .is_some_and(|confidence| confidence > 1_000)
            {
                return Err(Box::new(invalid_argument(
                    "symbol relationships request is invalid",
                )));
            }
            if request
                .max_results
                .is_some_and(|max| !(1..=500).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "symbol relationships request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::FlowTrace(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            require_wire_id(request.from.as_ref().map(|id| id.value.as_slice()), 20)?;
            if let Some(to) = request.to.as_ref() {
                require_wire_id(Some(to.value.as_slice()), 20)?;
            }
            if request.relations.is_empty() || request.relations.len() > 16 {
                return Err(Box::new(invalid_argument("flow trace request is invalid")));
            }
            for relation in &request.relations {
                if relation.is_empty() || relation.len() > 32 || relation.as_bytes().contains(&0) {
                    return Err(Box::new(invalid_argument("flow trace request is invalid")));
                }
            }
            if request.direction.as_ref().is_some_and(|direction| {
                direction.is_empty() || direction.len() > 16 || direction.as_bytes().contains(&0)
            }) {
                return Err(Box::new(invalid_argument("flow trace request is invalid")));
            }
            if request
                .max_depth
                .is_some_and(|depth| !(1..=8).contains(&depth))
            {
                return Err(Box::new(invalid_argument("flow trace request is invalid")));
            }
            if request
                .max_paths
                .is_some_and(|paths| !(1..=100).contains(&paths))
            {
                return Err(Box::new(invalid_argument("flow trace request is invalid")));
            }
            if request
                .min_confidence
                .is_some_and(|confidence| confidence > 1_000)
            {
                return Err(Box::new(invalid_argument("flow trace request is invalid")));
            }
        }
        FirstSliceIpcRequest::ArchitectureCycles(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.relations.is_empty() || request.relations.len() > 8 {
                return Err(Box::new(invalid_argument(
                    "architecture cycles request is invalid",
                )));
            }
            for relation in &request.relations {
                if relation.is_empty() || relation.len() > 32 || relation.as_bytes().contains(&0) {
                    return Err(Box::new(invalid_argument(
                        "architecture cycles request is invalid",
                    )));
                }
            }
            if request
                .min_size
                .is_some_and(|size| !(2..=64).contains(&size))
            {
                return Err(Box::new(invalid_argument(
                    "architecture cycles request is invalid",
                )));
            }
            if request
                .max_cycles
                .is_some_and(|max| !(1..=200).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "architecture cycles request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::CodeDead(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.entry_point_policy.as_ref().is_some_and(|policy| {
                policy.is_empty() || policy.len() > 32 || policy.as_bytes().contains(&0)
            }) {
                return Err(Box::new(invalid_argument("code dead request is invalid")));
            }
            if request
                .min_confidence
                .is_some_and(|confidence| confidence > 1_000)
            {
                return Err(Box::new(invalid_argument("code dead request is invalid")));
            }
            if request
                .max_candidates
                .is_some_and(|max| !(1..=500).contains(&max))
            {
                return Err(Box::new(invalid_argument("code dead request is invalid")));
            }
        }
        FirstSliceIpcRequest::ArchitectureOverview(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.views.len() > 8
                || request
                    .views
                    .iter()
                    .any(|view| view.is_empty() || view.len() > 32 || view.as_bytes().contains(&0))
            {
                return Err(Box::new(invalid_argument(
                    "architecture overview request is invalid",
                )));
            }
            if request
                .min_confidence
                .is_some_and(|confidence| confidence > 1_000)
            {
                return Err(Box::new(invalid_argument(
                    "architecture overview request is invalid",
                )));
            }
            if request
                .max_components
                .is_some_and(|max| !(1..=250).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "architecture overview request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::TestsSelect(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.seeds.is_empty() || request.seeds.len() > 64 {
                return Err(Box::new(invalid_argument(
                    "tests select request is invalid",
                )));
            }
            let mut observed = std::collections::BTreeSet::new();
            for seed in &request.seeds {
                require_wire_id(Some(seed.value.as_slice()), 20)?;
                if !observed.insert(seed.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "seed identifiers must be distinct",
                    )));
                }
            }
            if request.test_kinds.len() > 6
                || request
                    .test_kinds
                    .iter()
                    .any(|kind| kind.is_empty() || kind.len() > 32 || kind.as_bytes().contains(&0))
            {
                return Err(Box::new(invalid_argument(
                    "tests select request is invalid",
                )));
            }
            if request
                .max_tests
                .is_some_and(|max| !(1..=500).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "tests select request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::ChangeImpact(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.changed_symbols.is_empty() && request.changed_paths.is_empty() {
                return Err(Box::new(invalid_argument(
                    "change impact request requires an explicit change set",
                )));
            }
            if request.changed_symbols.len() > 256 {
                return Err(Box::new(invalid_argument(
                    "change impact request is invalid",
                )));
            }
            let mut observed = std::collections::BTreeSet::new();
            for symbol in &request.changed_symbols {
                require_wire_id(Some(symbol.value.as_slice()), 20)?;
                if !observed.insert(symbol.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "change symbol identifiers must be distinct",
                    )));
                }
            }
            if request.changed_paths.len() > 1_000
                || request.changed_paths.iter().any(|path| {
                    path.is_empty() || path.len() > 8_192 || path.as_bytes().contains(&0)
                })
            {
                return Err(Box::new(invalid_argument(
                    "change impact request is invalid",
                )));
            }
            if request
                .max_depth
                .is_some_and(|depth| !(1..=8).contains(&depth))
            {
                return Err(Box::new(invalid_argument(
                    "change impact request is invalid",
                )));
            }
            if request
                .min_confidence
                .is_some_and(|confidence| confidence > 1_000)
            {
                return Err(Box::new(invalid_argument(
                    "change impact request is invalid",
                )));
            }
            if request
                .max_dependents
                .is_some_and(|max| !(1..=500).contains(&max))
            {
                return Err(Box::new(invalid_argument(
                    "change impact request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::PlanChange(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.objective.is_empty() || request.objective.len() > 32 {
                return Err(Box::new(invalid_argument(
                    "plan change request requires an objective",
                )));
            }
            if request.objective_text.is_empty() || request.objective_text.len() > 4_096 {
                return Err(Box::new(invalid_argument(
                    "plan change request requires objective text",
                )));
            }
            if request.target_symbols.is_empty() && request.target_files.is_empty() {
                return Err(Box::new(invalid_argument(
                    "plan change request requires an explicit target set",
                )));
            }
            if request.target_symbols.len() > 64 {
                return Err(Box::new(invalid_argument("plan change request is invalid")));
            }
            let mut observed_symbols = std::collections::BTreeSet::new();
            for symbol in &request.target_symbols {
                require_wire_id(Some(symbol.value.as_slice()), 20)?;
                if !observed_symbols.insert(symbol.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "plan target symbol identifiers must be distinct",
                    )));
                }
            }
            if request.target_files.len() > 64 {
                return Err(Box::new(invalid_argument("plan change request is invalid")));
            }
            let mut observed_files = std::collections::BTreeSet::new();
            for file in &request.target_files {
                require_wire_id(Some(file.value.as_slice()), 20)?;
                if !observed_files.insert(file.value.as_slice()) {
                    return Err(Box::new(invalid_argument(
                        "plan target file identifiers must be distinct",
                    )));
                }
            }
            if request
                .max_steps
                .is_some_and(|steps| !(1..=100).contains(&steps))
            {
                return Err(Box::new(invalid_argument("plan change request is invalid")));
            }
        }
        FirstSliceIpcRequest::HistoryCompare(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_revision_selector(request.base.as_ref())?;
            validate_revision_selector(request.head.as_ref())?;
            if request.change_kinds.len() > 8 {
                return Err(Box::new(invalid_argument(
                    "history compare request is invalid",
                )));
            }
            for kind in &request.change_kinds {
                if kind.is_empty() || kind.len() > 32 {
                    return Err(Box::new(invalid_argument(
                        "history compare request is invalid",
                    )));
                }
            }
            if request
                .max_results
                .is_some_and(|results| !(1..=1_000).contains(&results))
            {
                return Err(Box::new(invalid_argument(
                    "history compare request is invalid",
                )));
            }
        }
        FirstSliceIpcRequest::QueryAdvanced(request) => {
            require_first_slice_schema(request.schema_version.as_ref())?;
            require_wire_id(
                request.repository.as_ref().map(|id| id.value.as_slice()),
                16,
            )?;
            validate_generation_selector(request.generation.as_ref())?;
            if request.query_ast.is_empty() || request.query_ast.len() > 65_536 {
                return Err(Box::new(invalid_argument(
                    "advanced query request is invalid",
                )));
            }
            if request
                .max_results
                .is_some_and(|results| !(1..=1_000).contains(&results))
            {
                return Err(Box::new(invalid_argument(
                    "advanced query request is invalid",
                )));
            }
            if request
                .max_depth
                .is_some_and(|depth| !(1..=5).contains(&depth))
            {
                return Err(Box::new(invalid_argument(
                    "advanced query request is invalid",
                )));
            }
        }
    }
    Ok(())
}

fn require_first_slice_schema(
    version: Option<&common::ContractVersion>,
) -> Result<(), Box<PublicError>> {
    if version.is_some_and(|version| version.major == 1 && version.minor == 0) {
        Ok(())
    } else {
        Err(Box::new(protocol_mismatch(
            "first-slice schema version is unsupported",
        )))
    }
}

fn require_wire_id(value: Option<&[u8]>, expected: usize) -> Result<(), Box<PublicError>> {
    if value.is_some_and(|value| value.len() == expected) {
        Ok(())
    } else {
        Err(Box::new(invalid_argument(
            "first-slice identifier is invalid",
        )))
    }
}

fn validate_generation_selector(
    selector: Option<&daemon::GenerationSelector>,
) -> Result<(), Box<PublicError>> {
    match selector.and_then(|selector| selector.selector.as_ref()) {
        Some(daemon::generation_selector::Selector::Active(true)) => Ok(()),
        Some(daemon::generation_selector::Selector::Generation(generation)) => {
            require_wire_id(Some(generation.value.as_slice()), 20)
        }
        _ => Err(Box::new(invalid_argument("generation selector is invalid"))),
    }
}

fn explicit_generation_bytes(selector: Option<&daemon::GenerationSelector>) -> Option<&[u8]> {
    match selector.and_then(|selector| selector.selector.as_ref()) {
        Some(daemon::generation_selector::Selector::Generation(generation)) => {
            Some(generation.value.as_slice())
        }
        _ => None,
    }
}

/// Validates a history-compare revision selector wire shape.
///
/// Both a generation identity and a git ref expression are well-formed here; the
/// daemon application rejects git refs as unsupported because it maps no git ref
/// to a retained generation.
fn validate_revision_selector(
    selector: Option<&daemon::FirstSliceRevisionSelector>,
) -> Result<(), Box<PublicError>> {
    match selector.and_then(|selector| selector.selector.as_ref()) {
        Some(daemon::first_slice_revision_selector::Selector::Generation(generation)) => {
            require_wire_id(Some(generation.value.as_slice()), 20)
        }
        Some(daemon::first_slice_revision_selector::Selector::Git(git)) => {
            if git.is_empty() || git.len() > 512 {
                return Err(Box::new(invalid_argument(
                    "history compare git revision is invalid",
                )));
            }
            Ok(())
        }
        None => Err(Box::new(invalid_argument(
            "history compare request requires a revision selector",
        ))),
    }
}

fn validate_source_reference(
    reference: &daemon::FirstSliceSourceRef,
) -> Result<(), Box<PublicError>> {
    require_wire_id(
        reference.repository.as_ref().map(|id| id.value.as_slice()),
        16,
    )?;
    require_wire_id(
        reference.generation.as_ref().map(|id| id.value.as_slice()),
        20,
    )?;
    require_wire_id(reference.file.as_ref().map(|id| id.value.as_slice()), 20)?;
    require_wire_id(
        reference
            .content_hash
            .as_ref()
            .map(|hash| hash.value.as_slice()),
        32,
    )?;
    let lines_valid = match (reference.start_line, reference.end_line) {
        (None, None) => true,
        (Some(start), Some(end)) => start > 0 && start <= end,
        _ => false,
    };
    if reference.start_byte > reference.end_byte || !lines_valid {
        return Err(Box::new(invalid_argument(
            "source reference range is invalid",
        )));
    }
    Ok(())
}

fn control_method_from_wire(request: Option<&daemon::request_envelope::Request>) -> ControlMethod {
    match request {
        Some(daemon::request_envelope::Request::Health(_)) => ControlMethod::Health,
        Some(daemon::request_envelope::Request::DiagnosticsQuick(_)) => {
            ControlMethod::DiagnosticsQuick
        }
        Some(daemon::request_envelope::Request::SupportBundle(_)) => ControlMethod::SupportBundle,
        Some(daemon::request_envelope::Request::OperationSubmit(_)) => {
            ControlMethod::OperationSubmit
        }
        Some(daemon::request_envelope::Request::OperationStatus(_)) => {
            ControlMethod::OperationStatus
        }
        Some(daemon::request_envelope::Request::OperationCancel(_)) => {
            ControlMethod::OperationCancel
        }
        Some(daemon::request_envelope::Request::OperationLeaseRenew(_)) => {
            ControlMethod::OperationLeaseRenew
        }
        Some(daemon::request_envelope::Request::RepositoryIndex(_)) => {
            ControlMethod::RepositoryIndex
        }
        Some(daemon::request_envelope::Request::RepositoryOperationStatus(_)) => {
            ControlMethod::RepositoryOperationStatus
        }
        Some(daemon::request_envelope::Request::CodeLocate(_)) => ControlMethod::CodeLocate,
        Some(daemon::request_envelope::Request::SymbolExplain(_)) => ControlMethod::SymbolExplain,
        Some(daemon::request_envelope::Request::SourceRead(_)) => ControlMethod::SourceRead,
        Some(daemon::request_envelope::Request::RepositoryList(_)) => ControlMethod::RepositoryList,
        Some(daemon::request_envelope::Request::RepositoryStatus(_)) => {
            ControlMethod::RepositoryStatus
        }
        Some(daemon::request_envelope::Request::SymbolRelationships(_)) => {
            ControlMethod::SymbolRelationships
        }
        Some(daemon::request_envelope::Request::FlowTrace(_)) => ControlMethod::FlowTrace,
        Some(daemon::request_envelope::Request::ArchitectureCycles(_)) => {
            ControlMethod::ArchitectureCycles
        }
        Some(daemon::request_envelope::Request::CodeDead(_)) => ControlMethod::CodeDead,
        Some(daemon::request_envelope::Request::ArchitectureOverview(_)) => {
            ControlMethod::ArchitectureOverview
        }
        Some(daemon::request_envelope::Request::TestsSelect(_)) => ControlMethod::TestsSelect,
        Some(daemon::request_envelope::Request::ChangeImpact(_)) => ControlMethod::ChangeImpact,
        Some(daemon::request_envelope::Request::PlanChange(_)) => ControlMethod::PlanChange,
        Some(daemon::request_envelope::Request::HistoryCompare(_)) => ControlMethod::HistoryCompare,
        Some(daemon::request_envelope::Request::AdvancedQuery(_)) => ControlMethod::QueryAdvanced,
        None => ControlMethod::Unknown,
    }
}

fn telemetry_outcome_from_wire(
    response: &daemon::response_envelope::Response,
) -> (TelemetryOutcome, Option<ObservabilityErrorCode>) {
    let daemon::response_envelope::Response::Error(error) = response else {
        return (TelemetryOutcome::Succeeded, None);
    };
    let error_code = common::ErrorCode::try_from(error.code).ok();
    let outcome = match error_code {
        Some(common::ErrorCode::InvalidArgument)
        | Some(common::ErrorCode::PermissionDenied)
        | Some(common::ErrorCode::ProtocolMismatch)
        | Some(common::ErrorCode::Busy)
        | Some(common::ErrorCode::ResourceExhausted) => TelemetryOutcome::Rejected,
        Some(common::ErrorCode::Cancelled) => TelemetryOutcome::Cancelled,
        _ => TelemetryOutcome::Failed,
    };
    let stable_code = error_code.map(daemon_error_code_to_observability);
    (outcome, stable_code)
}

const fn daemon_error_code_to_observability(code: common::ErrorCode) -> ObservabilityErrorCode {
    match code {
        common::ErrorCode::InvalidArgument => ObservabilityErrorCode::InvalidArgument,
        common::ErrorCode::NotFound => ObservabilityErrorCode::NotFound,
        common::ErrorCode::Conflict => ObservabilityErrorCode::Conflict,
        common::ErrorCode::StaleGeneration => ObservabilityErrorCode::StaleGeneration,
        common::ErrorCode::UnsupportedCapability => ObservabilityErrorCode::UnsupportedCapability,
        common::ErrorCode::IncompleteCoverage => ObservabilityErrorCode::IncompleteCoverage,
        common::ErrorCode::BudgetExceeded => ObservabilityErrorCode::BudgetExceeded,
        common::ErrorCode::ResourceExhausted => ObservabilityErrorCode::ResourceExhausted,
        common::ErrorCode::Cancelled => ObservabilityErrorCode::Cancelled,
        common::ErrorCode::AdapterFailed => ObservabilityErrorCode::AdapterFailed,
        common::ErrorCode::IndexCorrupt => ObservabilityErrorCode::IndexCorrupt,
        common::ErrorCode::MigrationRequired => ObservabilityErrorCode::MigrationRequired,
        common::ErrorCode::PermissionDenied => ObservabilityErrorCode::PermissionDenied,
        common::ErrorCode::ProtocolMismatch => ObservabilityErrorCode::ProtocolMismatch,
        common::ErrorCode::Busy => ObservabilityErrorCode::Busy,
        common::ErrorCode::Internal | common::ErrorCode::Unspecified => {
            ObservabilityErrorCode::Internal
        }
    }
}

async fn run_diagnostic_request(
    service: ControlService,
    kind: DiagnosticKind,
    requested_timeout_ms: Option<u32>,
) -> daemon::response_envelope::Response {
    let started = Instant::now();
    let (method, span_kind) = match kind {
        DiagnosticKind::Quick => (ControlMethod::DiagnosticsQuick, SpanKind::DiagnosticsQuick),
        DiagnosticKind::SupportBundle(_) => (ControlMethod::SupportBundle, SpanKind::SupportBundle),
    };
    let span = service.state.telemetry.start_span(span_kind);
    let timeout = bounded_request_timeout(&service, requested_timeout_ms);
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return response_to_wire(ControlResponse::Error(request_timed_out()));
    };
    let Some(actor) = service.diagnostic_actor.as_ref() else {
        return response_to_wire(ControlResponse::Error(internal_error()));
    };
    let receiver = match actor.request(kind, deadline) {
        Ok(receiver) => receiver,
        Err(ServiceError::QueueFull) if matches!(kind, DiagnosticKind::SupportBundle(_)) => {
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
    let response =
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver).await {
            Ok(Ok(response)) => response_to_wire(response),
            Ok(Err(_)) => response_to_wire(ControlResponse::Error(internal_error())),
            Err(_) => response_to_wire(ControlResponse::Error(request_timed_out())),
        };
    let (outcome, error_code) = telemetry_outcome_from_wire(&response);
    service
        .state
        .telemetry
        .record_diagnostic(method, outcome, started.elapsed(), error_code);
    span.finish(outcome, error_code);
    response
}

async fn await_journal_response_until(
    response: impl std::future::Future<Output = Result<ControlResponse, ServiceError>>,
    deadline: Instant,
    queue_limit: u32,
) -> daemon::response_envelope::Response {
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), response).await {
        Ok(response) => journal_response_to_wire(response, queue_limit),
        Err(_) => response_to_wire(ControlResponse::Error(request_timed_out())),
    }
}

async fn await_claimed_journal_response(
    response: impl std::future::Future<Output = Result<ControlResponse, ServiceError>>,
    queue_limit: u32,
) -> daemon::response_envelope::Response {
    journal_response_to_wire(response.await, queue_limit)
}

fn journal_response_to_wire(
    response: Result<ControlResponse, ServiceError>,
    queue_limit: u32,
) -> daemon::response_envelope::Response {
    match response {
        Ok(response) => response_to_wire(response),
        Err(ServiceError::Operations(error)) => response_to_wire(ControlResponse::Error(
            operation_error_to_public(&error, None),
        )),
        Err(ServiceError::Public(error)) => response_to_wire(ControlResponse::Error(*error)),
        Err(ServiceError::QueueFull) => {
            response_to_wire(ControlResponse::Error(queue_full(queue_limit)))
        }
        Err(ServiceError::ClientOperationLimit { limit }) => {
            response_to_wire(ControlResponse::Error(client_operation_limit(limit)))
        }
        Err(ServiceError::RequestTimedOut) => {
            response_to_wire(ControlResponse::Error(request_timed_out()))
        }
        Err(_) => response_to_wire(ControlResponse::Error(internal_error())),
    }
}

fn bounded_request_timeout(
    service: &ControlService,
    requested_timeout_ms: Option<u32>,
) -> Duration {
    requested_timeout_ms.map_or(service.limits.request_timeout(), |milliseconds| {
        Duration::from_millis(u64::from(milliseconds)).min(service.limits.request_timeout())
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

const fn observability_daemon_lifecycle(
    lifecycle: DaemonLifecycle,
) -> ObservabilityDaemonLifecycle {
    match lifecycle {
        DaemonLifecycle::Starting => ObservabilityDaemonLifecycle::Starting,
        DaemonLifecycle::Ready => ObservabilityDaemonLifecycle::Ready,
        DaemonLifecycle::Draining => ObservabilityDaemonLifecycle::Draining,
        DaemonLifecycle::Faulted => ObservabilityDaemonLifecycle::Faulted,
        DaemonLifecycle::Stopped => ObservabilityDaemonLifecycle::Stopped,
    }
}

fn health_snapshot(health: &Health) -> HealthSnapshot {
    HealthSnapshot {
        ready: health.ready,
        lifecycle: observability_daemon_lifecycle(health.lifecycle),
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
            Ok(DecodedRequest::Control(ControlRequest::SupportBundle(
                if selected_protocol_minor >= 5 {
                    SupportBundleSchema::V3
                } else if selected_protocol_minor >= 4 {
                    SupportBundleSchema::V2
                } else {
                    SupportBundleSchema::V1
                },
            )))
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
        Some(
            daemon::request_envelope::Request::RepositoryIndex(_)
            | daemon::request_envelope::Request::RepositoryOperationStatus(_)
            | daemon::request_envelope::Request::CodeLocate(_)
            | daemon::request_envelope::Request::SymbolExplain(_)
            | daemon::request_envelope::Request::SourceRead(_),
        ) => Err(Box::new(first_slice_unavailable())),
        Some(
            daemon::request_envelope::Request::RepositoryList(_)
            | daemon::request_envelope::Request::RepositoryStatus(_)
            | daemon::request_envelope::Request::SymbolRelationships(_)
            | daemon::request_envelope::Request::FlowTrace(_)
            | daemon::request_envelope::Request::ArchitectureCycles(_),
        ) => Err(Box::new(first_slice_unavailable())),
        Some(daemon::request_envelope::Request::CodeDead(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::ArchitectureOverview(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::TestsSelect(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::ChangeImpact(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::PlanChange(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::HistoryCompare(_)) => {
            Err(Box::new(first_slice_unavailable()))
        }
        Some(daemon::request_envelope::Request::AdvancedQuery(_)) => {
            Err(Box::new(first_slice_unavailable()))
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

/// Converts one checked durable record into its stable protobuf representation.
#[must_use]
pub fn operation_record_to_wire(record: &OperationRecord) -> daemon::OperationStatus {
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
        OperationKind::RepositoryIndex => daemon::OperationKind::RepositoryIndex,
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
        OperationError::MutationTimedOut => (
            ErrorCode::Busy,
            "operation lifecycle mutation timed out",
            true,
        ),
        OperationError::StartDeadlineElapsed => {
            (ErrorCode::Busy, "operation start timed out", true)
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
        | OperationError::PublicErrorTooLarge
        | OperationError::CatalogTooLarge
        | OperationError::CatalogRowLimitExceeded => (
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
        | OperationError::CommittedStartCompensationFailed
        | OperationError::InsecureLockFile
        | OperationError::WindowsSecurityPolicy
        | OperationError::CatalogInspection(_)
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
                | OperationError::DiagnosticTimedOut
                | OperationError::MutationTimedOut
                | OperationError::StartDeadlineElapsed,
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
                | OperationError::CatalogTooLarge
                | OperationError::CatalogRowLimitExceeded
                | OperationError::RevisionOverflow
                | OperationError::UnsupportedCancellationReason
                | OperationError::MutexPoisoned
                | OperationError::SerializePublicError(_)
                | OperationError::SystemClockBeforeEpoch
                | OperationError::TimestampOverflow
                | OperationError::CommittedStartCompensationFailed
                | OperationError::InsecureLockFile
                | OperationError::WindowsSecurityPolicy
                | OperationError::CatalogInspection(_)
                | OperationError::Sqlite(_)
                | OperationError::LockIo(_),
            )
            | Self::UnexpectedResponse
            | Self::Clock
            | Self::TimerAlreadyRegistered
            | Self::TimerDeliveryTimedOut => true,
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

fn first_slice_unavailable() -> PublicError {
    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "first-slice daemon service is unavailable",
    )
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

/// Converts one checked public error into its stable protobuf representation.
#[must_use]
pub fn public_error_to_wire(error: &PublicError) -> common::PublicError {
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
    /// Daemon limits violated a hard bound, relationship, conversion, or allocation constraint.
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
    /// A due interruption was not durably acknowledged within the fixed delivery budget.
    #[error("operation timer delivery timed out")]
    TimerDeliveryTimedOut,
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
        write_client_hello_async, write_request_async,
    };
    use rootlight_operations::Progress;
    use std::{sync::mpsc, thread, time::Duration};
    use tempfile::{TempDir, tempdir};
    use tokio::io::AsyncWriteExt as _;

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

    fn manual_orchestrator(
        control_capacity: usize,
        request_timeout: Duration,
    ) -> (
        DaemonOrchestrator,
        Arc<DaemonState>,
        Receiver<JournalCommand>,
        Receiver<JournalCommand>,
    ) {
        let (control, control_rx) = mpsc::sync_channel(control_capacity);
        let (normal, normal_rx) = mpsc::sync_channel(4);
        let journal = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                control,
                normal,
            }))),
        };
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let limits = DaemonLimits::new(
            4,
            control_capacity,
            32,
            1,
            request_timeout,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("test limits are valid");
        let orchestrator = DaemonOrchestrator::new(journal, Arc::clone(&state), limits)
            .expect("orchestrator starts");
        (orchestrator, state, control_rx, normal_rx)
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
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\rootlight-daemon-core-{}-{}-{label}",
            std::process::id(),
            temporary.path().display().to_string().len()
        ));
        Endpoint::new(path).expect("endpoint is valid")
    }

    async fn connected_async_streams(label: &str) -> (TempDir, AsyncLocalStream, AsyncLocalStream) {
        let temporary = private_tempdir();
        let endpoint = endpoint_named(&temporary, label);
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("async listener binds");
        let (client, server) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(connect_async(&endpoint), listener.accept())
        })
        .await
        .expect("connection setup completes");
        (
            temporary,
            client.expect("client connects"),
            server.expect("connection accepts"),
        )
    }

    fn test_dispatch_context(
        service: &ControlService,
        envelope: &daemon::RequestEnvelope,
        client_instance_id: ClientInstanceId,
    ) -> AsyncDispatchContext {
        AsyncDispatchContext {
            client_instance_id,
            selected_protocol_minor: PROTOCOL_MINOR,
            cancellation: Cancellation::new(),
            timing: RequestTiming::start(service, envelope),
            index_admission: None,
        }
    }

    #[derive(Debug, Default)]
    struct DeadlineCapturingFirstSlice {
        observed: Mutex<Option<Instant>>,
    }

    impl FirstSliceIpcHandler for DeadlineCapturingFirstSlice {
        fn dispatch(
            &self,
            _request: FirstSliceIpcRequest,
            context: FirstSliceIpcContext,
        ) -> FirstSliceIpcFuture {
            *self
                .observed
                .lock()
                .expect("deadline capture lock is healthy") = Some(context.deadline);
            Box::pin(async { Err(first_slice_unavailable()) })
        }
    }

    #[derive(Debug, Default)]
    struct DisconnectRecordingFirstSlice {
        requests: Mutex<Vec<FirstSliceIpcRequest>>,
        admit_indexes: bool,
    }

    impl FirstSliceIpcHandler for DisconnectRecordingFirstSlice {
        fn dispatch(
            &self,
            request: FirstSliceIpcRequest,
            context: FirstSliceIpcContext,
        ) -> FirstSliceIpcFuture {
            let is_index = matches!(request, FirstSliceIpcRequest::RepositoryIndex(_));
            if is_index
                && self.admit_indexes
                && let Some(admission) = context.index_admission.as_ref()
            {
                admission.mark_inserted();
            }
            self.requests
                .lock()
                .expect("request capture lock is healthy")
                .push(request);
            if is_index {
                Box::pin(std::future::pending())
            } else {
                Box::pin(async { Err(first_slice_unavailable()) })
            }
        }
    }

    #[tokio::test]
    async fn peer_disconnect_cancels_attached_dispatch() {
        let (_temporary, client, mut server) = connected_async_streams("attached-disconnect").await;
        let cancellation = Cancellation::new();
        drop(client);

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            dispatch_while_peer_connected(
                std::future::pending::<daemon::ResponseEnvelope>(),
                &mut server,
                &cancellation,
                false,
                None,
            ),
        )
        .await
        .expect("peer disconnect is observed")
        .expect("peer EOF is accepted");

        assert!(response.is_none());
        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::ClientRequest)
        );
    }

    #[tokio::test]
    async fn detached_dispatch_survives_peer_disconnect() {
        let (_temporary, client, mut server) = connected_async_streams("detached-disconnect").await;
        let cancellation = Cancellation::new();
        drop(client);

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            dispatch_while_peer_connected(
                std::future::pending::<daemon::ResponseEnvelope>(),
                &mut server,
                &cancellation,
                true,
                None,
            ),
        )
        .await
        .expect("peer disconnect is observed")
        .expect("peer EOF is accepted");

        assert!(response.is_none());
        assert_eq!(cancellation.reason(), None);
    }

    #[tokio::test]
    async fn unexpected_peer_data_cancels_attached_dispatch() {
        let (_temporary, mut client, mut server) =
            connected_async_streams("unexpected-peer-data").await;
        let cancellation = Cancellation::new();
        client
            .write_all(&[0x5a])
            .await
            .expect("unexpected byte writes");

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            dispatch_while_peer_connected(
                std::future::pending::<daemon::ResponseEnvelope>(),
                &mut server,
                &cancellation,
                false,
                None,
            ),
        )
        .await
        .expect("peer data is observed")
        .expect_err("trailing data is rejected");

        assert!(matches!(error, IpcError::UnexpectedPeerData));
        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::ClientRequest)
        );
    }

    #[tokio::test]
    async fn buffered_peer_data_rejects_a_ready_response() {
        let (_temporary, mut client, mut server) =
            connected_async_streams("buffered-peer-data").await;
        let cancellation = Cancellation::new();
        let admission = FirstSliceAdmission::default();
        admission.mark_inserted();
        let (release, ready) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            client
                .write_all(&[0x5a])
                .await
                .expect("unexpected byte writes");
            release.send(()).expect("ready response releases");
        });
        let expected = daemon::ResponseEnvelope {
            request_id: 80,
            response: None,
        };

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            dispatch_while_peer_connected(
                async move {
                    ready.await.expect("ready response is released");
                    expected
                },
                &mut server,
                &cancellation,
                false,
                Some(&admission),
            ),
        )
        .await
        .expect("peer data is observed")
        .expect_err("buffered trailing data rejects a ready response");
        peer.await.expect("peer writer joins");

        assert!(matches!(error, IpcError::UnexpectedPeerData));
        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::ClientRequest)
        );
        assert_eq!(
            admission.claim_publication(),
            PublicationAdmission::Cancelled
        );
    }

    #[tokio::test]
    async fn completed_deadline_reason_wins_over_simultaneous_peer_close() {
        let (_temporary, client, mut server) =
            connected_async_streams("deadline-before-disconnect").await;
        let cancellation = Cancellation::with_deadline(Instant::now());
        drop(client);

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            dispatch_while_peer_connected(
                std::future::pending::<daemon::ResponseEnvelope>(),
                &mut server,
                &cancellation,
                false,
                None,
            ),
        )
        .await
        .expect("peer close is observed")
        .expect("peer EOF is accepted");

        assert!(response.is_none());
        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::DeadlineExceeded)
        );
    }

    #[tokio::test]
    async fn disconnected_retry_without_new_admission_does_not_cancel() {
        let handler = DisconnectRecordingFirstSlice::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        let operation = vec![82; 16];

        cancel_peer_abandoned_index(
            &handler,
            Some(IndexTransportCancellation {
                request: FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
                        operation: Some(common::OperationId { value: operation }),
                        action: daemon::RepositoryOperationAction::RepositoryOperationCancel as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                ),
                admission: FirstSliceAdmission::default(),
            }),
            ClientInstanceId::from_bytes([82; 16]),
            PROTOCOL_MINOR,
            Some(deadline),
        )
        .await;

        assert!(
            handler
                .requests
                .lock()
                .expect("request capture lock is healthy")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn attached_index_disconnect_uses_independent_cancel_lane() {
        let temporary = private_tempdir();
        let endpoint = endpoint_named(&temporary, "attached-index-cancel");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("async listener binds");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let limits = DaemonLimits::default();
        let service = Arc::new(ControlService::new(journal, [7; 16]));
        let client_connections = Arc::new(ClientConnectionAdmissions::new(limits));
        let (submissions, _receiver) = tokio::sync::mpsc::channel(4);
        let handler = Arc::new(DisconnectRecordingFirstSlice {
            requests: Mutex::new(Vec::new()),
            admit_indexes: true,
        });
        let server_handler = Arc::clone(&handler);
        let server_service = Arc::clone(&service);
        let server_connections = Arc::clone(&client_connections);
        let server_journal = actor.handle();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.expect("connection accepts");
            handle_connection_async_with_first_slice(
                server_service,
                server_journal,
                OrchestratorSenders::new(submissions),
                server_connections,
                server_handler,
                FrameCodec::default(),
                &mut stream,
            )
            .await
        });
        let mut client = connect_async(&endpoint).await.expect("client connects");
        write_client_hello_async(
            FrameCodec::default(),
            &mut client,
            &supported_hello(vec![7; 16]),
        )
        .await
        .expect("client hello writes");
        let hello = read_server_hello_async(FrameCodec::default(), &mut client)
            .await
            .expect("server hello reads");
        assert!(hello.error.is_none());
        let operation = vec![83; 16];
        write_request_async(
            FrameCodec::default(),
            &mut client,
            &daemon::RequestEnvelope {
                request_id: 83,
                instance_nonce: vec![7; 16],
                timeout_ms: Some(1_000),
                request: Some(daemon::request_envelope::Request::RepositoryIndex(
                    daemon::RepositoryIndexRequest {
                        schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
                        root: "fixture".to_owned(),
                        operation: Some(common::OperationId {
                            value: operation.clone(),
                        }),
                        detached: false,
                    },
                )),
            },
        )
        .await
        .expect("index request writes");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if handler
                    .requests
                    .lock()
                    .expect("request capture lock is healthy")
                    .len()
                    == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("index admission is observed before disconnect");
        drop(client);

        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("disconnected handler completes")
            .expect("server task joins")
            .expect("peer EOF is accepted");
        let requests = handler
            .requests
            .lock()
            .expect("request capture lock is healthy");
        assert_eq!(requests.len(), 2);
        assert!(matches!(
            &requests[0],
            FirstSliceIpcRequest::RepositoryIndex(request) if !request.detached
        ));
        assert!(matches!(
            &requests[1],
            FirstSliceIpcRequest::RepositoryOperationStatus(request)
                if request.action
                    == daemon::RepositoryOperationAction::RepositoryOperationCancel as i32
                    && request.operation.as_ref().map(|id| &id.value) == Some(&operation)
        ));
        drop(requests);
        actor.join().expect("actor joins");
    }

    #[tokio::test]
    async fn detached_index_trailing_data_uses_independent_cancel_lane() {
        let temporary = private_tempdir();
        let endpoint = endpoint_named(&temporary, "detached-index-protocol-error");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("async listener binds");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let limits = DaemonLimits::default();
        let service = Arc::new(ControlService::new(journal, [7; 16]));
        let client_connections = Arc::new(ClientConnectionAdmissions::new(limits));
        let (submissions, _receiver) = tokio::sync::mpsc::channel(4);
        let handler = Arc::new(DisconnectRecordingFirstSlice {
            requests: Mutex::new(Vec::new()),
            admit_indexes: true,
        });
        let server_handler = Arc::clone(&handler);
        let server_service = Arc::clone(&service);
        let server_connections = Arc::clone(&client_connections);
        let server_journal = actor.handle();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.expect("connection accepts");
            handle_connection_async_with_first_slice(
                server_service,
                server_journal,
                OrchestratorSenders::new(submissions),
                server_connections,
                server_handler,
                FrameCodec::default(),
                &mut stream,
            )
            .await
        });
        let mut client = connect_async(&endpoint).await.expect("client connects");
        write_client_hello_async(
            FrameCodec::default(),
            &mut client,
            &supported_hello(vec![7; 16]),
        )
        .await
        .expect("client hello writes");
        let hello = read_server_hello_async(FrameCodec::default(), &mut client)
            .await
            .expect("server hello reads");
        assert!(hello.error.is_none());
        let operation = vec![84; 16];
        write_request_async(
            FrameCodec::default(),
            &mut client,
            &daemon::RequestEnvelope {
                request_id: 84,
                instance_nonce: vec![7; 16],
                timeout_ms: Some(1_000),
                request: Some(daemon::request_envelope::Request::RepositoryIndex(
                    daemon::RepositoryIndexRequest {
                        schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
                        root: "fixture".to_owned(),
                        operation: Some(common::OperationId {
                            value: operation.clone(),
                        }),
                        detached: true,
                    },
                )),
            },
        )
        .await
        .expect("index request writes");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !handler
                    .requests
                    .lock()
                    .expect("request capture lock is healthy")
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached request reaches admission");
        client
            .write_all(&[0x5a])
            .await
            .expect("trailing byte writes");

        let error = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("protocol error completes")
            .expect("server task joins")
            .expect_err("trailing data fails closed");
        assert!(matches!(
            error,
            ServiceError::Ipc(IpcError::UnexpectedPeerData)
        ));
        let requests = handler
            .requests
            .lock()
            .expect("request capture lock is healthy");
        assert_eq!(requests.len(), 2);
        assert!(matches!(
            &requests[0],
            FirstSliceIpcRequest::RepositoryIndex(request) if request.detached
        ));
        assert!(matches!(
            &requests[1],
            FirstSliceIpcRequest::RepositoryOperationStatus(request)
                if request.action
                    == daemon::RepositoryOperationAction::RepositoryOperationCancel as i32
                    && request.operation.as_ref().map(|id| &id.value) == Some(&operation)
        ));
        drop(requests);
        actor.join().expect("actor joins");
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
    fn negotiated_capabilities_never_advertise_lease_renewal() {
        let service = service();
        for minor in MINIMUM_PROTOCOL_MINOR..=PROTOCOL_MINOR {
            let hello = daemon::ClientHello {
                supported_protocols: Some(common::VersionRange {
                    minimum: Some(common::ContractVersion { major: 1, minor }),
                    maximum: Some(common::ContractVersion { major: 1, minor }),
                }),
                capabilities: vec!["operation.lease.renew".to_owned()],
                expected_instance_nonce: vec![7; 16],
                client_instance_id: vec![9; 16],
            };

            let negotiated = service.negotiate(&hello);

            assert!(negotiated.error.is_none());
            assert_eq!(
                negotiated
                    .selected_protocol
                    .expect("supported minor negotiates"),
                common::ContractVersion { major: 1, minor }
            );
            let expected = if minor >= 5 {
                CAPABILITIES.to_vec()
            } else if minor >= 4 {
                vec![
                    "diagnostics.quick",
                    "health",
                    "operation.cancel",
                    "operation.lifecycle.v1",
                    "operation.status",
                    "operation.submit",
                    "support.bundle.v1",
                    "support.bundle.v2",
                ]
            } else if minor >= 3 {
                vec![
                    "diagnostics.quick",
                    "health",
                    "operation.cancel",
                    "operation.lifecycle.v1",
                    "operation.status",
                    "operation.submit",
                    "support.bundle.v1",
                ]
            } else {
                vec![
                    "health",
                    "operation.cancel",
                    "operation.lifecycle.v1",
                    "operation.status",
                    "operation.submit",
                ]
            };
            assert_eq!(negotiated.capabilities, expected);
            assert!(
                !negotiated
                    .capabilities
                    .iter()
                    .any(|capability| capability == "operation.lease.renew")
            );
        }
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

        let bundle = service.execute(ControlRequest::SupportBundle(SupportBundleSchema::V1));
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
            actor.handle().request(DiagnosticKind::Quick, deadline),
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
            actor.handle().request(DiagnosticKind::Quick, deadline),
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
                kind: DiagnosticKind::Quick,
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

        let first = run_diagnostic_request(service.clone(), DiagnosticKind::Quick, Some(100)).await;
        let daemon::response_envelope::Response::DiagnosticsQuick(first) = first else {
            panic!("first diagnostics response expected");
        };
        assert_eq!(first.results.len(), 1);
        assert_eq!(
            first.results[0].outcome,
            daemon::DiagnosticOutcome::Passed as i32
        );

        let next = run_diagnostic_request(service, DiagnosticKind::Quick, Some(100)).await;
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

    #[tokio::test]
    async fn async_dispatch_records_bounded_request_metrics_and_spans() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let state = Arc::new(DaemonState::starting());
        state.set_catalog_status(HealthStatus::Healthy);
        state.set_endpoint_status(HealthStatus::NotConfigured);
        state.set_lifecycle(DaemonLifecycle::Ready);
        let service = ControlService::with_state(
            journal,
            [7; 16],
            Arc::clone(&state),
            DaemonLimits::default(),
        );
        let (submissions, _receiver) = tokio::sync::mpsc::channel(4);
        let commands = OrchestratorSenders::new(submissions);
        let envelope = daemon::RequestEnvelope {
            request_id: 1,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(100),
            request: Some(daemon::request_envelope::Request::Health(
                daemon::HealthRequest {},
            )),
        };
        let context = test_dispatch_context(&service, &envelope, ClientInstanceId::SYSTEM);

        let response = dispatch_async(
            &service,
            &actor.handle(),
            &commands,
            &UnavailableFirstSliceIpcHandler,
            envelope,
            context,
        )
        .await;
        assert!(matches!(
            response.response,
            Some(daemon::response_envelope::Response::Health(_))
        ));

        let snapshot = state.telemetry().snapshot();
        let health = snapshot
            .metrics
            .ipc_requests
            .iter()
            .find(|metric| metric.method == ControlMethod::Health)
            .expect("health metric exists");
        assert_eq!(health.succeeded_total, 1);
        assert!(snapshot.traces.iter().any(|span| {
            span.kind
                == SpanKind::IpcRequest {
                    method: ControlMethod::Health,
                }
                && span.outcome == TelemetryOutcome::Succeeded
        }));
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_slice_dispatch_preserves_outer_absolute_deadline() {
        let handler = DeadlineCapturingFirstSlice::default();
        let deadline = Instant::now() + Duration::from_secs(1);
        let request = FirstSliceIpcRequest::RepositoryOperationStatus(
            daemon::RepositoryOperationStatusRequest {
                schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
                operation: Some(common::OperationId {
                    value: vec![92; 16],
                }),
                action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                wait_ms: None,
                after_revision: None,
            },
        );

        let response = dispatch_first_slice(
            &handler,
            request,
            ClientInstanceId::from_bytes([92; 16]),
            PROTOCOL_MINOR,
            deadline,
            Cancellation::new(),
            None,
        )
        .await;

        assert!(matches!(
            response,
            daemon::response_envelope::Response::Error(_)
        ));
        assert_eq!(
            *handler
                .observed
                .lock()
                .expect("deadline capture lock is healthy"),
            Some(deadline)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timed_out_cancel_is_abandoned_and_lease_renewal_is_unsupported() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let owner = ClientInstanceId::new([91; 16]).expect("client identity is valid");
        let cancelled = OperationId::from_bytes([90; 16]);
        journal
            .enqueue(cancelled)
            .expect("cancel operation enqueues");
        journal
            .start_execution(cancelled)
            .expect("cancel operation starts");
        let leased = OperationId::from_bytes([91; 16]);
        let lease_expiry = capture_admission_clock()
            .expect("admission clock reads")
            .wall_unix_ms
            .checked_add(60_000)
            .expect("lease expiry is representable");
        journal
            .submit(
                OperationSubmission::new(
                    leased,
                    OperationKind::ControlProbe,
                    PlanHash::from_bytes([91; 32]),
                    owner,
                    false,
                    None,
                    Some(lease_expiry),
                )
                .expect("lease submission is valid"),
            )
            .expect("lease operation submits");

        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("actor starts");
        let handle = actor.handle();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");

        let state = Arc::new(DaemonState::starting());
        state.set_catalog_status(HealthStatus::Healthy);
        state.set_lifecycle(DaemonLifecycle::Ready);
        let service = ControlService::with_state(
            Arc::clone(&journal),
            [7; 16],
            state,
            DaemonLimits::default(),
        );
        let (submissions, _receiver) = tokio::sync::mpsc::channel(4);
        let commands = OrchestratorSenders::new(submissions);
        let cancel_envelope = daemon::RequestEnvelope {
            request_id: 90,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(20),
            request: Some(daemon::request_envelope::Request::OperationCancel(
                daemon::OperationCancelRequest {
                    operation: Some(common::OperationId {
                        value: cancelled.as_bytes().to_vec(),
                    }),
                },
            )),
        };
        let cancel_context = test_dispatch_context(&service, &cancel_envelope, owner);
        let cancel_response = dispatch_async(
            &service,
            &handle,
            &commands,
            &UnavailableFirstSliceIpcHandler,
            cancel_envelope,
            cancel_context,
        )
        .await;
        assert!(matches!(
            cancel_response.response,
            Some(daemon::response_envelope::Response::Error(error))
                if error.code == common::ErrorCode::Busy as i32
        ));

        let renew_envelope = daemon::RequestEnvelope {
            request_id: 91,
            instance_nonce: vec![7; 16],
            timeout_ms: Some(20),
            request: Some(daemon::request_envelope::Request::OperationLeaseRenew(
                daemon::OperationLeaseRenewRequest {
                    operation: Some(common::OperationId {
                        value: leased.as_bytes().to_vec(),
                    }),
                    lease_expires_unix_ms: lease_expiry + 60_000,
                },
            )),
        };
        let renew_context = test_dispatch_context(&service, &renew_envelope, owner);
        let renew_response = dispatch_async(
            &service,
            &handle,
            &commands,
            &UnavailableFirstSliceIpcHandler,
            renew_envelope,
            renew_context,
        )
        .await;
        assert!(matches!(
            renew_response.response,
            Some(daemon::response_envelope::Response::Error(error))
                if error.code == common::ErrorCode::UnsupportedCapability as i32
        ));

        release_tx.send(()).expect("actor resumes");
        handle
            .control(ControlRequest::Health)
            .await
            .expect("control-lane barrier completes");
        let cancelled_record = journal.status(cancelled).expect("cancel state loads");
        assert_eq!(cancelled_record.state, OperationState::Running);
        assert!(!cancelled_record.cancellation_requested);
        assert_eq!(
            journal
                .status(leased)
                .expect("lease state loads")
                .lease_expires_unix_ms,
            Some(lease_expiry)
        );
        actor.join().expect("actor joins");
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
    fn journal_actor_drop_drains_buffered_durable_commands() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([10; 16]);
        journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(Arc::clone(&journal), 2, 1).expect("actor starts");
        let handle = actor.handle();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: entered_tx,
                    release: release_rx,
                },
            )
            .expect("barrier queues");
        entered_rx.recv().expect("actor enters barrier");
        let response = handle
            .interrupt_deadline_receiver(operation)
            .expect("durable interrupt queues");

        handle.begin_drain();
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        assert!(matches!(
            handle.try_send(JournalLane::Control, JournalCommand::Checkpoint { reply }),
            Err(ServiceError::ChannelClosed)
        ));
        let dropped = thread::spawn(move || drop(actor));
        release_tx.send(()).expect("actor barrier releases");
        dropped.join().expect("actor owner drops");

        let observed = response
            .blocking_recv()
            .expect("accepted command returns")
            .expect("durable interrupt succeeds");
        assert_eq!(observed.state, OperationState::Interrupted);
        assert_eq!(
            journal
                .status(operation)
                .expect("durable state remains readable")
                .state,
            OperationState::Interrupted
        );
    }

    #[test]
    fn journal_actor_drain_rejects_new_commands_and_joins_with_full_lane() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 1, 1).expect("actor starts");
        let handle = actor.handle();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: entered_tx,
                    release: release_rx,
                },
            )
            .expect("barrier queues");
        entered_rx.recv().expect("actor enters barrier");
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        handle
            .try_send(JournalLane::Control, JournalCommand::Checkpoint { reply })
            .expect("control lane fills");
        handle.begin_drain();
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        assert!(matches!(
            handle.try_send(JournalLane::Control, JournalCommand::Checkpoint { reply }),
            Err(ServiceError::ChannelClosed)
        ));
        release_tx.send(()).expect("actor barrier releases");
        actor.join().expect("actor drains and joins");
    }

    #[test]
    fn full_normal_lane_preserves_worker_authorization_for_retry() {
        let (control, _control_rx) = mpsc::sync_channel(1);
        let (normal, normal_rx) = mpsc::sync_channel(1);
        let handle = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                control,
                normal,
            }))),
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
        let (_acknowledged, acknowledgement) = mpsc::sync_channel(1);
        let command = JournalCommand::StartOperation {
            operation,
            deadline: WorkerDeadline::from_timeout(DEFAULT_REQUEST_TIMEOUT)
                .expect("deadline is valid"),
            started,
            acknowledged: acknowledgement,
        };
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
    fn worker_start_handshake_bounds_saturation_and_actor_close() {
        let (control, _control_rx) = mpsc::sync_channel(1);
        let (normal, normal_rx) = mpsc::sync_channel(1);
        let handle = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                control,
                normal,
            }))),
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
        let saturated_deadline = WorkerDeadline::with_remaining_checks(DEFAULT_REQUEST_TIMEOUT, 1)
            .expect("deadline is valid");

        assert!(matches!(
            handle.start_operation_blocking(OperationId::from_bytes([58; 16]), &saturated_deadline),
            Err(ServiceError::QueueFull)
        ));
        assert!(matches!(
            normal_rx.try_recv(),
            Ok(JournalCommand::Checkpoint { .. })
        ));
        assert!(matches!(normal_rx.try_recv(), Err(TryRecvError::Empty)));

        handle.begin_drain();
        let deadline =
            WorkerDeadline::from_timeout(DEFAULT_REQUEST_TIMEOUT).expect("deadline is valid");
        assert!(matches!(
            handle.start_operation_blocking(OperationId::from_bytes([59; 16]), &deadline),
            Err(ServiceError::ChannelClosed)
        ));
    }

    #[test]
    fn worker_start_handshake_rejects_a_stale_admitted_command() {
        let operation = OperationId::from_bytes([60; 16]);
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        journal.enqueue(operation).expect("operation enqueues");
        let (control, _control_rx) = mpsc::sync_channel(1);
        let (normal, normal_rx) = mpsc::sync_channel(1);
        let handle = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                control,
                normal,
            }))),
        };
        let deadline = WorkerDeadline::with_remaining_checks(DEFAULT_REQUEST_TIMEOUT, 2)
            .expect("deadline is valid");

        assert!(matches!(
            handle.start_operation_blocking(operation, &deadline),
            Err(ServiceError::RequestTimedOut)
        ));
        let command = normal_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("start command remains admitted");
        execute_journal_command(&journal, command).expect("journal command executes");

        assert_eq!(
            journal.status(operation).expect("status loads").state,
            OperationState::Queued
        );
    }

    #[test]
    fn worker_start_acknowledgement_loss_interrupts_durable_running_state() {
        let operation = OperationId::from_bytes([62; 16]);
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        journal.enqueue(operation).expect("operation enqueues");
        let deadline =
            WorkerDeadline::from_timeout(DEFAULT_REQUEST_TIMEOUT).expect("deadline is valid");
        let (started, receiver) = mpsc::sync_channel(0);
        let (_acknowledged, acknowledgement) = mpsc::sync_channel(0);
        drop(receiver);

        execute_journal_command(
            &journal,
            JournalCommand::StartOperation {
                operation,
                deadline,
                started,
                acknowledged: acknowledgement,
            },
        )
        .expect("lost acknowledgement is compensated");

        let operation = journal.status(operation).expect("status loads");
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(
            operation.recovery_class,
            RecoveryClass::InterruptedByRestart
        );
    }

    #[test]
    fn worker_acknowledgement_disconnect_preserves_restart_provenance() {
        let operation = OperationId::from_bytes([66; 16]);
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let running = journal
            .start_execution(operation)
            .expect("operation starts");
        let deadline =
            WorkerDeadline::from_timeout(DEFAULT_REQUEST_TIMEOUT).expect("deadline is valid");
        let (started, start_receiver) = mpsc::sync_channel(1);
        let (acknowledged, acknowledgement) = mpsc::sync_channel(0);
        drop(acknowledged);

        deliver_worker_start(
            &journal,
            operation,
            &deadline,
            started,
            acknowledgement,
            Ok((running, Some(cancellation.clone()))),
        )
        .expect("disconnected acknowledgement is compensated");

        assert!(matches!(start_receiver.recv(), Ok(Ok(_))));
        let operation = journal.status(operation).expect("status loads");
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(
            operation.recovery_class,
            RecoveryClass::InterruptedByRestart
        );
        assert_eq!(
            cancellation.reason(),
            Some(rootlight_operations::CancellationReason::Shutdown)
        );
    }

    #[test]
    fn worker_start_delivery_does_not_wait_for_worker_receive() {
        let operation = OperationId::from_bytes([67; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let handle = actor.handle();
        let ControlledStartReceive {
            deadline,
            force_expired,
            entered,
            release,
        } = WorkerDeadline::controlled_before_start_receive(DEFAULT_REQUEST_TIMEOUT)
            .expect("deadline is valid");
        let worker_handle = handle.clone();
        let worker =
            thread::spawn(move || worker_handle.start_operation_blocking(operation, &deadline));

        entered
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("worker reaches the start receive boundary");
        let durable_start_observed = (0..100).any(|_| {
            if journal
                .status(operation)
                .is_ok_and(|record| record.state == OperationState::Running)
            {
                true
            } else {
                thread::sleep(Duration::from_millis(1));
                false
            }
        });
        assert!(
            durable_start_observed,
            "actor must deliver the buffered start before worker receive"
        );
        force_expired.store(true, Ordering::Release);

        let (sentinel_entered, sentinel_receiver) = mpsc::sync_channel(0);
        let (sentinel_release, sentinel_release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: sentinel_entered,
                    release: sentinel_release_receiver,
                },
            )
            .expect("control sentinel queues");
        sentinel_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("actor remains responsive after worker deadline");
        sentinel_release
            .send(())
            .expect("control sentinel releases");
        release.send(()).expect("start receive boundary releases");

        assert!(matches!(
            worker.join().expect("worker joins"),
            Err(ServiceError::RequestTimedOut)
        ));
        let operation = journal.status(operation).expect("status loads");
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(operation.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(
            cancellation.reason(),
            Some(rootlight_operations::CancellationReason::DeadlineExceeded)
        );
        actor.join().expect("actor joins");
    }

    #[test]
    fn worker_start_expiry_after_rendezvous_receipt_prevents_execution() {
        let operation = OperationId::from_bytes([63; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let handle = actor.handle();
        let ControlledStartReceipt {
            deadline,
            force_expired,
            entered,
            release,
        } = WorkerDeadline::controlled_at_start_receipt(DEFAULT_REQUEST_TIMEOUT)
            .expect("deadline is valid");
        let worker_handle = handle.clone();
        let worker =
            thread::spawn(move || worker_handle.start_operation_blocking(operation, &deadline));

        entered
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("worker reaches the start receipt boundary");
        force_expired.store(true, Ordering::Release);
        release.send(()).expect("start receipt boundary releases");

        assert!(matches!(
            worker.join().expect("worker joins"),
            Err(ServiceError::RequestTimedOut)
        ));
        let (entered, entered_receiver) = mpsc::sync_channel(0);
        let (release, release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered,
                    release: release_receiver,
                },
            )
            .expect("actor barrier queues");
        entered_receiver
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("actor reaches barrier");
        release.send(()).expect("actor barrier releases");

        let operation = journal.status(operation).expect("status loads");
        assert_eq!(operation.state, OperationState::Interrupted);
        assert_eq!(operation.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(
            cancellation.reason(),
            Some(rootlight_operations::CancellationReason::DeadlineExceeded)
        );
        actor.join().expect("actor joins");
    }

    #[test]
    fn stale_start_acknowledgement_cannot_authorize_after_deadline() {
        let operation = OperationId::from_bytes([65; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let handle = actor.handle();
        let ControlledStartAcknowledgement {
            deadline,
            force_expired,
            entered,
            release,
        } = WorkerDeadline::controlled_before_start_acknowledgement(DEFAULT_REQUEST_TIMEOUT)
            .expect("deadline is valid");
        let worker_handle = handle.clone();
        let worker =
            thread::spawn(move || worker_handle.start_operation_blocking(operation, &deadline));

        entered
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("worker reaches the start acknowledgement boundary");
        force_expired.store(true, Ordering::Release);
        release
            .send(())
            .expect("start acknowledgement boundary releases");

        assert!(matches!(
            worker.join().expect("worker joins"),
            Err(ServiceError::RequestTimedOut)
        ));
        let (sentinel_entered, sentinel_receiver) = mpsc::sync_channel(0);
        let (sentinel_release, sentinel_release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: sentinel_entered,
                    release: sentinel_release_receiver,
                },
            )
            .expect("control sentinel queues");
        sentinel_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("actor processes the stale acknowledgement");
        sentinel_release
            .send(())
            .expect("control sentinel releases");

        let interrupted = journal.status(operation).expect("status loads");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(interrupted.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(
            cancellation.reason(),
            Some(rootlight_operations::CancellationReason::DeadlineExceeded)
        );
        actor.join().expect("actor joins");
    }

    #[test]
    fn real_deadline_at_start_acknowledgement_never_authorizes_work() {
        let operation = OperationId::from_bytes([80; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let handle = actor.handle();
        let ControlledStartAcknowledgement {
            deadline,
            force_expired: _,
            entered,
            release,
        } = WorkerDeadline::controlled_before_start_acknowledgement(DEFAULT_REQUEST_TIMEOUT)
            .expect("deadline is valid");
        let expires_at = deadline.expires_at();
        let worker_handle = handle.clone();
        let worker =
            thread::spawn(move || worker_handle.start_operation_blocking(operation, &deadline));

        entered
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("worker reaches the real deadline boundary");
        while Instant::now() < expires_at {
            thread::sleep(expires_at.saturating_duration_since(Instant::now()));
        }
        release.send(()).expect("real deadline boundary releases");

        assert!(matches!(
            worker.join().expect("worker joins"),
            Err(ServiceError::RequestTimedOut)
        ));
        let (sentinel_entered, sentinel_receiver) = mpsc::sync_channel(0);
        let (sentinel_release, sentinel_release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: sentinel_entered,
                    release: sentinel_release_receiver,
                },
            )
            .expect("control sentinel queues");
        sentinel_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("actor settles the real deadline");
        sentinel_release
            .send(())
            .expect("control sentinel releases");

        let interrupted = journal.status(operation).expect("status loads");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(interrupted.recovery_class, RecoveryClass::DeadlineElapsed);
        assert_eq!(
            cancellation.reason(),
            Some(rootlight_operations::CancellationReason::DeadlineExceeded)
        );
        actor.join().expect("actor joins");
    }

    #[test]
    fn committed_start_compensation_failure_stops_the_journal_actor() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 1, 2).expect("actor starts");
        let handle = actor.handle();
        let (barrier_entered, barrier_entered_receiver) = mpsc::sync_channel(0);
        let (barrier_release, barrier_release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Barrier {
                    entered: barrier_entered,
                    release: barrier_release_receiver,
                },
            )
            .expect("barrier queues");
        barrier_entered_receiver
            .recv()
            .expect("actor enters barrier");
        let deadline =
            WorkerDeadline::from_timeout(DEFAULT_REQUEST_TIMEOUT).expect("deadline is valid");
        let (started, receiver) = mpsc::sync_channel(0);
        let (_acknowledged, acknowledgement) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Normal,
                JournalCommand::DeliverStart {
                    operation: OperationId::from_bytes([64; 16]),
                    deadline,
                    started,
                    acknowledged: acknowledgement,
                    result: Box::new(Err(ServiceError::Operations(
                        OperationError::CommittedStartCompensationFailed,
                    ))),
                },
            )
            .expect("faulting delivery queues");
        let (sentinel_entered, sentinel_receiver) = mpsc::sync_channel(0);
        let (_sentinel_release, sentinel_release_receiver) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Normal,
                JournalCommand::Barrier {
                    entered: sentinel_entered,
                    release: sentinel_release_receiver,
                },
            )
            .expect("sentinel queues behind the fault");

        barrier_release.send(()).expect("barrier releases");

        assert!(matches!(receiver.recv(), Err(mpsc::RecvError)));
        assert!(matches!(sentinel_receiver.recv(), Err(mpsc::RecvError)));
        actor.join().expect("faulted actor joins");
    }

    #[tokio::test]
    async fn worker_admission_and_completion_release_owned_permits() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let closed_handle = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Draining)),
        };
        let mut pool = SyntheticWorkerPool::start(1, 1).expect("worker pool starts");
        let permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            ClientInstanceId::SYSTEM,
            3,
            3,
        )
        .expect("permit reserves");
        let (admitted_tx, admitted_rx) = mpsc::sync_channel(1);
        pool.submit(WorkerJob {
            operation: OperationId::from_bytes([61; 16]),
            admitted: admitted_rx,
            handshake_timeout: DEFAULT_REQUEST_TIMEOUT,
            journal: closed_handle,
            permit,
            started: None,
        })
        .expect("job submits");
        drop(admitted_tx);
        pool.join_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .expect("disconnected admission cannot stall join");
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);

        let (completion_tx, completions) = tokio::sync::mpsc::channel(1);
        let first_permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            ClientInstanceId::SYSTEM,
            3,
            3,
        )
        .expect("first permit reserves");
        assert!(deliver_worker_completion(
            &completion_tx,
            WorkerCompletion {
                operation: OperationId::from_bytes([62; 16]),
                start: Err(ServiceError::RequestTimedOut),
                cancellation_reason: None,
                permit: first_permit,
            }
        ));
        let second_permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            Arc::clone(&client_admissions),
            ClientInstanceId::SYSTEM,
            3,
            3,
        )
        .expect("second permit reserves");
        assert!(!deliver_worker_completion(
            &completion_tx,
            WorkerCompletion {
                operation: OperationId::from_bytes([63; 16]),
                start: Err(ServiceError::RequestTimedOut),
                cancellation_reason: None,
                permit: second_permit,
            }
        ));
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 1);
        drop(completions);
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);

        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
        drop(closed_rx);
        let closed_permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            client_admissions,
            ClientInstanceId::SYSTEM,
            3,
            3,
        )
        .expect("closed-channel permit reserves");
        assert!(!deliver_worker_completion(
            &closed_tx,
            WorkerCompletion {
                operation: OperationId::from_bytes([64; 16]),
                start: Err(ServiceError::ChannelClosed),
                cancellation_reason: None,
                permit: closed_permit,
            }
        ));
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
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
            Arc::clone(&service),
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
        let telemetry = service.state.telemetry().snapshot();
        assert!(telemetry.traces.iter().any(|span| {
            span.kind == SpanKind::IpcNegotiation && span.outcome == TelemetryOutcome::Succeeded
        }));
        assert!(telemetry.traces.iter().any(|span| {
            span.kind == SpanKind::IpcNegotiation
                && span.outcome == TelemetryOutcome::Rejected
                && span.error_code == Some(ObservabilityErrorCode::ResourceExhausted)
        }));
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
    fn daemon_limits_enforce_hard_resource_caps() {
        assert!(
            DaemonLimits::new_with_client_limits(
                MAX_CONNECTION_LIMIT,
                MAX_CONNECTION_LIMIT,
                MAX_CONTROL_QUEUE_LIMIT,
                MAX_OPERATION_QUEUE_LIMIT,
                MAX_OPERATION_QUEUE_LIMIT,
                MAX_OPERATION_WORKERS,
                MAX_REQUEST_TIMEOUT,
                MAX_MAINTENANCE_INTERVAL,
                MAX_SHUTDOWN_GRACE,
            )
            .is_ok()
        );

        for invalid in [
            DaemonLimits::new(
                u32::MAX,
                1,
                1,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                usize::MAX,
                1,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                1,
                u32::MAX,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                1,
                1,
                usize::MAX,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                1,
                1,
                1,
                Duration::MAX,
                Duration::from_secs(1),
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                1,
                1,
                1,
                Duration::from_secs(1),
                Duration::MAX,
                Duration::from_secs(1),
            ),
            DaemonLimits::new(
                1,
                1,
                1,
                1,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::MAX,
            ),
        ] {
            assert!(matches!(invalid, Err(ServiceError::InvalidLimits)));
        }
    }

    #[test]
    fn shutdown_rounds_cover_the_hard_ceiling_and_empty_confirmation() {
        for (maximum, expected) in [
            (0, 1),
            (1, 2),
            (255, 2),
            (256, 2),
            (257, 3),
            (rootlight_operations::MAX_NONTERMINAL_OPERATIONS, 257),
        ] {
            assert_eq!(
                shutdown_interrupt_rounds(maximum).expect("hard ceiling is representable"),
                expected
            );
        }
    }

    #[test]
    fn actors_reject_hostile_capacities_without_allocating() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        assert!(matches!(
            JournalActor::start(Arc::clone(&journal), usize::MAX, 1),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            JournalActor::start(Arc::clone(&journal), 1, usize::MAX),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            SyntheticWorkerPool::start(usize::MAX, 1),
            Err(ServiceError::InvalidLimits)
        ));
        assert!(matches!(
            SyntheticWorkerPool::start(1, usize::MAX),
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

        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let actor_response = execute_journal_request(
            &journal,
            ControlRequest::OperationLeaseRenew {
                operation,
                owner,
                expiry_unix_ms: 1,
            },
        )
        .expect("actor returns a public compatibility error");
        let ControlResponse::Error(error) = actor_response else {
            panic!("journal actor renewal must remain unsupported");
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_retries_durable_state_after_pending_handoff_wins() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::default();
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let initial_not_found = Arc::new(tokio::sync::Barrier::new(2));
        let resume = Arc::new(tokio::sync::Barrier::new(2));
        let mut control_service =
            ControlService::with_state(Arc::clone(&journal), [7; 16], Arc::clone(&state), limits);
        control_service.cancellation_handoff_hook = Some(CancellationHandoffTestHook {
            initial_not_found: Arc::clone(&initial_not_found),
            resume: Arc::clone(&resume),
        });
        let service = Arc::new(control_service);
        let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        let (submissions, mut submission_rx) = tokio::sync::mpsc::channel(1);
        let commands = OrchestratorSenders::new(submissions);
        let operation = OperationId::from_bytes([44; 16]);
        let owner = ClientInstanceId::new([44; 16]).expect("client identity is valid");

        // Dropping either set aborts its dispatch if a bounded rendezvous fails.
        let mut submission_dispatches = tokio::task::JoinSet::new();
        {
            let service = Arc::clone(&service);
            let actor = actor.handle();
            let commands = commands.clone();
            submission_dispatches.spawn(async move {
                let envelope = daemon::RequestEnvelope {
                    request_id: 1,
                    instance_nonce: vec![7; 16],
                    timeout_ms: None,
                    request: Some(daemon::request_envelope::Request::OperationSubmit(
                        daemon::OperationSubmitRequest {
                            operation: Some(common::OperationId {
                                value: operation.as_bytes().to_vec(),
                            }),
                            kind: daemon::OperationKind::ControlProbe as i32,
                            plan_hash: CONTROL_PROBE_PLAN_HASH.to_vec(),
                            detached: true,
                            timeout_ms: None,
                            deadline_unix_ms: None,
                            lease_expires_unix_ms: None,
                        },
                    )),
                };
                let context = test_dispatch_context(service.as_ref(), &envelope, owner);
                dispatch_async(
                    service.as_ref(),
                    &actor,
                    &commands,
                    &UnavailableFirstSliceIpcHandler,
                    envelope,
                    context,
                )
                .await
            });
        }
        let admission =
            tokio::time::timeout(CANCELLATION_HANDOFF_TEST_TIMEOUT, submission_rx.recv())
                .await
                .expect("submission dispatch did not reach the pending lane")
                .expect("submission lane closed before receiving the admission");
        let mut cancellation_dispatches = tokio::task::JoinSet::new();
        {
            let service = Arc::clone(&service);
            let actor = actor.handle();
            let commands = commands.clone();
            cancellation_dispatches.spawn(async move {
                let envelope = daemon::RequestEnvelope {
                    request_id: 2,
                    instance_nonce: vec![7; 16],
                    timeout_ms: None,
                    request: Some(daemon::request_envelope::Request::OperationCancel(
                        daemon::OperationCancelRequest {
                            operation: Some(common::OperationId {
                                value: operation.as_bytes().to_vec(),
                            }),
                        },
                    )),
                };
                let context = test_dispatch_context(service.as_ref(), &envelope, owner);
                dispatch_async(
                    service.as_ref(),
                    &actor,
                    &commands,
                    &UnavailableFirstSliceIpcHandler,
                    envelope,
                    context,
                )
                .await
            });
        }

        tokio::time::timeout(CANCELLATION_HANDOFF_TEST_TIMEOUT, initial_not_found.wait())
            .await
            .expect("cancellation dispatch did not reach the initial durable NotFound");
        orchestrator
            .submit(admission)
            .await
            .expect("pending submission becomes durable");
        assert!(
            !service
                .pending_admissions
                .lock()
                .expect("pending registry is available")
                .by_operation
                .contains_key(&operation)
        );
        tokio::time::timeout(CANCELLATION_HANDOFF_TEST_TIMEOUT, resume.wait())
            .await
            .expect("cancellation dispatch did not rendezvous after the durable handoff");

        let cancellation = tokio::time::timeout(
            CANCELLATION_HANDOFF_TEST_TIMEOUT,
            cancellation_dispatches.join_next(),
        )
        .await
        .expect("cancellation dispatch did not complete after the durable handoff")
        .expect("cancellation dispatch task is present")
        .expect("cancellation dispatch task joins");
        let Some(daemon::response_envelope::Response::OperationCancel(cancellation)) =
            cancellation.response
        else {
            panic!("durable retry must acknowledge cancellation");
        };
        assert!(cancellation.accepted);
        assert!(
            cancellation
                .operation
                .expect("cancelled operation is present")
                .cancellation_requested
        );
        assert!(
            journal
                .status(operation)
                .expect("durable operation loads")
                .cancellation_requested
        );
        assert!(matches!(
            tokio::time::timeout(
                CANCELLATION_HANDOFF_TEST_TIMEOUT,
                submission_dispatches.join_next(),
            )
            .await
            .expect("submission dispatch did not complete after becoming durable")
            .expect("submission dispatch task is present")
            .expect("submission dispatch task joins")
            .response,
            Some(daemon::response_envelope::Response::OperationSubmit(_))
        ));

        orchestrator
            .shutdown()
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_timeout_includes_the_journal_wait() {
        let service = service();
        let (control, control_rx) = mpsc::sync_channel(1);
        let (normal, _normal_rx) = mpsc::sync_channel(1);
        let journal = JournalActorHandle {
            state: Arc::new(Mutex::new(JournalActorState::Accepting(JournalSenders {
                control,
                normal,
            }))),
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
        let context = test_dispatch_context(
            &service,
            &envelope,
            ClientInstanceId::new([9; 16]).expect("client identity is valid"),
        );

        let response = tokio::time::timeout(
            Duration::from_millis(100),
            dispatch_async(
                &service,
                &journal,
                &commands,
                &UnavailableFirstSliceIpcHandler,
                envelope,
                context,
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
    async fn deadline_schedule_is_inclusive_and_serializes_every_due_timer() {
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
                .take_next_due(now + Duration::from_millis(99))
                .is_none()
        );
        let mut due = 0;
        while timers
            .take_next_due(now + Duration::from_millis(100))
            .is_some()
        {
            due += 1;
        }
        assert_eq!(due, 20);
        assert!(timers.by_timer.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn due_timer_queue_saturation_faults_without_dropping_the_timer() {
        let (mut orchestrator, state, control_rx, _normal_rx) =
            manual_orchestrator(1, Duration::from_millis(10));
        let now = tokio::time::Instant::now();
        let operation = OperationId::from_bytes([50; 16]);
        orchestrator
            .timers
            .register(
                ScheduledTimer {
                    operation,
                    reason: TimerReason::Deadline,
                },
                now,
            )
            .expect("timer registers");
        let (reply, _receiver) = tokio::sync::oneshot::channel();
        orchestrator
            .journal
            .try_send(JournalLane::Control, JournalCommand::Checkpoint { reply })
            .expect("control lane fills");

        let error = orchestrator
            .process_event(OrchestratorEvent {
                kind: OrchestratorEventKind::Timer,
            })
            .await
            .expect_err("saturation faults instead of retrying forever");

        assert!(matches!(error, ServiceError::QueueFull));
        assert_eq!(orchestrator.timers.next_deadline(), Some(now));
        assert!(orchestrator.pending_timer.is_none());
        assert_eq!(state.lifecycle(), DaemonLifecycle::Faulted);
        assert!(matches!(
            control_rx.try_recv(),
            Ok(JournalCommand::Checkpoint { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn due_timer_channel_failure_faults_without_dropping_the_timer() {
        let (mut orchestrator, state, control_rx, _normal_rx) =
            manual_orchestrator(1, Duration::from_millis(10));
        drop(control_rx);
        let now = tokio::time::Instant::now();
        let operation = OperationId::from_bytes([51; 16]);
        orchestrator
            .timers
            .register(
                ScheduledTimer {
                    operation,
                    reason: TimerReason::Deadline,
                },
                now,
            )
            .expect("timer registers");

        let error = orchestrator
            .process_event(OrchestratorEvent {
                kind: OrchestratorEventKind::Timer,
            })
            .await
            .expect_err("closed actor lane faults immediately");

        assert!(matches!(error, ServiceError::ChannelClosed));
        assert_eq!(orchestrator.timers.next_deadline(), Some(now));
        assert!(orchestrator.pending_timer.is_none());
        assert_eq!(state.lifecycle(), DaemonLifecycle::Faulted);
    }

    #[tokio::test]
    async fn timer_actor_failure_restores_the_due_timer_and_faults() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 1, 1).expect("actor starts");
        let mut orchestrator =
            DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), DaemonLimits::default())
                .expect("orchestrator starts");
        let now = tokio::time::Instant::now();
        orchestrator
            .timers
            .register(
                ScheduledTimer {
                    operation: OperationId::from_bytes([52; 16]),
                    reason: TimerReason::Deadline,
                },
                now,
            )
            .expect("timer registers");

        orchestrator
            .process_event(OrchestratorEvent {
                kind: OrchestratorEventKind::Timer,
            })
            .await
            .expect("delivery enters the actor lane");
        let event = orchestrator
            .next_event()
            .await
            .expect("actor failure becomes ready");
        let error = orchestrator
            .process_event(event)
            .await
            .expect_err("missing durable operation faults delivery");

        assert!(
            matches!(&error, ServiceError::Operations(OperationError::NotFound)),
            "unexpected timer actor failure: {error:?}"
        );
        assert_eq!(orchestrator.timers.next_deadline(), Some(now));
        assert_eq!(state.lifecycle(), DaemonLifecycle::Faulted);
        orchestrator
            .shutdown()
            .await
            .expect("healthy actor still permits bounded shutdown");
        actor.join().expect("actor joins");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_bounds_an_unacknowledged_timer_without_losing_its_command() {
        let (mut orchestrator, state, control_rx, _normal_rx) =
            manual_orchestrator(1, Duration::from_millis(10));
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let operation = OperationId::from_bytes([53; 16]);
        journal.enqueue(operation).expect("operation enqueues");
        orchestrator.active_operations.insert(
            operation,
            journal.status(operation).expect("operation is durable"),
        );
        state.set_operation_counts(1, 1, 0);
        let deadline = tokio::time::Instant::now();
        orchestrator
            .timers
            .register(
                ScheduledTimer {
                    operation,
                    reason: TimerReason::Deadline,
                },
                deadline,
            )
            .expect("timer registers");
        orchestrator
            .process_event(OrchestratorEvent {
                kind: OrchestratorEventKind::Timer,
            })
            .await
            .expect("timer command enters the actor lane");

        let started = tokio::time::Instant::now();
        let error = orchestrator
            .shutdown_until(started + Duration::from_millis(10))
            .await
            .expect_err("unresponsive actor faults bounded shutdown");

        assert!(matches!(error, ServiceError::TimerDeliveryTimedOut));
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_millis(10)
        );
        assert_eq!(state.lifecycle(), DaemonLifecycle::Faulted);
        assert_eq!(orchestrator.timers.next_deadline(), Some(deadline));
        assert!(orchestrator.active_operations.contains_key(&operation));
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 1);
        assert_eq!(state.queued_operations.load(Ordering::Acquire), 1);
        let command = control_rx
            .try_recv()
            .expect("accepted durable interruption remains in the actor lane");
        assert!(matches!(
            &command,
            JournalCommand::InterruptDeadline {
                operation: observed,
                ..
            } if *observed == operation
        ));
        execute_journal_command(&journal, command).expect("journal command executes");
        assert_eq!(
            journal.status(operation).expect("operation persists").state,
            OperationState::Interrupted
        );
    }

    #[tokio::test(start_paused = true)]
    async fn timer_storm_keeps_status_and_cancel_reachable_ahead_of_timer_two() {
        let (mut orchestrator, state, control_rx, _normal_rx) =
            manual_orchestrator(3, Duration::from_millis(10));
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let now = tokio::time::Instant::now();
        for seed in 1..=20 {
            let operation = OperationId::from_bytes([seed; 16]);
            journal.enqueue(operation).expect("operation enqueues");
            orchestrator
                .timers
                .register(
                    ScheduledTimer {
                        operation,
                        reason: TimerReason::Deadline,
                    },
                    now,
                )
                .expect("timer registers");
        }
        let control_operation = OperationId::from_bytes([54; 16]);
        journal
            .enqueue(control_operation)
            .expect("control operation enqueues");

        orchestrator
            .process_event(OrchestratorEvent {
                kind: OrchestratorEventKind::Timer,
            })
            .await
            .expect("only the first timer enters the control lane");
        let (status_reply, status_receiver) = tokio::sync::oneshot::channel();
        orchestrator
            .journal
            .try_send(
                JournalLane::Control,
                JournalCommand::Execute {
                    request: ControlRequest::OperationStatus(control_operation),
                    claim: None,
                    reply: status_reply,
                },
            )
            .expect("status remains admissible");
        let (cancel_reply, cancel_receiver) = tokio::sync::oneshot::channel();
        orchestrator
            .journal
            .try_send(
                JournalLane::Control,
                JournalCommand::Execute {
                    request: ControlRequest::OperationCancel(control_operation),
                    claim: None,
                    reply: cancel_reply,
                },
            )
            .expect("cancel remains admissible");

        for expected in ["timer", "status", "cancel"] {
            let command = control_rx.try_recv().expect("queued command is present");
            match (expected, &command) {
                ("timer", JournalCommand::InterruptDeadline { .. })
                | (
                    "status",
                    JournalCommand::Execute {
                        request: ControlRequest::OperationStatus(_),
                        ..
                    },
                )
                | (
                    "cancel",
                    JournalCommand::Execute {
                        request: ControlRequest::OperationCancel(_),
                        ..
                    },
                ) => {}
                _ => panic!("unexpected control-lane ordering"),
            }
            execute_journal_command(&journal, command).expect("journal command executes");
        }
        assert!(matches!(
            status_receiver
                .await
                .expect("status reply channel remains open")
                .expect("status succeeds"),
            ControlResponse::OperationStatus(operation)
                if operation.operation == control_operation
                    && !operation.cancellation_requested
        ));
        assert!(matches!(
            cancel_receiver
                .await
                .expect("cancel reply channel remains open")
                .expect("cancel succeeds"),
            ControlResponse::OperationCancel {
                accepted: true,
                operation,
            } if operation.operation == control_operation
        ));

        let event = orchestrator
            .next_event()
            .await
            .expect("first timer acknowledgement becomes ready");
        let interrupted = orchestrator
            .process_event(event)
            .await
            .expect("first timer acknowledgement persists")
            .expect("timer delivery returns durable state");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(orchestrator.timers.by_timer.len(), 19);
        assert!(orchestrator.pending_timer.is_none());
        assert_eq!(state.lifecycle(), DaemonLifecycle::Ready);
    }

    #[tokio::test]
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
                monotonic: now
                    .checked_sub(Duration::from_millis(100))
                    .expect("test monotonic instant can represent admission delay"),
            },
        );

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
            .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(5))
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
            .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .expect("orchestrator shuts down");
        actor.join().expect("actor joins");
    }

    #[test]
    fn default_limits_bind_per_client_admission_to_global_bounds() {
        let limits = DaemonLimits::default();

        assert_eq!(
            limits.client_connection_limit(),
            DEFAULT_CLIENT_CONNECTION_LIMIT
        );
        assert_eq!(
            limits.client_operation_limit(),
            DEFAULT_CLIENT_OPERATION_LIMIT
        );
        assert_eq!(limits.operation_workers(), DEFAULT_OPERATION_WORKERS);
        assert_eq!(limits.operation_workers(), 4);
        assert!(limits.client_connection_limit() <= limits.connection_limit());
        assert!(limits.client_operation_limit() <= limits.operation_queue_limit());
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

    #[tokio::test(flavor = "current_thread")]
    async fn synthetic_worker_join_is_bounded_and_drop_does_not_rejoin() {
        let state = Arc::new(DaemonState::starting());
        let admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let operation = OperationId::from_bytes([70; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancellation = journal.enqueue(operation).expect("operation enqueues");
        let actor = JournalActor::start(journal, 2, 2).expect("actor starts");
        let permit = SchedulerPermit::reserve(
            Arc::clone(&state),
            admissions,
            ClientInstanceId::SYSTEM,
            1,
            1,
        )
        .expect("permit reserves");
        let mut workers = SyntheticWorkerPool::start(1, 1).expect("worker pool starts");
        let (admitted_tx, admitted_rx) = mpsc::sync_channel(1);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        workers
            .submit(WorkerJob {
                operation,
                admitted: admitted_rx,
                handshake_timeout: DEFAULT_REQUEST_TIMEOUT,
                journal: actor.handle(),
                permit,
                started: Some(started_tx),
            })
            .expect("worker job submits");
        admitted_tx.send(()).expect("worker admission is durable");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker starts");

        let started = Instant::now();
        assert!(matches!(
            workers
                .join_until(tokio::time::Instant::now() + Duration::from_millis(20))
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        let drop_started = Instant::now();
        drop(workers);
        assert!(drop_started.elapsed() < Duration::from_millis(100));
        let _ = cancellation.cancel(rootlight_operations::CancellationReason::Shutdown);
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.admitted_operations.load(Ordering::Acquire) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached worker releases its permit");
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synthetic_worker_join_wakes_full_completion_channel() {
        let state = Arc::new(DaemonState::starting());
        let admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let mut workers = SyntheticWorkerPool::start(1, 1).expect("worker pool starts");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let first = OperationId::from_bytes([80; 16]);
        let second = OperationId::from_bytes([81; 16]);
        let first_cancellation = journal.enqueue(first).expect("first operation enqueues");
        let second_cancellation = journal.enqueue(second).expect("second operation enqueues");
        assert!(first_cancellation.cancel(rootlight_operations::CancellationReason::Shutdown));
        assert!(second_cancellation.cancel(rootlight_operations::CancellationReason::Shutdown));
        let (first_admitted_tx, first_admitted_rx) = mpsc::sync_channel(1);
        let (first_started_tx, first_started_rx) = mpsc::sync_channel(0);
        workers
            .submit(WorkerJob {
                operation: first,
                admitted: first_admitted_rx,
                handshake_timeout: DEFAULT_REQUEST_TIMEOUT,
                journal: actor.handle(),
                permit: SchedulerPermit::reserve(
                    Arc::clone(&state),
                    Arc::clone(&admissions),
                    ClientInstanceId::SYSTEM,
                    2,
                    2,
                )
                .expect("first permit reserves"),
                started: Some(first_started_tx),
            })
            .expect("first job submits");
        first_admitted_tx
            .send(())
            .expect("first worker admission is durable");
        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first job starts");

        let (second_admitted_tx, second_admitted_rx) = mpsc::sync_channel(1);
        let (second_started_tx, second_started_rx) = mpsc::sync_channel(0);
        workers
            .submit(WorkerJob {
                operation: second,
                admitted: second_admitted_rx,
                handshake_timeout: DEFAULT_REQUEST_TIMEOUT,
                journal: actor.handle(),
                permit: SchedulerPermit::reserve(
                    Arc::clone(&state),
                    admissions,
                    ClientInstanceId::SYSTEM,
                    2,
                    2,
                )
                .expect("second permit reserves"),
                started: Some(second_started_tx),
            })
            .expect("second job submits");
        second_admitted_tx
            .send(())
            .expect("second worker admission is durable");
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second job reaches the full completion channel");

        workers
            .join_until(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .expect("closed completion admission wakes the worker");
        while let Ok(completion) = workers.completions.try_recv() {
            completion.permit.finish();
        }
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn orchestrator_timeout_does_not_block_drop_on_running_worker() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let limits = DaemonLimits::new(
            4,
            4,
            4,
            1,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_secs(1),
        )
        .expect("limits are valid");
        let state = Arc::new(DaemonState::starting());
        state.set_lifecycle(DaemonLifecycle::Ready);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let mut orchestrator = DaemonOrchestrator::new(handle.clone(), Arc::clone(&state), limits)
            .expect("orchestrator starts");
        orchestrator
            .schedule(prepared(OperationSubmission::control_probe(
                OperationId::from_bytes([79; 16]),
            )))
            .await
            .expect("operation schedules");
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.running_operations.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker begins execution");

        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");
        assert!(matches!(
            orchestrator
                .shutdown_until(tokio::time::Instant::now() + Duration::from_millis(20))
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        let drop_started = Instant::now();
        drop(orchestrator);
        assert!(drop_started.elapsed() < Duration::from_millis(100));

        release_tx.send(()).expect("journal actor resumes");
        handle
            .control(ControlRequest::Health)
            .await
            .expect("queued shutdown interruption completes first");
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_claimed_mutations_never_run_after_actor_unblocks() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let owner = ClientInstanceId::new([71; 16]).expect("client identity is valid");
        let queued = OperationId::from_bytes([72; 16]);
        let publication = OperationId::from_bytes([73; 16]);
        let completion = OperationId::from_bytes([74; 16]);
        let failure = OperationId::from_bytes([75; 16]);
        let cancellation = OperationId::from_bytes([76; 16]);
        for (operation, seed) in [
            (queued, 72),
            (publication, 73),
            (completion, 74),
            (failure, 75),
            (cancellation, 76),
        ] {
            journal
                .submit(
                    OperationSubmission::new(
                        operation,
                        OperationKind::RepositoryIndex,
                        PlanHash::from_bytes([seed; 32]),
                        owner,
                        true,
                        None,
                        None,
                    )
                    .expect("submission is valid"),
                )
                .expect("operation submits");
        }
        for operation in [publication, completion, failure, cancellation] {
            journal
                .start_execution(operation)
                .expect("operation starts");
        }
        let actor = JournalActor::start(Arc::clone(&journal), 16, 16).expect("actor starts");
        let handle = actor.handle();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");

        let new_operation = OperationId::from_bytes([77; 16]);
        let submission = OperationSubmission::new(
            new_operation,
            OperationKind::RepositoryIndex,
            PlanHash::from_bytes([77; 32]),
            owner,
            true,
            None,
            None,
        )
        .expect("submission is valid");
        let timeout = || Instant::now() + Duration::from_millis(10);
        assert!(matches!(
            handle.submit_until(submission, timeout()).await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(matches!(
            handle.activate_operation_until(queued, timeout()).await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(matches!(
            handle
                .complete_publication_until(publication, timeout())
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(matches!(
            handle
                .finish_operation_until(completion, None, timeout())
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        let checked_error = PublicError::builder(ErrorCode::Internal, "checked failure")
            .build()
            .expect("error is valid");
        assert!(matches!(
            handle
                .fail_operation_until(failure, checked_error, timeout())
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(matches!(
            handle
                .control_until(ControlRequest::OperationCancel(cancellation), timeout(),)
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        release_tx.send(()).expect("actor resumes");
        handle
            .normal(ControlRequest::Health)
            .await
            .expect("normal-lane barrier completes");
        assert!(matches!(
            journal.status(new_operation),
            Err(OperationError::NotFound)
        ));
        assert_eq!(
            journal.status(queued).expect("queued state loads").state,
            OperationState::Queued
        );
        for operation in [publication, completion, failure, cancellation] {
            let record = journal.status(operation).expect("running state loads");
            assert_eq!(record.state, OperationState::Running);
            assert_eq!(record.stage, OperationStage::Executing);
            assert!(!record.cancellation_requested);
        }
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admission_and_cancel_claims_expire_before_finalization_grace() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let cancelled = OperationId::from_bytes([84; 16]);
        journal
            .enqueue(cancelled)
            .expect("cancel operation enqueues");
        journal
            .start_execution(cancelled)
            .expect("cancel operation starts");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");

        let admitted = OperationId::from_bytes([85; 16]);
        let client_deadline = Instant::now() + Duration::from_millis(20);
        let finalization_deadline = client_deadline + Duration::from_secs(2);
        assert!(matches!(
            handle
                .submit_until(
                    OperationSubmission::control_probe(admitted),
                    client_deadline,
                )
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(matches!(
            handle
                .control_until(ControlRequest::OperationCancel(cancelled), client_deadline,)
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(Instant::now() < finalization_deadline);

        release_tx.send(()).expect("actor resumes during grace");
        handle
            .normal(ControlRequest::Health)
            .await
            .expect("normal-lane barrier completes");
        assert!(matches!(
            journal.status(admitted),
            Err(OperationError::NotFound)
        ));
        let cancelled_record = journal.status(cancelled).expect("cancel state loads");
        assert_eq!(cancelled_record.state, OperationState::Running);
        assert!(!cancelled_record.cancellation_requested);
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_claimed_future_abandons_queued_mutation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");

        let operation = OperationId::from_bytes([78; 16]);
        let owner = ClientInstanceId::new([78; 16]).expect("client identity is valid");
        let task_handle = handle.clone();
        let mutation = tokio::spawn(async move {
            task_handle
                .submit_until(
                    OperationSubmission::new(
                        operation,
                        OperationKind::RepositoryIndex,
                        PlanHash::from_bytes([78; 32]),
                        owner,
                        true,
                        None,
                        None,
                    )
                    .expect("submission is valid"),
                    Instant::now() + Duration::from_secs(30),
                )
                .await
        });
        tokio::task::yield_now().await;
        mutation.abort();
        assert!(
            mutation
                .await
                .expect_err("mutation task is cancelled")
                .is_cancelled()
        );
        release_tx.send(()).expect("actor resumes");
        handle
            .normal(ControlRequest::Health)
            .await
            .expect("normal-lane barrier completes");
        assert!(matches!(
            journal.status(operation),
            Err(OperationError::NotFound)
        ));
        actor.join().expect("actor joins");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn journal_actor_join_is_bounded_by_shutdown_deadline() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 4, 4).expect("actor starts");
        let live_handle = actor.handle();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        live_handle
            .try_send(
                JournalLane::Control,
                JournalCommand::Block {
                    started: started_tx,
                    release: release_rx,
                },
            )
            .expect("actor blocker queues");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reaches blocker");

        let started = Instant::now();
        assert!(matches!(
            actor
                .join_until(tokio::time::Instant::now() + Duration::from_millis(20))
                .await,
            Err(ServiceError::RequestTimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            live_handle.control(ControlRequest::Health).await,
            Err(ServiceError::ChannelClosed)
        ));
        release_tx
            .send(())
            .expect("detached join coordinator resumes");
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
    async fn publication_completion_serializes_with_cancellation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("actor starts");
        let handle = actor.handle();
        let owner = ClientInstanceId::new([31; 16]).expect("client identity is valid");

        let authorized = OperationId::from_bytes([32; 16]);
        handle
            .submit(
                OperationSubmission::new(
                    authorized,
                    OperationKind::RepositoryIndex,
                    PlanHash::from_bytes([32; 32]),
                    owner,
                    true,
                    None,
                    None,
                )
                .expect("submission is valid"),
            )
            .await
            .expect("operation submits");
        handle
            .activate_operation(authorized)
            .await
            .expect("operation activates");
        let publication = handle
            .complete_publication(authorized)
            .await
            .expect("publication completion wins");
        assert_eq!(publication.stage, OperationStage::Cleanup);
        assert_eq!(publication.state, OperationState::Succeeded);
        let cancellation = handle
            .control(ControlRequest::OperationCancel(authorized))
            .await
            .expect("late cancellation responds");
        assert!(matches!(
            cancellation,
            ControlResponse::OperationCancel {
                accepted: false,
                operation: OperationRecord {
                    state: OperationState::Succeeded,
                    ..
                },
            }
        ));

        let cancelled = OperationId::from_bytes([33; 16]);
        handle
            .submit(
                OperationSubmission::new(
                    cancelled,
                    OperationKind::RepositoryIndex,
                    PlanHash::from_bytes([33; 32]),
                    owner,
                    true,
                    None,
                    None,
                )
                .expect("submission is valid"),
            )
            .await
            .expect("operation submits");
        handle
            .activate_operation(cancelled)
            .await
            .expect("operation activates");
        let cancellation = handle
            .control(ControlRequest::OperationCancel(cancelled))
            .await
            .expect("early cancellation responds");
        assert!(matches!(
            cancellation,
            ControlResponse::OperationCancel { accepted: true, .. }
        ));
        assert!(matches!(
            handle.complete_publication(cancelled).await,
            Err(ServiceError::Operations(OperationError::CancellationWon))
        ));
        let terminal = handle
            .finish_operation(
                cancelled,
                Some(rootlight_operations::CancellationReason::ClientRequest),
            )
            .await
            .expect("cancelled operation terminalizes");
        assert_eq!(terminal.state, OperationState::Cancelled);

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
            handshake_timeout: DEFAULT_REQUEST_TIMEOUT,
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
        pool.join_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .expect("worker joins");
        actor.join().expect("actor joins");
        assert_eq!(state.admitted_operations.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn queued_worker_gets_a_fresh_handshake_deadline_when_dequeued() {
        let state = Arc::new(DaemonState::starting());
        let client_admissions = Arc::new(Mutex::new(ClientOperationAdmissions::default()));
        let mut pool = SyntheticWorkerPool::start(1, 2).expect("worker pool starts");
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 2, 2).expect("actor starts");
        let first = OperationId::from_bytes([40; 16]);
        let second = OperationId::from_bytes([41; 16]);
        let handshake_timeout = Duration::from_secs(1);
        journal.enqueue(first).expect("first operation enqueues");
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
                handshake_timeout,
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

        first_started_rx
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("first worker starts");
        assert_eq!(
            journal.status(first).expect("first status loads").state,
            OperationState::Running
        );
        assert_eq!(
            journal.status(second).expect("second status loads").state,
            OperationState::Queued
        );

        // The first slice exceeds the second job's enqueue-time budget by construction.
        let first_completion = pool.completion().await.expect("first completion arrives");
        assert!(first_completion.start.is_ok());
        assert_eq!(first_completion.cancellation_reason, None);
        first_completion.permit.finish();
        second_started_rx
            .recv_timeout(DEFAULT_REQUEST_TIMEOUT)
            .expect("second worker starts with a fresh handshake deadline");
        assert_eq!(
            journal.status(second).expect("second status loads").state,
            OperationState::Running
        );

        assert!(
            second_cancellation.cancel(rootlight_operations::CancellationReason::ClientRequest)
        );
        let second_completion = pool.completion().await.expect("second completion arrives");
        second_completion.permit.finish();
        pool.join_until(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .expect("worker joins");
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
            .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(5))
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
            .shutdown_until(tokio::time::Instant::now() + Duration::from_secs(5))
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

    #[tokio::test]
    async fn shutdown_interrupts_multiple_full_journal_batches() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        for value in 1_u128..=300 {
            journal
                .enqueue(OperationId::from_bytes(value.to_le_bytes()))
                .expect("operation enqueues");
        }
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
        let mut orchestrator =
            DaemonOrchestrator::new(actor.handle(), state, limits).expect("orchestrator starts");

        orchestrator
            .shutdown()
            .await
            .expect("all operation batches are interrupted");

        assert_eq!(journal.active_count().expect("active count loads"), 0);
        for value in [1_u128, 256, 257, 300] {
            assert_eq!(
                journal
                    .status(OperationId::from_bytes(value.to_le_bytes()))
                    .expect("operation persists")
                    .state,
                OperationState::Interrupted
            );
        }
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

    #[test]
    fn first_slice_source_references_validate_identity_range_and_correlation() {
        let repository = common::RepositoryId { value: vec![1; 16] };
        let generation = common::GenerationId { value: vec![2; 20] };
        let reference = daemon::FirstSliceSourceRef {
            repository: Some(repository.clone()),
            generation: Some(generation.clone()),
            file: Some(common::FileId { value: vec![3; 20] }),
            start_byte: 4,
            end_byte: 12,
            content_hash: Some(common::ContentHash { value: vec![4; 32] }),
            start_line: Some(1),
            end_line: Some(2),
        };
        let request = |references: Vec<daemon::FirstSliceSourceRef>| {
            FirstSliceIpcRequest::SourceRead(daemon::SourceReadRequest {
                schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
                repository: Some(repository.clone()),
                generation: Some(daemon::GenerationSelector {
                    selector: Some(daemon::generation_selector::Selector::Generation(
                        generation.clone(),
                    )),
                }),
                references,
            })
        };
        assert!(validate_first_slice_request(&request(vec![reference.clone()])).is_ok());

        let mut invalid_file = reference.clone();
        invalid_file.file = Some(common::FileId { value: vec![3; 19] });
        assert!(validate_first_slice_request(&request(vec![invalid_file])).is_err());

        let mut invalid_hash = reference.clone();
        invalid_hash.content_hash = Some(common::ContentHash { value: vec![4; 31] });
        assert!(validate_first_slice_request(&request(vec![invalid_hash])).is_err());

        let mut inverted = reference.clone();
        inverted.start_byte = 13;
        assert!(validate_first_slice_request(&request(vec![inverted])).is_err());

        let mut incomplete_lines = reference.clone();
        incomplete_lines.end_line = None;
        assert!(validate_first_slice_request(&request(vec![incomplete_lines])).is_err());

        let mut foreign_repository = reference.clone();
        foreign_repository.repository = Some(common::RepositoryId { value: vec![9; 16] });
        assert!(validate_first_slice_request(&request(vec![foreign_repository])).is_err());

        let mut foreign_generation = reference.clone();
        foreign_generation.generation = Some(common::GenerationId { value: vec![8; 20] });
        assert!(validate_first_slice_request(&request(vec![foreign_generation])).is_err());
        assert!(
            validate_first_slice_request(&request(vec![reference.clone(), reference])).is_err()
        );
    }

    #[test]
    fn first_slice_capabilities_are_only_advertised_on_minor_five() {
        let service = service();
        let hello = |maximum_minor| daemon::ClientHello {
            supported_protocols: Some(common::VersionRange {
                minimum: Some(common::ContractVersion { major: 1, minor: 1 }),
                maximum: Some(common::ContractVersion {
                    major: 1,
                    minor: maximum_minor,
                }),
            }),
            capabilities: Vec::new(),
            expected_instance_nonce: vec![7; 16],
            client_instance_id: vec![8; 16],
        };
        let previous = service.negotiate(&hello(4));
        assert!(
            !previous
                .capabilities
                .iter()
                .any(|capability| capability == "repository.index.v1")
        );
        let current = service.negotiate(&hello(5));
        assert!(
            current
                .capabilities
                .iter()
                .any(|capability| capability == "repository.index.v1")
        );
    }

    #[test]
    fn first_slice_response_variant_mismatch_is_a_bounded_internal_error() {
        let request = FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest::default());
        let wire = correlated_first_slice_response(
            &request,
            FirstSliceIpcResponse::SourceRead(daemon::SourceReadResponse::default()),
        );
        let daemon::response_envelope::Response::Error(error) = wire else {
            panic!("mismatched first-slice response must fail closed");
        };
        assert_eq!(error.code, common::ErrorCode::Internal as i32);
        assert_eq!(error.message, "internal operation failed");
        assert!(error.details.is_empty());
    }

    fn correlation_context(
        repository: &common::RepositoryId,
        generation: &common::GenerationId,
        results: u64,
        source_bytes: u64,
    ) -> daemon::FirstSliceQueryContext {
        daemon::FirstSliceQueryContext {
            repository: Some(repository.clone()),
            generation: Some(generation.clone()),
            parent_generation: Some(common::GenerationId { value: vec![7; 20] }),
            active_generation: true,
            tier: daemon::FirstSliceAnalysisTier::FirstSliceTierC as i32,
            coverage_status: daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete as i32,
            skipped_inputs: 0,
            usage: Some(daemon::FirstSliceQueryUsage {
                rows: results,
                edges: 0,
                results,
                source_bytes,
                json_bytes: 0,
                estimated_tokens: 0,
                elapsed_micros: 1,
            }),
        }
    }

    fn correlation_source(
        repository: &common::RepositoryId,
        generation: &common::GenerationId,
        file_byte: u8,
        start_byte: u64,
        end_byte: u64,
    ) -> daemon::FirstSliceSourceRef {
        daemon::FirstSliceSourceRef {
            repository: Some(repository.clone()),
            generation: Some(generation.clone()),
            file: Some(common::FileId {
                value: vec![file_byte; 20],
            }),
            start_byte,
            end_byte,
            content_hash: Some(common::ContentHash {
                value: vec![file_byte; 32],
            }),
            start_line: Some(1),
            end_line: Some(1),
        }
    }

    #[test]
    fn first_slice_response_correlation_rejects_identity_substitution() {
        let schema = Some(common::ContractVersion { major: 1, minor: 0 });
        let repository = common::RepositoryId { value: vec![1; 16] };
        let generation = common::GenerationId { value: vec![2; 20] };
        let operation = common::OperationId { value: vec![3; 16] };

        let index_request = FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
            schema_version: schema,
            root: "C:\\bounded".to_owned(),
            operation: Some(operation.clone()),
            detached: true,
        });
        let index_response = daemon::RepositoryIndexResponse {
            schema_version: schema,
            repository: Some(repository.clone()),
            operation: Some(operation.clone()),
            state: daemon::OperationState::Succeeded as i32,
            revision: 1,
            parent_generation: None,
            published_generation: Some(generation.clone()),
            discovered_inputs: 2,
            indexed_files: 2,
            entities: 2,
            elapsed_micros: 1,
        };
        assert!(first_slice_response_correlates(
            &index_request,
            &FirstSliceIpcResponse::RepositoryIndex(index_response.clone())
        ));
        let mut self_parent = index_response.clone();
        self_parent.parent_generation = self_parent.published_generation.clone();
        assert!(!first_slice_response_correlates(
            &index_request,
            &FirstSliceIpcResponse::RepositoryIndex(self_parent)
        ));
        let mut foreign_index = index_response;
        foreign_index.operation = Some(common::OperationId { value: vec![9; 16] });
        assert!(!first_slice_response_correlates(
            &index_request,
            &FirstSliceIpcResponse::RepositoryIndex(foreign_index)
        ));

        let status_request = FirstSliceIpcRequest::RepositoryOperationStatus(
            daemon::RepositoryOperationStatusRequest {
                schema_version: schema,
                operation: Some(operation.clone()),
                ..Default::default()
            },
        );
        let mut status_response = daemon::RepositoryOperationStatusResponse {
            schema_version: schema,
            operation: Some(daemon::OperationStatus {
                operation: Some(operation.clone()),
                state: daemon::OperationState::Queued as i32,
                revision: 1,
                completed_units: 0,
                total_units: 1,
                error: None,
                kind: daemon::OperationKind::RepositoryIndex as i32,
                stage: daemon::OperationStage::Accepted as i32,
                plan_hash: vec![4; 32],
                detached: true,
                cancellation_requested: false,
                deadline_unix_ms: None,
                lease_expires_unix_ms: None,
                recovery_class: daemon::RecoveryClass::NotApplicable as i32,
            }),
            published_generation: None,
            started_unix_ms: 1,
            peak_rss_bytes: 0,
            written_bytes: 0,
            files_examined: 0,
            retry_after_ms: None,
        };
        assert!(first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(status_response.clone())
        ));
        status_response.retry_after_ms = Some(0);
        assert!(first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(status_response.clone())
        ));
        let wire_failure = |message: &str| common::PublicError {
            code: common::ErrorCode::Internal as i32,
            message: message.to_owned(),
            retryable: false,
            retry_after_ms: None,
            repository: None,
            operation: None,
            generation: None,
            details: Default::default(),
            next_actions: Vec::new(),
        };
        let mut failed_without_error = status_response.clone();
        let failed_without_error_operation = failed_without_error
            .operation
            .as_mut()
            .expect("operation exists");
        failed_without_error_operation.state = daemon::OperationState::Failed as i32;
        failed_without_error.retry_after_ms = None;
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(failed_without_error)
        ));
        let mut queued_with_error = status_response.clone();
        queued_with_error
            .operation
            .as_mut()
            .expect("operation exists")
            .error = Some(wire_failure("checked failure"));
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(queued_with_error)
        ));
        let mut checked_failure = status_response.clone();
        let checked_failure_operation = checked_failure
            .operation
            .as_mut()
            .expect("operation exists");
        checked_failure_operation.state = daemon::OperationState::Failed as i32;
        checked_failure_operation.error = Some(wire_failure("checked failure"));
        checked_failure.retry_after_ms = None;
        assert!(first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(checked_failure.clone())
        ));
        let mut foreign_nested_error = checked_failure.clone();
        foreign_nested_error
            .operation
            .as_mut()
            .expect("operation exists")
            .error
            .as_mut()
            .expect("error exists")
            .operation = Some(common::OperationId { value: vec![9; 16] });
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(foreign_nested_error)
        ));
        let mut contradictory_retry = checked_failure;
        let contradictory_retry_error = contradictory_retry
            .operation
            .as_mut()
            .expect("operation exists")
            .error
            .as_mut()
            .expect("error exists");
        contradictory_retry_error.retry_after_ms = Some(1);
        contradictory_retry_error.retryable = false;
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(contradictory_retry)
        ));
        let mut oversized_message = status_response.clone();
        let oversized_message_operation = oversized_message
            .operation
            .as_mut()
            .expect("operation exists");
        oversized_message_operation.state = daemon::OperationState::Failed as i32;
        oversized_message_operation.error = Some(wire_failure(
            &"x".repeat(MAX_WIRE_PUBLIC_ERROR_MESSAGE_BYTES + 1),
        ));
        oversized_message.retry_after_ms = None;
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(oversized_message)
        ));
        let mut oversized_details = status_response.clone();
        let oversized_details_operation = oversized_details
            .operation
            .as_mut()
            .expect("operation exists");
        oversized_details_operation.state = daemon::OperationState::Failed as i32;
        let mut too_many_details = wire_failure("checked failure");
        for index in 0..=MAX_WIRE_PUBLIC_ERROR_DETAILS {
            too_many_details.details.insert(
                format!("detail_{index}"),
                common::PublicValue {
                    value: Some(common::public_value::Value::Boolean(true)),
                },
            );
        }
        oversized_details_operation.error = Some(too_many_details);
        oversized_details.retry_after_ms = None;
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(oversized_details)
        ));
        let mut unsafe_failure = status_response;
        let unsafe_failure_operation = unsafe_failure.operation.as_mut().expect("operation exists");
        unsafe_failure_operation.state = daemon::OperationState::Failed as i32;
        unsafe_failure_operation.error = Some(wire_failure(r"C:\secret\src\lib.rs"));
        unsafe_failure.retry_after_ms = None;
        assert!(!first_slice_response_correlates(
            &status_request,
            &FirstSliceIpcResponse::RepositoryOperationStatus(unsafe_failure)
        ));

        let first_source = correlation_source(&repository, &generation, 4, 1, 2);
        let second_source = correlation_source(&repository, &generation, 5, 4, 5);
        let locate_request = FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest {
            schema_version: schema,
            repository: Some(repository.clone()),
            generation: Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Active(true)),
            }),
            query: "answer".to_owned(),
            mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
            maximum_results: 2,
        });
        let locate_hit =
            |symbol_byte: u8, source: &daemon::FirstSliceSourceRef| daemon::FirstSliceLocateHit {
                symbol: Some(common::SymbolId {
                    value: vec![symbol_byte; 20],
                }),
                file: source.file.clone(),
                identifier: "answer".to_owned(),
                qualified_name: "answer".to_owned(),
                path: "src/lib.rs".to_owned(),
                kind: "function".to_owned(),
                language: "rust".to_owned(),
                tier: daemon::FirstSliceAnalysisTier::FirstSliceTierC as i32,
                generated: false,
                score: 1_000,
                source: Some(source.clone()),
            };
        let locate_response = daemon::CodeLocateResponse {
            schema_version: schema,
            context: Some(correlation_context(&repository, &generation, 3, 0)),
            hits: vec![locate_hit(6, &first_source), locate_hit(7, &second_source)],
            matched_candidates: 2,
            truncated: false,
        };
        assert!(first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(locate_response.clone())
        ));
        let mut incomplete_without_truncation = locate_response.clone();
        incomplete_without_truncation.matched_candidates = 3;
        assert!(!first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(incomplete_without_truncation.clone())
        ));
        incomplete_without_truncation.truncated = true;
        assert!(first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(incomplete_without_truncation)
        ));
        let mut wrong_result_usage = locate_response.clone();
        wrong_result_usage
            .context
            .as_mut()
            .expect("context exists")
            .usage
            .as_mut()
            .expect("usage exists")
            .results = 1;
        assert!(!first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(wrong_result_usage)
        ));
        let pinned_locate_request = FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest {
            schema_version: schema,
            repository: Some(repository.clone()),
            generation: Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Generation(
                    generation.clone(),
                )),
            }),
            query: "answer".to_owned(),
            mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
            maximum_results: 2,
        });
        assert!(first_slice_response_correlates(
            &pinned_locate_request,
            &FirstSliceIpcResponse::CodeLocate(locate_response.clone())
        ));
        let mut foreign_context = locate_response.clone();
        foreign_context
            .context
            .as_mut()
            .expect("context exists")
            .repository = Some(common::RepositoryId { value: vec![9; 16] });
        assert!(!first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(foreign_context)
        ));
        let mut foreign_source = locate_response;
        foreign_source.hits[0]
            .source
            .as_mut()
            .expect("source exists")
            .generation = Some(common::GenerationId { value: vec![9; 20] });
        assert!(!first_slice_response_correlates(
            &locate_request,
            &FirstSliceIpcResponse::CodeLocate(foreign_source)
        ));

        let symbols = [
            common::SymbolId { value: vec![6; 20] },
            common::SymbolId { value: vec![7; 20] },
        ];
        let explain_request = FirstSliceIpcRequest::SymbolExplain(daemon::SymbolExplainRequest {
            schema_version: schema,
            repository: Some(repository.clone()),
            generation: Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Active(true)),
            }),
            symbols: symbols.to_vec(),
        });
        let explain_response = daemon::SymbolExplainResponse {
            schema_version: schema,
            context: Some(correlation_context(&repository, &generation, 2, 0)),
            symbols: vec![daemon::FirstSliceSymbolExplanation {
                symbol: Some(symbols[0].clone()),
                kind: "function".to_owned(),
                display_name: "answer".to_owned(),
                signature: None,
                definition: Some(first_source.clone()),
                outbound_exact: 0,
                outbound_candidates: 0,
                inbound_exact: 0,
                inbound_candidates: 0,
                references_exact: 0,
                provider: "tree-sitter".to_owned(),
                evidence: "parser".to_owned(),
                confidence: 1_000,
            }],
            unresolved_symbols: vec![symbols[1].clone()],
            truncated: false,
        };
        assert!(first_slice_response_correlates(
            &explain_request,
            &FirstSliceIpcResponse::SymbolExplain(explain_response.clone())
        ));
        let mut duplicate = explain_response;
        duplicate.unresolved_symbols[0] = symbols[0].clone();
        assert!(!first_slice_response_correlates(
            &explain_request,
            &FirstSliceIpcResponse::SymbolExplain(duplicate)
        ));

        let source_request = FirstSliceIpcRequest::SourceRead(daemon::SourceReadRequest {
            schema_version: schema,
            repository: Some(repository.clone()),
            generation: Some(daemon::GenerationSelector {
                selector: Some(daemon::generation_selector::Selector::Active(true)),
            }),
            references: vec![first_source.clone(), second_source.clone()],
        });
        let source_chunk =
            |source: &daemon::FirstSliceSourceRef,
             start_byte: u64,
             end_byte: u64,
             content: &str| daemon::FirstSliceSourceChunk {
                source: Some(source.clone()),
                path: "src/lib.rs".to_owned(),
                start_byte,
                end_byte,
                start_line: 1,
                end_line: 1,
                content: content.to_owned(),
                content_hash: source.content_hash.clone(),
                language: "rust".to_owned(),
                generated: false,
            };
        let source_response = daemon::SourceReadResponse {
            schema_version: schema,
            context: Some(correlation_context(&repository, &generation, 2, 6)),
            chunks: vec![
                source_chunk(&first_source, 0, 3, "aaa"),
                source_chunk(&second_source, 3, 6, "bbb"),
            ],
            total_source_bytes: 6,
            truncated: false,
        };
        assert!(first_slice_response_correlates(
            &source_request,
            &FirstSliceIpcResponse::SourceRead(source_response.clone())
        ));
        let mut wrong_usage = source_response.clone();
        wrong_usage
            .context
            .as_mut()
            .expect("context exists")
            .usage
            .as_mut()
            .expect("usage exists")
            .source_bytes = 5;
        assert!(!first_slice_response_correlates(
            &source_request,
            &FirstSliceIpcResponse::SourceRead(wrong_usage)
        ));
        let mut wrong_results = source_response.clone();
        wrong_results
            .context
            .as_mut()
            .expect("context exists")
            .usage
            .as_mut()
            .expect("usage exists")
            .results = 1;
        assert!(!first_slice_response_correlates(
            &source_request,
            &FirstSliceIpcResponse::SourceRead(wrong_results)
        ));
        let mut reordered = source_response;
        reordered.chunks.swap(0, 1);
        assert!(!first_slice_response_correlates(
            &source_request,
            &FirstSliceIpcResponse::SourceRead(reordered)
        ));

        let rejected = correlated_first_slice_response(
            &source_request,
            FirstSliceIpcResponse::SourceRead(daemon::SourceReadResponse::default()),
        );
        let daemon::response_envelope::Response::Error(error) = rejected else {
            panic!("malformed handler response must fail closed");
        };
        assert_eq!(error.code, common::ErrorCode::Internal as i32);
        assert_eq!(error.message, "internal operation failed");
        assert!(error.details.is_empty());
    }
}
