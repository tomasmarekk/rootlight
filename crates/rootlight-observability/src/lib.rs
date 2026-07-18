//! Source-free operational evidence and deterministic support archives.
//!
//! This crate accepts only allow-listed aggregate data. It owns the privacy and
//! size boundary for support bundles so transport and CLI layers cannot add
//! repository content, identifiers, paths, or arbitrary diagnostic text.

#![forbid(unsafe_code)]

use std::{
    io::{Cursor, Write as _},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// Frozen support-bundle schema used by protocol 1.3 clients.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = 1;
/// Frozen support-bundle schema used by protocol 1.4 clients.
pub const PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = 2;
/// Current support-bundle schema including every protocol 1.5 control method.
pub const CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = 3;
/// Schema version for normalized telemetry snapshots.
pub const TELEMETRY_SCHEMA_VERSION: u32 = 1;
/// Maximum encoded support archive returned through daemon IPC.
pub const MAX_SUPPORT_ARCHIVE_BYTES: usize = 768 * 1024;
/// Maximum JSON payload accepted for one support entry.
pub const MAX_SUPPORT_ENTRY_BYTES: usize = 128 * 1024;
/// Stricter maximum for one normalized telemetry entry.
pub const MAX_TELEMETRY_ENTRY_BYTES: usize = 64 * 1024;
/// Maximum recent structured log records retained in memory.
pub const RECENT_LOG_CAPACITY: usize = 64;
/// Maximum recent completed spans retained in memory.
pub const RECENT_TRACE_CAPACITY: usize = 64;
/// Maximum encoded bytes for one structured JSON log line, including its newline.
pub const MAX_STRUCTURED_LOG_LINE_BYTES: usize = 512;
/// Fixed upper bounds for local request-duration histogram buckets.
pub const DURATION_BUCKET_UPPER_US: [u64; 10] = [
    100, 500, 1_000, 5_000, 10_000, 25_000, 50_000, 100_000, 1_000_000, 5_000_000,
];
/// Requests at or above this duration are retained as structured log events.
pub const SLOW_CONTROL_REQUEST_US: u64 = 50_000;

const SUPPORT_ENTRY_COUNT_V1: usize = 5;
const SUPPORT_ENTRY_COUNT_V2: usize = 6;
const SUPPORT_ENTRY_COUNT_V3: usize = 6;
const CONTROL_METHOD_COUNT_V2: usize = 8;
const CONTROL_METHOD_COUNT: usize = 13;
const TELEMETRY_OUTCOME_COUNT: usize = 6;
/// Ordered allow-list for the frozen support archive schema.
pub const SUPPORT_ENTRY_NAMES: [&str; SUPPORT_ENTRY_COUNT_V1] = [
    "diagnostics/quick.json",
    "health.json",
    "manifest.json",
    "operations-summary.json",
    "redaction-report.json",
];
/// Ordered allow-list for support archives with normalized telemetry.
pub const SUPPORT_ENTRY_NAMES_V2: [&str; SUPPORT_ENTRY_COUNT_V2] = [
    "diagnostics/quick.json",
    "health.json",
    "manifest.json",
    "operations-summary.json",
    "redaction-report.json",
    "telemetry.json",
];
/// Ordered allow-list for current support archives with normalized telemetry.
pub const SUPPORT_ENTRY_NAMES_V3: [&str; SUPPORT_ENTRY_COUNT_V3] = SUPPORT_ENTRY_NAMES_V2;
/// Data classes that the frozen support schema must explicitly omit.
pub const OMITTED_DATA_CLASSES: [&str; 12] = [
    "absolute_roots",
    "adapter_output",
    "compiler_output",
    "credentials",
    "environment",
    "identifiers",
    "paths",
    "prompts",
    "raw_logs",
    "raw_sqlite_errors",
    "source",
    "traces",
];
/// Data classes omitted by support archives containing normalized telemetry.
pub const OMITTED_DATA_CLASSES_V2: [&str; 12] = [
    "absolute_roots",
    "adapter_output",
    "compiler_output",
    "credentials",
    "environment",
    "free_form_text",
    "identifiers",
    "paths",
    "prompts",
    "raw_logs",
    "raw_sqlite_errors",
    "source",
];
/// Data classes omitted by current support archives.
pub const OMITTED_DATA_CLASSES_V3: [&str; 12] = OMITTED_DATA_CLASSES_V2;

/// Closed daemon protocol version emitted by this support schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolVersion {
    /// Rootlight daemon protocol 1.3.
    #[serde(rename = "1.3")]
    V1_3,
    /// Rootlight daemon protocol 1.4.
    #[serde(rename = "1.4")]
    V1_4,
    /// Rootlight daemon protocol 1.5.
    #[serde(rename = "1.5")]
    V1_5,
}

/// Closed target operating-system family emitted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingSystem {
    /// Linux target family.
    Linux,
    /// macOS target family.
    Macos,
    /// Windows target family.
    Windows,
    /// Another target family not yet classified by this schema.
    Other,
}

/// Closed target architecture emitted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Architecture {
    /// 64-bit Arm target.
    #[serde(rename = "aarch64")]
    Aarch64,
    /// 32-bit Arm target.
    #[serde(rename = "arm")]
    Arm,
    /// 32-bit x86 target.
    #[serde(rename = "x86")]
    X86,
    /// 64-bit x86 target.
    #[serde(rename = "x86_64")]
    X86_64,
    /// Another target architecture not yet classified by this schema.
    #[serde(rename = "other")]
    Other,
}

/// Closed source-free daemon lifecycle used in operational evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Closed stable public error code accepted by support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// The caller supplied an invalid value.
    InvalidArgument,
    /// The requested entity does not exist.
    NotFound,
    /// The request conflicts with current state.
    Conflict,
    /// The selected generation is stale.
    StaleGeneration,
    /// The requested capability is unavailable.
    UnsupportedCapability,
    /// The result lacks requested coverage.
    IncompleteCoverage,
    /// The request exceeded a work budget.
    BudgetExceeded,
    /// A bounded resource is exhausted.
    ResourceExhausted,
    /// The operation was cancelled.
    Cancelled,
    /// An isolated adapter failed.
    AdapterFailed,
    /// Stored index data is corrupt.
    IndexCorrupt,
    /// Stored data requires migration.
    MigrationRequired,
    /// Policy denied the request.
    PermissionDenied,
    /// Protocol negotiation failed.
    ProtocolMismatch,
    /// A resource is temporarily busy.
    Busy,
    /// A failure cannot be safely disclosed.
    Internal,
}

/// Closed source-free subsystem status used in operational evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Closed host resource-pressure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Source-free daemon health snapshot accepted by support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSnapshot {
    /// Whether the daemon is ready for its current contract.
    pub ready: bool,
    /// Closed daemon lifecycle state.
    pub lifecycle: DaemonLifecycle,
    /// Whether operation admission is open.
    pub accepting_operations: bool,
    /// Number of accepted connections currently in flight.
    pub active_connections: u32,
    /// Configured global connection limit.
    pub connection_limit: u32,
    /// Number of admitted operations.
    pub admitted_operations: u32,
    /// Number of operations awaiting workers.
    pub queued_operations: u32,
    /// Number of operations currently executing.
    pub running_operations: u32,
    /// Configured global operation admission limit.
    pub operation_queue_limit: u32,
    /// Cached catalog status.
    pub catalog_status: HealthStatus,
    /// Current catalog schema version.
    pub catalog_schema_version: u32,
    /// Current generation subsystem status.
    pub generation_status: HealthStatus,
    /// Current adapter subsystem status.
    pub adapter_status: HealthStatus,
    /// Current watcher subsystem status.
    pub watcher_status: HealthStatus,
    /// Current endpoint ownership status.
    pub endpoint_status: HealthStatus,
    /// Current endpoint/discovery schema version.
    pub endpoint_schema_version: u32,
    /// Current bounded host-pressure classification.
    pub resource_pressure: ResourcePressure,
}

/// Closed outcome for the catalog quick check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticOutcome {
    /// The checked catalog passed validation.
    Passed,
    /// The checked catalog failed validation.
    Failed,
    /// The bounded check exceeded its deadline.
    TimedOut,
    /// The check could not be admitted or executed.
    Unavailable,
}

/// Source-free quick-diagnostic snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsQuickSnapshot {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Aggregate status after the check.
    pub overall_status: HealthStatus,
    /// Catalog quick-check outcome.
    pub catalog_quick_check: DiagnosticOutcome,
    /// Monotonic elapsed time rounded to milliseconds.
    pub duration_ms: u32,
    /// Stable public error code, when the check did not pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

/// Aggregate operation counts safe for support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationsSummary {
    /// Operations durably queued.
    pub queued: u32,
    /// Operations durably running.
    pub running: u32,
    /// Operations completing cancellation cleanup.
    pub cancelling: u32,
}

/// Closed output behavior for the in-process telemetry recorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryOutput {
    /// Retain records for local snapshots without writing process output.
    RetainedOnly,
    /// Retain records and emit selected structured JSON events to stderr.
    StderrJson,
}

/// Closed severity for normalized structured log events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSeverity {
    /// Normal lifecycle or completion evidence.
    Info,
    /// A request was rejected, timed out, or degraded.
    Warn,
    /// A required daemon action failed.
    Error,
}

/// Closed subsystem target for normalized telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryTarget {
    /// Daemon process lifecycle.
    Daemon,
    /// Authenticated local control transport.
    Ipc,
    /// Durable operation orchestration.
    Operation,
    /// Health, diagnostics, and support evidence.
    Diagnostics,
}

/// Closed local control method dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMethod {
    /// The request variant was missing or malformed.
    Unknown,
    /// Lock-free daemon health.
    Health,
    /// Bounded catalog quick diagnostics.
    DiagnosticsQuick,
    /// Deterministic support evidence.
    SupportBundle,
    /// Durable operation admission.
    OperationSubmit,
    /// Durable operation status.
    OperationStatus,
    /// Cooperative operation cancellation.
    OperationCancel,
    /// Attached operation lease renewal.
    OperationLeaseRenew,
    /// Whole-root first-slice repository indexing.
    RepositoryIndex,
    /// Repository index lifecycle status or cancellation.
    RepositoryOperationStatus,
    /// Generation-pinned lexical lookup.
    CodeLocate,
    /// Generation-pinned symbol explanation.
    SymbolExplain,
    /// Verified immutable source read.
    SourceRead,
}

impl ControlMethod {
    /// Returns every metric dimension in canonical serialized order.
    pub const ALL: [Self; CONTROL_METHOD_COUNT] = [
        Self::Unknown,
        Self::Health,
        Self::DiagnosticsQuick,
        Self::SupportBundle,
        Self::OperationSubmit,
        Self::OperationStatus,
        Self::OperationCancel,
        Self::OperationLeaseRenew,
        Self::RepositoryIndex,
        Self::RepositoryOperationStatus,
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SourceRead,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Unknown => 0,
            Self::Health => 1,
            Self::DiagnosticsQuick => 2,
            Self::SupportBundle => 3,
            Self::OperationSubmit => 4,
            Self::OperationStatus => 5,
            Self::OperationCancel => 6,
            Self::OperationLeaseRenew => 7,
            Self::RepositoryIndex => 8,
            Self::RepositoryOperationStatus => 9,
            Self::CodeLocate => 10,
            Self::SymbolExplain => 11,
            Self::SourceRead => 12,
        }
    }
}

/// Closed outcome shared by local log, metric, and span records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryOutcome {
    /// The bounded action succeeded.
    Succeeded,
    /// Policy, validation, or capacity rejected the action.
    Rejected,
    /// The action exceeded its monotonic deadline.
    TimedOut,
    /// The action ended through cooperative cancellation.
    Cancelled,
    /// The action completed with a stable failure.
    Failed,
    /// A started span was dropped without explicit completion.
    Abandoned,
}

impl TelemetryOutcome {
    const fn index(self) -> usize {
        match self {
            Self::Succeeded => 0,
            Self::Rejected => 1,
            Self::TimedOut => 2,
            Self::Cancelled => 3,
            Self::Failed => 4,
            Self::Abandoned => 5,
        }
    }
}

/// Closed source-free structured event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEvent {
    /// The daemon entered one lifecycle phase.
    LifecycleChanged {
        /// Closed lifecycle phase.
        lifecycle: DaemonLifecycle,
    },
    /// One local control request completed.
    RequestCompleted {
        /// Closed control method.
        method: ControlMethod,
        /// Closed completion outcome.
        outcome: TelemetryOutcome,
        /// Monotonic elapsed microseconds.
        duration_us: u64,
        /// Stable public failure code, when applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// One diagnostic or support operation completed.
    DiagnosticCompleted {
        /// Closed diagnostic method.
        method: ControlMethod,
        /// Closed completion outcome.
        outcome: TelemetryOutcome,
        /// Monotonic elapsed microseconds.
        duration_us: u64,
        /// Stable public failure code, when applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// One accepted connection was rejected by the global process bound.
    ConnectionRejected {
        /// Stable source-free failure code.
        error_code: ErrorCode,
    },
    /// One accepted connection task failed outside a request response.
    ConnectionTaskFailed {
        /// Stable source-free failure code.
        error_code: ErrorCode,
    },
    /// The daemon process failed before or outside a request response.
    DaemonFailed {
        /// Stable source-free failure code.
        error_code: ErrorCode,
    },
}

/// One normalized bounded structured log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredLogRecord {
    /// Telemetry schema version.
    pub schema_version: u32,
    /// Process-local monotonic record sequence.
    pub sequence: u64,
    /// Best-effort wall-clock timestamp for local diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_unix_ms: Option<u64>,
    /// Process uptime at emission.
    pub uptime_us: u64,
    /// Closed event severity.
    pub severity: LogSeverity,
    /// Closed subsystem target.
    pub target: TelemetryTarget,
    /// Closed event payload without arbitrary text.
    pub event: LogEvent,
}

/// Closed completed-span kind retained for local diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// Daemon process startup.
    DaemonStartup,
    /// Daemon graceful shutdown.
    DaemonShutdown,
    /// Authenticated local negotiation.
    IpcNegotiation,
    /// One local control request.
    IpcRequest {
        /// Closed control method.
        method: ControlMethod,
    },
    /// One bounded diagnostic quick check.
    DiagnosticsQuick,
    /// One deterministic support archive construction.
    SupportBundle,
}

/// One normalized completed local span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletedSpan {
    /// Telemetry schema version.
    pub schema_version: u32,
    /// Process-local monotonic record sequence.
    pub sequence: u64,
    /// Span start relative to recorder creation.
    pub started_uptime_us: u64,
    /// Monotonic elapsed span time.
    pub duration_us: u64,
    /// Closed action kind.
    pub kind: SpanKind,
    /// Closed completion outcome.
    pub outcome: TelemetryOutcome,
    /// Stable public failure code without arbitrary error text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

/// Snapshot of one fixed request-duration histogram.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistogramSnapshot {
    /// Fixed bucket upper bounds in microseconds.
    pub upper_bounds_us: [u64; 10],
    /// Counts for ten bounded buckets and one overflow bucket.
    pub bucket_counts: [u64; 11],
    /// Total observations.
    pub count: u64,
    /// Saturating sum of observed microseconds.
    pub sum_us: u64,
}

/// Fixed-cardinality metrics for one local control method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcMethodMetrics {
    /// Closed method dimension.
    pub method: ControlMethod,
    /// Successful request count.
    pub succeeded_total: u64,
    /// Rejected request count.
    pub rejected_total: u64,
    /// Timed-out request count.
    pub timed_out_total: u64,
    /// Cancelled request count.
    pub cancelled_total: u64,
    /// Failed request count.
    pub failed_total: u64,
    /// Abandoned request count.
    pub abandoned_total: u64,
    /// Monotonic duration distribution.
    pub duration_us: HistogramSnapshot,
}

/// Fixed-cardinality process-local telemetry metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSnapshot {
    /// Telemetry schema version.
    pub schema_version: u32,
    /// Exact canonical method metric rows.
    pub ipc_requests: Vec<IpcMethodMetrics>,
    /// Structured records displaced from the bounded log ring.
    pub logs_overwritten_total: u64,
    /// Completed spans displaced from the bounded trace ring.
    pub traces_overwritten_total: u64,
    /// Structured stderr records that could not be emitted.
    pub log_write_failures_total: u64,
    /// Whether process-local sequence allocation exhausted `u64`.
    pub sequence_exhausted: bool,
}

/// Bounded normalized telemetry snapshot accepted by support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySnapshot {
    /// Telemetry schema version.
    pub schema_version: u32,
    /// Configured recent-log capacity.
    pub log_capacity: u32,
    /// Configured recent-span capacity.
    pub trace_capacity: u32,
    /// Recent structured records in oldest-to-newest order.
    pub logs: Vec<StructuredLogRecord>,
    /// Fixed-cardinality process lifetime metrics.
    pub metrics: MetricsSnapshot,
    /// Recent completed spans in oldest-to-newest order.
    pub traces: Vec<CompletedSpan>,
}

/// Support archive schema selected by negotiated protocol semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportBundleSchema {
    /// Frozen five-entry schema without telemetry.
    V1,
    /// Six-entry schema with normalized bounded telemetry.
    V2,
    /// Six-entry schema covering every protocol 1.5 control method.
    V3,
}

/// Inputs accepted by the support-bundle privacy boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportBundleInput {
    /// Current private daemon protocol version.
    pub protocol_version: ProtocolVersion,
    /// Sanitized target operating-system family.
    pub operating_system: OperatingSystem,
    /// Sanitized target architecture.
    pub architecture: Architecture,
    /// Source-free health snapshot.
    pub health: HealthSnapshot,
    /// Latest bounded quick-diagnostic snapshot.
    pub diagnostics: DiagnosticsQuickSnapshot,
    /// Aggregate durable operation counts.
    pub operations: OperationsSummary,
    /// Pre-assembly normalized telemetry for schema v2.
    pub telemetry: Option<TelemetrySnapshot>,
}

/// Validated encoded support bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportBundle {
    archive: Vec<u8>,
    sha256: [u8; 32],
}

impl SupportBundle {
    /// Returns the deterministic ZIP archive bytes.
    #[must_use]
    pub fn archive(&self) -> &[u8] {
        &self.archive
    }

    /// Returns the SHA-256 digest of the complete ZIP archive.
    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// Returns the encoded archive length.
    #[must_use]
    pub fn archive_bytes(&self) -> u64 {
        u64::try_from(self.archive.len())
            .unwrap_or_else(|_| unreachable!("bounded support archive length fits u64"))
    }

    /// Reports whether this archive contains repository source.
    #[must_use]
    pub const fn contains_source(&self) -> bool {
        false
    }
}

/// Parsed support manifest used to validate transported archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportManifest {
    /// Support schema version.
    pub schema_version: u32,
    /// Daemon protocol version that emitted the archive.
    pub protocol_version: ProtocolVersion,
    /// Sanitized target operating-system family.
    pub operating_system: OperatingSystem,
    /// Sanitized target architecture.
    pub architecture: Architecture,
    /// Must remain false for this support schema.
    pub contains_source: bool,
    /// Hash and size records for every non-manifest entry.
    pub entries: Vec<SupportManifestEntry>,
}

/// One manifest record for an allow-listed support entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportManifestEntry {
    /// Allow-listed archive entry name.
    pub name: String,
    /// Uncompressed JSON byte length.
    pub bytes: u64,
    /// Lowercase SHA-256 digest of the JSON bytes.
    pub sha256: String,
}

/// Parsed redaction declaration used to validate transported archives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionReport {
    /// Support schema version.
    pub schema_version: u32,
    /// Must remain false for this support schema.
    pub contains_source: bool,
    /// Exact set of sensitive data classes excluded by the builder.
    pub omitted_data_classes: Vec<String>,
}

#[derive(Debug)]
struct FixedRing<T, const N: usize> {
    entries: [Option<T>; N],
    next: usize,
    len: usize,
    overwritten: u64,
}

impl<T: Copy, const N: usize> FixedRing<T, N> {
    fn new() -> Self {
        Self {
            entries: [None; N],
            next: 0,
            len: 0,
            overwritten: 0,
        }
    }

    fn push(&mut self, value: T) {
        if N == 0 {
            self.overwritten = self.overwritten.saturating_add(1);
            return;
        }
        if self.len == N {
            self.overwritten = self.overwritten.saturating_add(1);
        } else {
            self.len += 1;
        }
        self.entries[self.next] = Some(value);
        self.next = (self.next + 1) % N;
    }

    fn snapshot(&self) -> Vec<T> {
        if self.len == 0 {
            return Vec::new();
        }
        let oldest = if self.len == N { self.next } else { 0 };
        (0..self.len)
            .filter_map(|offset| self.entries[(oldest + offset) % N])
            .collect()
    }
}

#[derive(Debug)]
struct AtomicHistogram {
    buckets: [AtomicU64; 11],
    count: AtomicU64,
    sum_us: AtomicU64,
}

impl AtomicHistogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
        }
    }

    fn record(&self, duration_us: u64) {
        let bucket = DURATION_BUCKET_UPPER_US
            .iter()
            .position(|upper| duration_us <= *upper)
            .unwrap_or(DURATION_BUCKET_UPPER_US.len());
        saturating_increment(&self.buckets[bucket]);
        saturating_increment(&self.count);
        saturating_add(&self.sum_us, duration_us);
    }

    fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            upper_bounds_us: DURATION_BUCKET_UPPER_US,
            bucket_counts: std::array::from_fn(|index| self.buckets[index].load(Ordering::Relaxed)),
            count: self.count.load(Ordering::Relaxed),
            sum_us: self.sum_us.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
struct MethodMetrics {
    outcomes: [AtomicU64; TELEMETRY_OUTCOME_COUNT],
    duration: AtomicHistogram,
}

impl MethodMetrics {
    fn new() -> Self {
        Self {
            outcomes: std::array::from_fn(|_| AtomicU64::new(0)),
            duration: AtomicHistogram::new(),
        }
    }

    fn snapshot(&self, method: ControlMethod) -> IpcMethodMetrics {
        IpcMethodMetrics {
            method,
            succeeded_total: self.outcomes[TelemetryOutcome::Succeeded.index()]
                .load(Ordering::Relaxed),
            rejected_total: self.outcomes[TelemetryOutcome::Rejected.index()]
                .load(Ordering::Relaxed),
            timed_out_total: self.outcomes[TelemetryOutcome::TimedOut.index()]
                .load(Ordering::Relaxed),
            cancelled_total: self.outcomes[TelemetryOutcome::Cancelled.index()]
                .load(Ordering::Relaxed),
            failed_total: self.outcomes[TelemetryOutcome::Failed.index()].load(Ordering::Relaxed),
            abandoned_total: self.outcomes[TelemetryOutcome::Abandoned.index()]
                .load(Ordering::Relaxed),
            duration_us: self.duration.snapshot(),
        }
    }
}

/// Fixed-cardinality in-process telemetry recorder.
#[derive(Debug)]
pub struct Telemetry {
    started: Instant,
    output: TelemetryOutput,
    sequence: AtomicU64,
    sequence_exhausted: AtomicBool,
    logs: Mutex<FixedRing<StructuredLogRecord, RECENT_LOG_CAPACITY>>,
    traces: Mutex<FixedRing<CompletedSpan, RECENT_TRACE_CAPACITY>>,
    methods: [MethodMetrics; CONTROL_METHOD_COUNT],
    log_write_failures: AtomicU64,
}

impl Telemetry {
    /// Creates one recorder with fixed memory and cardinality bounds.
    #[must_use]
    pub fn new(output: TelemetryOutput) -> Self {
        Self {
            started: Instant::now(),
            output,
            sequence: AtomicU64::new(0),
            sequence_exhausted: AtomicBool::new(false),
            logs: Mutex::new(FixedRing::new()),
            traces: Mutex::new(FixedRing::new()),
            methods: std::array::from_fn(|_| MethodMetrics::new()),
            log_write_failures: AtomicU64::new(0),
        }
    }

    /// Starts one local span guard without holding a lock across its lifetime.
    #[must_use]
    pub fn start_span(self: &Arc<Self>, kind: SpanKind) -> SpanGuard {
        SpanGuard {
            telemetry: Arc::clone(self),
            kind,
            started: Instant::now(),
            completed: false,
        }
    }

    /// Records one request outcome and monotonic duration.
    pub fn record_request(
        &self,
        method: ControlMethod,
        outcome: TelemetryOutcome,
        duration: Duration,
        error_code: Option<ErrorCode>,
    ) {
        let duration_us = duration_us(duration);
        let metrics = &self.methods[method.index()];
        saturating_increment(&metrics.outcomes[outcome.index()]);
        metrics.duration.record(duration_us);
        if outcome != TelemetryOutcome::Succeeded || duration_us >= SLOW_CONTROL_REQUEST_US {
            self.record_log(LogEvent::RequestCompleted {
                method,
                outcome,
                duration_us,
                error_code,
            });
        }
    }

    /// Records one daemon lifecycle transition.
    pub fn record_lifecycle(&self, lifecycle: DaemonLifecycle) {
        self.record_log(LogEvent::LifecycleChanged { lifecycle });
    }

    /// Records one diagnostic completion event.
    pub fn record_diagnostic(
        &self,
        method: ControlMethod,
        outcome: TelemetryOutcome,
        duration: Duration,
        error_code: Option<ErrorCode>,
    ) {
        self.record_log(LogEvent::DiagnosticCompleted {
            method,
            outcome,
            duration_us: duration_us(duration),
            error_code,
        });
    }

    /// Records global connection load shedding without retaining peer data.
    pub fn record_connection_rejected(&self) {
        self.record_log(LogEvent::ConnectionRejected {
            error_code: ErrorCode::ResourceExhausted,
        });
    }

    /// Records a connection task failure without retaining error text.
    pub fn record_connection_task_failed(&self) {
        self.record_log(LogEvent::ConnectionTaskFailed {
            error_code: ErrorCode::Internal,
        });
    }

    /// Records a daemon process failure without retaining error text.
    pub fn record_daemon_failed(&self) {
        self.record_log(LogEvent::DaemonFailed {
            error_code: ErrorCode::Internal,
        });
    }

    /// Returns a bounded source-free process-local snapshot.
    #[must_use]
    pub fn snapshot(&self) -> TelemetrySnapshot {
        let logs = self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let log_records = logs.snapshot();
        let logs_overwritten_total = logs.overwritten;
        drop(logs);
        let traces = self
            .traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let trace_records = traces.snapshot();
        let traces_overwritten_total = traces.overwritten;
        drop(traces);
        TelemetrySnapshot {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            log_capacity: u32::try_from(RECENT_LOG_CAPACITY)
                .unwrap_or_else(|_| unreachable!("reviewed log capacity fits u32")),
            trace_capacity: u32::try_from(RECENT_TRACE_CAPACITY)
                .unwrap_or_else(|_| unreachable!("reviewed trace capacity fits u32")),
            logs: log_records,
            metrics: MetricsSnapshot {
                schema_version: TELEMETRY_SCHEMA_VERSION,
                ipc_requests: ControlMethod::ALL
                    .into_iter()
                    .map(|method| self.methods[method.index()].snapshot(method))
                    .collect(),
                logs_overwritten_total,
                traces_overwritten_total,
                log_write_failures_total: self.log_write_failures.load(Ordering::Relaxed),
                sequence_exhausted: self.sequence_exhausted.load(Ordering::Acquire),
            },
            traces: trace_records,
        }
    }

    fn record_log(&self, event: LogEvent) {
        let Some(sequence) = self.next_sequence() else {
            return;
        };
        let (severity, target) = classify_log_event(event);
        let record = StructuredLogRecord {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            sequence,
            timestamp_unix_ms: unix_timestamp_ms(),
            uptime_us: duration_us(self.started.elapsed()),
            severity,
            target,
            event,
        };
        self.logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(record);
        if self.output == TelemetryOutput::StderrJson && !write_log_record(record) {
            saturating_increment(&self.log_write_failures);
        }
    }

    fn finish_span(
        &self,
        kind: SpanKind,
        started: Instant,
        outcome: TelemetryOutcome,
        error_code: Option<ErrorCode>,
    ) {
        let Some(sequence) = self.next_sequence() else {
            return;
        };
        let elapsed = started.elapsed();
        let elapsed_us = duration_us(elapsed);
        let started_uptime_us = duration_us(self.started.elapsed().saturating_sub(elapsed));
        self.traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(CompletedSpan {
                schema_version: TELEMETRY_SCHEMA_VERSION,
                sequence,
                started_uptime_us,
                duration_us: elapsed_us,
                kind,
                outcome,
                error_code,
            });
    }

    fn next_sequence(&self) -> Option<u64> {
        let allocated =
            self.sequence
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(1)
                });
        match allocated {
            Ok(previous) => Some(previous + 1),
            Err(_) => {
                self.sequence_exhausted.store(true, Ordering::Release);
                None
            }
        }
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new(TelemetryOutput::RetainedOnly)
    }
}

/// Guard that records exactly one completed local span.
#[derive(Debug)]
pub struct SpanGuard {
    telemetry: Arc<Telemetry>,
    kind: SpanKind,
    started: Instant,
    completed: bool,
}

impl SpanGuard {
    /// Finishes the span with one closed outcome and optional stable error code.
    pub fn finish(mut self, outcome: TelemetryOutcome, error_code: Option<ErrorCode>) {
        self.completed = true;
        self.telemetry
            .finish_span(self.kind, self.started, outcome, error_code);
    }
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.telemetry
                .finish_span(self.kind, self.started, TelemetryOutcome::Abandoned, None);
        }
    }
}

fn classify_log_event(event: LogEvent) -> (LogSeverity, TelemetryTarget) {
    match event {
        LogEvent::LifecycleChanged {
            lifecycle: DaemonLifecycle::Faulted,
        } => (LogSeverity::Error, TelemetryTarget::Daemon),
        LogEvent::LifecycleChanged { .. } => (LogSeverity::Info, TelemetryTarget::Daemon),
        LogEvent::RequestCompleted {
            outcome: TelemetryOutcome::Failed,
            ..
        }
        | LogEvent::ConnectionTaskFailed { .. } => (LogSeverity::Error, TelemetryTarget::Ipc),
        LogEvent::DaemonFailed { .. } => (LogSeverity::Error, TelemetryTarget::Daemon),
        LogEvent::ConnectionRejected { .. } | LogEvent::RequestCompleted { .. } => {
            (LogSeverity::Warn, TelemetryTarget::Ipc)
        }
        LogEvent::DiagnosticCompleted {
            outcome: TelemetryOutcome::Succeeded,
            ..
        } => (LogSeverity::Info, TelemetryTarget::Diagnostics),
        LogEvent::DiagnosticCompleted {
            outcome: TelemetryOutcome::Failed,
            ..
        } => (LogSeverity::Error, TelemetryTarget::Diagnostics),
        LogEvent::DiagnosticCompleted { .. } => (LogSeverity::Warn, TelemetryTarget::Diagnostics),
    }
}

fn write_log_record(record: StructuredLogRecord) -> bool {
    let mut bytes = match serde_json::to_vec(&record) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    bytes.push(b'\n');
    if bytes.len() > MAX_STRUCTURED_LOG_LINE_BYTES {
        return false;
    }
    let stderr = std::io::stderr();
    let mut locked = stderr.lock();
    locked.write_all(&bytes).is_ok()
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn unix_timestamp_ms() -> Option<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(millis).ok()
}

fn saturating_increment(value: &AtomicU64) {
    saturating_add(value, 1);
}

fn saturating_add(value: &AtomicU64, addition: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(addition))
    });
}

struct SupportEntry {
    name: &'static str,
    bytes: Vec<u8>,
}

/// Builds one deterministic bounded source-free support archive.
///
/// This compatibility entry point preserves the frozen schema-v1 archive contract.
///
/// # Errors
///
/// Returns [`SupportBundleError`] when serialization or ZIP encoding fails or
/// an entry/archive exceeds its reviewed limit.
pub fn build_support_bundle(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    build_support_bundle_for_schema(input, SupportBundleSchema::V1)
}

/// Builds one deterministic support archive for an explicitly selected schema.
///
/// # Errors
///
/// Returns [`SupportBundleError`] when the selected schema lacks required input,
/// serialization or ZIP encoding fails, or an entry/archive exceeds its reviewed limit.
pub fn build_support_bundle_for_schema(
    input: &SupportBundleInput,
    schema: SupportBundleSchema,
) -> Result<SupportBundle, SupportBundleError> {
    let expected_protocol = match schema {
        SupportBundleSchema::V1 => ProtocolVersion::V1_3,
        SupportBundleSchema::V2 => ProtocolVersion::V1_4,
        SupportBundleSchema::V3 => ProtocolVersion::V1_5,
    };
    if input.protocol_version != expected_protocol {
        return Err(SupportBundleError::ProtocolVersionMismatch);
    }
    match schema {
        SupportBundleSchema::V1 => build_support_bundle_v1(input),
        SupportBundleSchema::V2 => build_support_bundle_v2(input),
        SupportBundleSchema::V3 => build_support_bundle_v3(input),
    }
}

fn build_support_bundle_v1(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    let diagnostics = json_entry("diagnostics/quick.json", &input.diagnostics)?;
    let health = json_entry("health.json", &input.health)?;
    let operations = json_entry("operations-summary.json", &input.operations)?;
    let redaction = redaction_entry(SUPPORT_BUNDLE_SCHEMA_VERSION, &OMITTED_DATA_CLASSES)?;
    let manifest = support_manifest_entry(
        SUPPORT_BUNDLE_SCHEMA_VERSION,
        input,
        [&diagnostics, &health, &operations, &redaction],
    )?;
    finish_support_bundle(&[diagnostics, health, manifest, operations, redaction])
}

fn build_support_bundle_v2(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    let telemetry = input
        .telemetry
        .as_ref()
        .ok_or(SupportBundleError::MissingTelemetry)?;
    let telemetry = project_telemetry_v2(telemetry);
    let diagnostics = json_entry("diagnostics/quick.json", &input.diagnostics)?;
    let health = json_entry("health.json", &input.health)?;
    let operations = json_entry("operations-summary.json", &input.operations)?;
    let redaction = redaction_entry(
        PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION,
        &OMITTED_DATA_CLASSES_V2,
    )?;
    let telemetry = json_entry_with_limit("telemetry.json", &telemetry, MAX_TELEMETRY_ENTRY_BYTES)?;
    let manifest = support_manifest_entry(
        PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION,
        input,
        [&diagnostics, &health, &operations, &redaction, &telemetry],
    )?;
    finish_support_bundle(&[
        diagnostics,
        health,
        manifest,
        operations,
        redaction,
        telemetry,
    ])
}

fn build_support_bundle_v3(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    let telemetry = input
        .telemetry
        .as_ref()
        .ok_or(SupportBundleError::MissingTelemetry)?;
    let diagnostics = json_entry("diagnostics/quick.json", &input.diagnostics)?;
    let health = json_entry("health.json", &input.health)?;
    let operations = json_entry("operations-summary.json", &input.operations)?;
    let redaction = redaction_entry(
        CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION,
        &OMITTED_DATA_CLASSES_V3,
    )?;
    let telemetry = json_entry_with_limit("telemetry.json", telemetry, MAX_TELEMETRY_ENTRY_BYTES)?;
    let manifest = support_manifest_entry(
        CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION,
        input,
        [&diagnostics, &health, &operations, &redaction, &telemetry],
    )?;
    finish_support_bundle(&[
        diagnostics,
        health,
        manifest,
        operations,
        redaction,
        telemetry,
    ])
}

fn project_telemetry_v2(telemetry: &TelemetrySnapshot) -> TelemetrySnapshot {
    let mut projected = telemetry.clone();
    projected
        .logs
        .retain(|record| log_event_supported_by_v2(record.event));
    projected
        .metrics
        .ipc_requests
        .truncate(CONTROL_METHOD_COUNT_V2);
    projected
        .traces
        .retain(|span| span_kind_supported_by_v2(span.kind));
    projected
}

const fn control_method_supported_by_v2(method: ControlMethod) -> bool {
    method.index() < CONTROL_METHOD_COUNT_V2
}

const fn log_event_supported_by_v2(event: LogEvent) -> bool {
    match event {
        LogEvent::RequestCompleted { method, .. }
        | LogEvent::DiagnosticCompleted { method, .. } => control_method_supported_by_v2(method),
        LogEvent::LifecycleChanged { .. }
        | LogEvent::ConnectionRejected { .. }
        | LogEvent::ConnectionTaskFailed { .. }
        | LogEvent::DaemonFailed { .. } => true,
    }
}

const fn span_kind_supported_by_v2(kind: SpanKind) -> bool {
    match kind {
        SpanKind::IpcRequest { method } => control_method_supported_by_v2(method),
        SpanKind::DaemonStartup
        | SpanKind::DaemonShutdown
        | SpanKind::IpcNegotiation
        | SpanKind::DiagnosticsQuick
        | SpanKind::SupportBundle => true,
    }
}

fn redaction_entry(
    schema_version: u32,
    omitted: &[&str],
) -> Result<SupportEntry, SupportBundleError> {
    json_entry(
        "redaction-report.json",
        &RedactionReport {
            schema_version,
            contains_source: false,
            omitted_data_classes: omitted.iter().map(|value| (*value).to_owned()).collect(),
        },
    )
}

fn support_manifest_entry<const N: usize>(
    schema_version: u32,
    input: &SupportBundleInput,
    entries: [&SupportEntry; N],
) -> Result<SupportEntry, SupportBundleError> {
    json_entry(
        "manifest.json",
        &SupportManifest {
            schema_version,
            protocol_version: input.protocol_version,
            operating_system: input.operating_system,
            architecture: input.architecture,
            contains_source: false,
            entries: entries
                .into_iter()
                .map(manifest_entry)
                .collect::<Result<_, _>>()?,
        },
    )
}

fn finish_support_bundle(entries: &[SupportEntry]) -> Result<SupportBundle, SupportBundleError> {
    let archive = encode_zip(entries)?;
    if archive.len() > MAX_SUPPORT_ARCHIVE_BYTES {
        return Err(SupportBundleError::ArchiveTooLarge);
    }
    let sha256: [u8; 32] = Sha256::digest(&archive).into();
    Ok(SupportBundle { archive, sha256 })
}

fn json_entry(
    name: &'static str,
    value: &impl Serialize,
) -> Result<SupportEntry, SupportBundleError> {
    json_entry_with_limit(name, value, MAX_SUPPORT_ENTRY_BYTES)
}

fn json_entry_with_limit(
    name: &'static str,
    value: &impl Serialize,
    maximum: usize,
) -> Result<SupportEntry, SupportBundleError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(SupportBundleError::SerializeJson)?;
    bytes.push(b'\n');
    if bytes.len() > maximum {
        return Err(SupportBundleError::EntryTooLarge { name });
    }
    Ok(SupportEntry { name, bytes })
}

fn manifest_entry(entry: &SupportEntry) -> Result<SupportManifestEntry, SupportBundleError> {
    Ok(SupportManifestEntry {
        name: entry.name.to_owned(),
        bytes: u64::try_from(entry.bytes.len())
            .map_err(|_| SupportBundleError::EntryTooLarge { name: entry.name })?,
        sha256: hex_digest(&entry.bytes),
    })
}

fn encode_zip(entries: &[SupportEntry]) -> Result<Vec<u8>, SupportBundleError> {
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    for entry in entries {
        writer
            .start_file(entry.name, options)
            .map_err(SupportBundleError::Zip)?;
        writer
            .write_all(&entry.bytes)
            .map_err(SupportBundleError::WriteZip)?;
    }
    writer
        .finish()
        .map(Cursor::into_inner)
        .map_err(SupportBundleError::Zip)
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .unwrap_or_else(|_| unreachable!("formatting into String cannot fail"));
    }
    encoded
}

/// Support-bundle construction failure.
#[derive(Debug, thiserror::Error)]
pub enum SupportBundleError {
    /// One allow-listed entry exceeded its bounded JSON size.
    #[error("support bundle entry exceeds its size limit")]
    EntryTooLarge {
        /// Stable allow-listed entry name.
        name: &'static str,
    },
    /// The complete encoded archive exceeded its transport-safe limit.
    #[error("support bundle archive exceeds its size limit")]
    ArchiveTooLarge,
    /// Schema v2 was selected without a normalized telemetry snapshot.
    #[error("support bundle telemetry is required for this schema")]
    MissingTelemetry,
    /// The selected support schema and daemon protocol version did not match.
    #[error("support bundle protocol version does not match its schema")]
    ProtocolVersionMismatch,
    /// Allow-listed JSON failed serialization.
    #[error("support bundle JSON serialization failed")]
    SerializeJson(#[source] serde_json::Error),
    /// ZIP metadata or entry creation failed.
    #[error("support bundle ZIP encoding failed")]
    Zip(#[source] zip::result::ZipError),
    /// Writing an allow-listed entry to the in-memory ZIP failed.
    #[error("support bundle ZIP write failed")]
    WriteZip(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn input() -> SupportBundleInput {
        SupportBundleInput {
            protocol_version: ProtocolVersion::V1_3,
            operating_system: OperatingSystem::Windows,
            architecture: Architecture::X86_64,
            health: HealthSnapshot {
                ready: true,
                lifecycle: DaemonLifecycle::Ready,
                accepting_operations: true,
                active_connections: 1,
                connection_limit: 128,
                admitted_operations: 2,
                queued_operations: 1,
                running_operations: 1,
                operation_queue_limit: 256,
                catalog_status: HealthStatus::Healthy,
                catalog_schema_version: 2,
                generation_status: HealthStatus::NotConfigured,
                adapter_status: HealthStatus::NotConfigured,
                watcher_status: HealthStatus::NotConfigured,
                endpoint_status: HealthStatus::Healthy,
                endpoint_schema_version: 2,
                resource_pressure: ResourcePressure::Unknown,
            },
            diagnostics: DiagnosticsQuickSnapshot {
                schema_version: 1,
                overall_status: HealthStatus::Healthy,
                catalog_quick_check: DiagnosticOutcome::Passed,
                duration_ms: 4,
                error_code: None,
            },
            operations: OperationsSummary {
                queued: 1,
                running: 1,
                cancelling: 0,
            },
            telemetry: None,
        }
    }

    #[test]
    fn support_archive_is_deterministic_and_allow_listed() {
        let first = build_support_bundle(&input()).expect("support bundle builds");
        let second = build_support_bundle(&input()).expect("support bundle rebuilds");
        assert_eq!(first, second);
        assert!(!first.contains_source());
        assert!(first.archive().len() <= MAX_SUPPORT_ARCHIVE_BYTES);
        assert_eq!(
            <[u8; 32]>::from(Sha256::digest(first.archive())),
            first.sha256()
        );

        let cursor = Cursor::new(first.archive());
        let mut archive = zip::ZipArchive::new(cursor).expect("support ZIP opens");
        assert_eq!(archive.len(), SUPPORT_ENTRY_COUNT_V1);
        let names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("entry opens")
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "diagnostics/quick.json",
                "health.json",
                "manifest.json",
                "operations-summary.json",
                "redaction-report.json",
            ]
        );

        let mut manifest = String::new();
        archive
            .by_name("manifest.json")
            .expect("manifest opens")
            .read_to_string(&mut manifest)
            .expect("manifest reads");
        assert!(manifest.contains("\"contains_source\": false"));
        assert!(manifest.contains("diagnostics/quick.json"));
    }

    #[test]
    fn support_archive_never_accepts_arbitrary_sensitive_payloads() {
        let bundle = build_support_bundle(&input()).expect("support bundle builds");
        let forbidden = [
            b"PRIVATE_SOURCE_BODY".as_slice(),
            b"sk-secret-token".as_slice(),
            b"C:\\Users\\private\\repo".as_slice(),
            b"/home/private/repo".as_slice(),
            b"raw sqlite failure".as_slice(),
            b"prompt injection".as_slice(),
        ];
        for value in forbidden {
            assert!(
                !bundle
                    .archive()
                    .windows(value.len())
                    .any(|window| window == value)
            );
        }
    }

    #[test]
    fn telemetry_ring_retains_the_newest_bounded_records() {
        let telemetry = Telemetry::default();
        for index in 0..=RECENT_LOG_CAPACITY {
            telemetry.record_lifecycle(if index % 2 == 0 {
                DaemonLifecycle::Starting
            } else {
                DaemonLifecycle::Ready
            });
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.logs.len(), RECENT_LOG_CAPACITY);
        assert_eq!(snapshot.metrics.logs_overwritten_total, 1);
        assert!(
            snapshot
                .logs
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(snapshot.logs.first().map(|record| record.sequence), Some(2));
    }

    #[test]
    fn telemetry_histogram_uses_fixed_boundaries() {
        let telemetry = Telemetry::default();
        telemetry.record_request(
            ControlMethod::Health,
            TelemetryOutcome::Succeeded,
            Duration::from_micros(100),
            None,
        );
        telemetry.record_request(
            ControlMethod::Health,
            TelemetryOutcome::TimedOut,
            Duration::from_micros(5_000_001),
            Some(ErrorCode::Busy),
        );

        let snapshot = telemetry.snapshot();
        let health = snapshot
            .metrics
            .ipc_requests
            .iter()
            .find(|metric| metric.method == ControlMethod::Health)
            .expect("health metric exists");
        assert_eq!(health.succeeded_total, 1);
        assert_eq!(health.timed_out_total, 1);
        assert_eq!(health.duration_us.bucket_counts[0], 1);
        assert_eq!(health.duration_us.bucket_counts[10], 1);
        assert_eq!(health.duration_us.count, 2);
        assert_eq!(health.duration_us.sum_us, 5_000_101);
    }

    #[test]
    fn span_guard_records_explicit_and_abandoned_completion_once() {
        let telemetry = Arc::new(Telemetry::default());
        telemetry
            .start_span(SpanKind::IpcRequest {
                method: ControlMethod::Health,
            })
            .finish(TelemetryOutcome::Succeeded, None);
        drop(telemetry.start_span(SpanKind::IpcNegotiation));

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.traces.len(), 2);
        assert_eq!(snapshot.traces[0].outcome, TelemetryOutcome::Succeeded);
        assert_eq!(snapshot.traces[1].outcome, TelemetryOutcome::Abandoned);
    }

    #[test]
    fn schema_v2_support_archive_contains_bounded_normalized_telemetry() {
        let telemetry = Arc::new(Telemetry::default());
        telemetry.record_lifecycle(DaemonLifecycle::Ready);
        telemetry.record_request(
            ControlMethod::CodeLocate,
            TelemetryOutcome::Rejected,
            Duration::from_millis(1),
            Some(ErrorCode::InvalidArgument),
        );
        telemetry
            .start_span(SpanKind::IpcRequest {
                method: ControlMethod::CodeLocate,
            })
            .finish(TelemetryOutcome::Rejected, Some(ErrorCode::InvalidArgument));
        let mut input = input();
        input.protocol_version = ProtocolVersion::V1_4;
        input.telemetry = Some(telemetry.snapshot());

        let first = build_support_bundle_for_schema(&input, SupportBundleSchema::V2)
            .expect("schema v2 support bundle builds");
        let second = build_support_bundle_for_schema(&input, SupportBundleSchema::V2)
            .expect("schema v2 support bundle rebuilds");
        assert_eq!(first, second);

        let cursor = Cursor::new(first.archive());
        let mut archive = zip::ZipArchive::new(cursor).expect("support ZIP opens");
        assert_eq!(archive.len(), SUPPORT_ENTRY_COUNT_V2);
        let names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("entry opens")
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, SUPPORT_ENTRY_NAMES_V2);
        let mut telemetry_entry = archive.by_name("telemetry.json").expect("telemetry opens");
        assert!(telemetry_entry.size() <= u64::try_from(MAX_TELEMETRY_ENTRY_BYTES).unwrap());
        let mut bytes = Vec::new();
        telemetry_entry
            .read_to_end(&mut bytes)
            .expect("telemetry reads");
        let telemetry: TelemetrySnapshot =
            serde_json::from_slice(&bytes).expect("telemetry decodes");
        assert_eq!(
            telemetry.metrics.ipc_requests.len(),
            CONTROL_METHOD_COUNT_V2
        );
        assert!(
            telemetry
                .logs
                .iter()
                .all(|record| log_event_supported_by_v2(record.event))
        );
        assert!(
            telemetry
                .traces
                .iter()
                .all(|span| span_kind_supported_by_v2(span.kind))
        );
    }

    #[test]
    fn schema_v3_support_archive_retains_current_control_methods() {
        let telemetry = Arc::new(Telemetry::default());
        telemetry.record_request(
            ControlMethod::CodeLocate,
            TelemetryOutcome::Rejected,
            Duration::from_millis(1),
            Some(ErrorCode::InvalidArgument),
        );
        telemetry
            .start_span(SpanKind::IpcRequest {
                method: ControlMethod::CodeLocate,
            })
            .finish(TelemetryOutcome::Rejected, Some(ErrorCode::InvalidArgument));
        let mut input = input();
        input.protocol_version = ProtocolVersion::V1_5;
        input.telemetry = Some(telemetry.snapshot());

        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V3)
            .expect("schema v3 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut telemetry_entry = archive.by_name("telemetry.json").expect("telemetry opens");
        let mut bytes = Vec::new();
        telemetry_entry
            .read_to_end(&mut bytes)
            .expect("telemetry reads");
        let telemetry: TelemetrySnapshot =
            serde_json::from_slice(&bytes).expect("telemetry decodes");
        assert_eq!(telemetry.metrics.ipc_requests.len(), CONTROL_METHOD_COUNT);
        assert!(telemetry.logs.iter().any(|record| {
            matches!(
                record.event,
                LogEvent::RequestCompleted {
                    method: ControlMethod::CodeLocate,
                    ..
                }
            )
        }));
        assert!(telemetry.traces.iter().any(|span| {
            span.kind
                == SpanKind::IpcRequest {
                    method: ControlMethod::CodeLocate,
                }
        }));
    }

    #[test]
    fn support_schema_rejects_a_mismatched_protocol_version() {
        let mut input = input();
        input.telemetry = Some(Telemetry::default().snapshot());

        assert!(matches!(
            build_support_bundle_for_schema(&input, SupportBundleSchema::V2),
            Err(SupportBundleError::ProtocolVersionMismatch)
        ));
    }

    #[test]
    fn structured_log_records_are_bounded_and_source_free() {
        let telemetry = Telemetry::default();
        telemetry.record_request(
            ControlMethod::OperationSubmit,
            TelemetryOutcome::Rejected,
            Duration::from_secs(5),
            Some(ErrorCode::ResourceExhausted),
        );
        let snapshot = telemetry.snapshot();
        let record = snapshot.logs.first().expect("request log retained");
        let mut bytes = serde_json::to_vec(record).expect("record serializes");
        bytes.push(b'\n');
        assert!(bytes.len() <= MAX_STRUCTURED_LOG_LINE_BYTES);
        for forbidden in [
            "PRIVATE_SOURCE_BODY",
            "C:\\Users\\private\\repo",
            "/home/private/repo",
            "sk-secret-token",
            "client-capability-value",
        ] {
            assert!(!String::from_utf8_lossy(&bytes).contains(forbidden));
        }
    }

    #[test]
    fn connection_rejection_record_is_closed_and_source_free() {
        let telemetry = Telemetry::default();
        telemetry.record_connection_rejected();

        let snapshot = telemetry.snapshot();
        let record = snapshot
            .logs
            .first()
            .expect("connection rejection retained");
        assert_eq!(record.severity, LogSeverity::Warn);
        assert_eq!(record.target, TelemetryTarget::Ipc);
        assert_eq!(
            record.event,
            LogEvent::ConnectionRejected {
                error_code: ErrorCode::ResourceExhausted,
            }
        );
    }

    #[test]
    fn daemon_failure_record_is_closed_and_source_free() {
        let telemetry = Telemetry::default();
        telemetry.record_daemon_failed();

        let snapshot = telemetry.snapshot();
        let record = snapshot.logs.first().expect("daemon failure retained");
        assert_eq!(record.severity, LogSeverity::Error);
        assert_eq!(record.target, TelemetryTarget::Daemon);
        assert_eq!(
            record.event,
            LogEvent::DaemonFailed {
                error_code: ErrorCode::Internal,
            }
        );
        let bytes = serde_json::to_vec(record).expect("record serializes");
        assert!(bytes.len() < MAX_STRUCTURED_LOG_LINE_BYTES);
    }
}
