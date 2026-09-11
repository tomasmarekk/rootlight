//! Source-free operational evidence and deterministic support archives.
//!
//! This crate accepts only allow-listed aggregate data. It owns the privacy and
//! size boundary for support bundles so transport and CLI layers cannot add
//! repository content, paths, or arbitrary diagnostic text.

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
/// Frozen support-bundle schema including every protocol 1.5 control method.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V3: u32 = 3;
/// Frozen production support-bundle schema used by protocol 1.8 through 1.11 clients.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V4: u32 = 4;
/// Frozen support-bundle schema with storage-accounting inventory.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V5: u32 = 5;
/// Frozen support-bundle schema with installed language capabilities.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V6: u32 = 6;
/// Frozen support-bundle schema with reconciliable durable storage accounting.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V7: u32 = 7;
/// Frozen support-bundle schema with authoritative repository-capacity accounting.
pub const SUPPORT_BUNDLE_SCHEMA_VERSION_V8: u32 = 8;
/// Current support-bundle schema.
pub const CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION: u32 = SUPPORT_BUNDLE_SCHEMA_VERSION_V8;
/// Schema version for normalized telemetry snapshots.
pub const TELEMETRY_SCHEMA_VERSION: u32 = 1;
/// Maximum encoded support archive returned through daemon IPC.
pub const MAX_SUPPORT_ARCHIVE_BYTES: usize = 768 * 1024;
/// Maximum JSON payload accepted for one support entry.
pub const MAX_SUPPORT_ENTRY_BYTES: usize = 128 * 1024;
/// Maximum JSON payload accepted for one normalized telemetry entry.
pub const MAX_TELEMETRY_ENTRY_BYTES: usize = MAX_SUPPORT_ENTRY_BYTES;
/// Maximum recent structured log records retained in memory.
pub const RECENT_LOG_CAPACITY: usize = 64;
/// Maximum recent completed spans retained in memory.
pub const RECENT_TRACE_CAPACITY: usize = 64;
/// Maximum recent terminal operations included in one support archive.
pub const MAX_SUPPORT_TERMINAL_OPERATIONS: usize = 32;
/// Maximum dependency records included in one support archive.
pub const MAX_SUPPORT_DEPENDENCIES: usize = 32;
/// Maximum adapter records included in one support archive.
pub const MAX_SUPPORT_ADAPTERS: usize = 32;
/// Maximum repository records included in one support archive.
pub const MAX_SUPPORT_REPOSITORIES: usize = 128;
/// Maximum generation records included in one support archive.
pub const MAX_SUPPORT_GENERATIONS: usize = 256;
/// Maximum installed language capabilities included in one support archive.
pub const MAX_SUPPORT_LANGUAGE_CAPABILITIES: usize = 64;
/// Maximum language or tier labels retained in one inventory record.
pub const MAX_SUPPORT_RECORD_LABELS: usize = 32;
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
const SUPPORT_ENTRY_COUNT_V4: usize = 7;
const CONTROL_METHOD_COUNT_V2: usize = 8;
const CONTROL_METHOD_COUNT: usize = 25;
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
/// Ordered allow-list for current production support archives.
pub const SUPPORT_ENTRY_NAMES_V4: [&str; SUPPORT_ENTRY_COUNT_V4] = [
    "diagnostics/quick.json",
    "health.json",
    "inventory.json",
    "manifest.json",
    "operations-summary.json",
    "redaction-report.json",
    "telemetry.json",
];
/// Ordered allow-list for current production support archives.
pub const SUPPORT_ENTRY_NAMES_V5: [&str; SUPPORT_ENTRY_COUNT_V4] = SUPPORT_ENTRY_NAMES_V4;
/// Ordered allow-list for current production support archives.
pub const SUPPORT_ENTRY_NAMES_V6: [&str; SUPPORT_ENTRY_COUNT_V4] = SUPPORT_ENTRY_NAMES_V4;
/// Ordered allow-list for support archives with authoritative storage accounting.
pub const SUPPORT_ENTRY_NAMES_V7: [&str; SUPPORT_ENTRY_COUNT_V4] = SUPPORT_ENTRY_NAMES_V4;
/// Ordered allow-list for current production support archives.
pub const SUPPORT_ENTRY_NAMES_V8: [&str; SUPPORT_ENTRY_COUNT_V4] = SUPPORT_ENTRY_NAMES_V4;
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
/// Data classes omitted by production support archives with opaque identifiers.
pub const OMITTED_DATA_CLASSES_V4: [&str; 12] = [
    "absolute_roots",
    "adapter_output",
    "compiler_output",
    "credentials",
    "environment",
    "free_form_text",
    "git_remote_credentials",
    "paths",
    "prompts",
    "raw_sqlite_errors",
    "source",
    "symbol_names",
];
/// Data classes omitted by current storage-accounting support archives.
pub const OMITTED_DATA_CLASSES_V5: [&str; 12] = OMITTED_DATA_CLASSES_V4;
/// Data classes omitted by current language-capability support archives.
pub const OMITTED_DATA_CLASSES_V6: [&str; 12] = OMITTED_DATA_CLASSES_V5;
/// Data classes omitted by authoritative storage-accounting support archives.
pub const OMITTED_DATA_CLASSES_V7: [&str; 12] = OMITTED_DATA_CLASSES_V6;
/// Data classes omitted by current repository-capacity support archives.
pub const OMITTED_DATA_CLASSES_V8: [&str; 12] = OMITTED_DATA_CLASSES_V7;

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
    /// Rootlight daemon protocol 1.8.
    #[serde(rename = "1.8")]
    V1_8,
    /// Rootlight daemon protocol 1.12.
    #[serde(rename = "1.12")]
    V1_12,
    /// Rootlight daemon protocol 1.13.
    #[serde(rename = "1.13")]
    V1_13,
    /// Rootlight daemon protocol 1.14.
    #[serde(rename = "1.14")]
    V1_14,
    /// Rootlight daemon protocol 1.15.
    #[serde(rename = "1.15")]
    V1_15,
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
    /// A pagination cursor is invalid, expired, forged, or context-mismatched.
    InvalidCursor,
    /// A supplied value has the wrong type for its target field.
    TypeMismatch,
    /// The request exceeded a cost limit before execution.
    CostLimit,
    /// The query uses an operator outside the documented allowlist.
    OperatorForbidden,
    /// A batch binding reference is malformed or unresolved.
    BindingInvalid,
    /// A batch binding produced a value of the wrong type for its target.
    BindingTypeMismatch,
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

/// Closed terminal operation kind retained in production support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportOperationKind {
    /// Internal control-path lifecycle probe.
    ControlProbe,
    /// Repository indexing and generation publication.
    RepositoryIndex,
    /// Durable generation recovery after process restart.
    Recovery,
}

/// Closed terminal operation state retained in production support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportOperationState {
    /// Work completed successfully.
    Succeeded,
    /// Work ended with a stable public error.
    Failed,
    /// Cooperative cancellation completed.
    Cancelled,
    /// Restart or shutdown interrupted unfinished work.
    Interrupted,
}

/// Closed operation stage retained in production support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportOperationStage {
    /// Work was durably admitted.
    Accepted,
    /// Work was executing.
    Executing,
    /// Temporary resources or publication state were being finalized.
    Cleanup,
}

/// Monotonic operation progress retained in production support evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportOperationProgress {
    /// Completed bounded work units.
    pub completed: u32,
    /// Total bounded work units, or zero when unknown.
    pub total: u32,
}

/// Bounded primitive detail value retained from a checked public error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    tag = "type",
    content = "value",
    rename_all = "snake_case"
)]
pub enum SupportDetailValue {
    /// Boolean diagnostic property.
    Boolean(bool),
    /// Signed integer diagnostic property.
    Integer(i64),
    /// Unsigned integer diagnostic property.
    Unsigned(u64),
    /// Opaque lowercase hexadecimal repository identity.
    Repository(String),
    /// Opaque lowercase hexadecimal generation identity.
    Generation(String),
    /// Opaque lowercase hexadecimal operation identity.
    Operation(String),
    /// Source-free bounded diagnostic label.
    Label(String),
}

/// Stable remediation hint retained from a checked public error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "action", rename_all = "snake_case")]
pub enum SupportNextAction {
    /// Correct one named input field.
    CorrectField {
        /// Stable field name.
        field: String,
    },
    /// Retry the identical request.
    Retry,
    /// Select a compatible contract version.
    SelectSupportedVersion,
    /// Inspect the associated operation.
    InspectOperation,
    /// Rebuild the affected repository generation.
    RebuildRepository,
    /// Collect another protected support archive.
    CollectSupportBundle,
    /// Restart enumeration after an invalid continuation.
    RestartEnumeration,
    /// Update one checked configuration value.
    UpdateConfiguration {
        /// Canonical source-free configuration key.
        key: String,
    },
    /// Delete an unneeded repository through the supported catalog mutation.
    DeleteRepository,
}

/// Stable source-free terminal error retained in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportTerminalError {
    /// Stable public error family.
    pub code: ErrorCode,
    /// Whether an unchanged request may succeed when retried.
    pub retryable: bool,
    /// Optional bounded retry delay.
    pub retry_after_ms: Option<u64>,
    /// Opaque associated repository identity.
    pub repository_id: Option<String>,
    /// Opaque associated operation identity.
    pub operation_id: Option<String>,
    /// Opaque associated generation identity.
    pub generation_id: Option<String>,
    /// Checked source-free diagnostic details.
    pub details: std::collections::BTreeMap<String, SupportDetailValue>,
    /// Stable bounded remediation hints.
    pub next_actions: Vec<SupportNextAction>,
}

/// One bounded durable terminal operation retained in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportTerminalOperation {
    /// Opaque lowercase hexadecimal operation identity.
    pub operation_id: String,
    /// Opaque lowercase hexadecimal repository identity, when associated.
    pub repository_id: Option<String>,
    /// Submitted operation kind.
    pub kind: SupportOperationKind,
    /// Terminal lifecycle state.
    pub state: SupportOperationState,
    /// Final monotonic operation stage.
    pub stage: SupportOperationStage,
    /// Final durable lifecycle revision.
    pub revision: u64,
    /// Final monotonic progress snapshot.
    pub progress: SupportOperationProgress,
    /// Source-free adapter or provider label, when known.
    pub provider: Option<String>,
    /// Stable terminal failure, present only for failed work.
    pub error: Option<SupportTerminalError>,
}

/// Current operation counts plus bounded recent terminal evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportOperationsV4 {
    /// Current nonterminal operation counts.
    pub current: OperationsSummary,
    /// Recent durable terminal operations in newest-to-oldest order.
    pub recent_terminal: Vec<SupportTerminalOperation>,
}

/// Product and sanitized host facts included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportRuntimeInventory {
    /// Rootlight product version.
    pub product_version: String,
    /// Stable binary role.
    pub binary_name: String,
    /// SHA-256 of the running binary when the executable was readable.
    pub binary_sha256: Option<String>,
    /// Closed compile/runtime feature profile labels.
    pub feature_profile: Vec<String>,
    /// Negotiated protocol major.
    pub protocol_major: u32,
    /// Negotiated protocol minor.
    pub protocol_minor: u32,
    /// Sanitized logical processor count.
    pub logical_processors: u32,
    /// Sanitized physical memory bytes when available without private host data.
    pub physical_memory_bytes: Option<u64>,
}

/// One source-free dependency identity included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportDependencyInventory {
    /// Stable dependency name.
    pub name: String,
    /// Stable dependency version.
    pub version: String,
    /// Binary or package digest when available.
    pub sha256: Option<String>,
}

/// One source-free adapter identity included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportAdapterInventory {
    /// Stable adapter or provider name.
    pub name: String,
    /// Adapter implementation version when separately versioned.
    pub version: Option<String>,
    /// Closed language labels handled by the adapter.
    pub languages: Vec<String>,
    /// Whether the adapter is available in this process.
    pub available: bool,
    /// Whether analysis executes through an isolated host.
    pub isolated: bool,
    /// Adapter binary digest when a separate executable exists.
    pub binary_sha256: Option<String>,
    /// Grammar or model artifact digest when available.
    pub artifact_sha256: Option<String>,
}

/// One installed source-language capability included in production evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportLanguageCapabilityInventory {
    /// Canonical normalized language label.
    pub language: String,
    /// Audited filename suffixes, including the leading dot.
    pub suffixes: Vec<String>,
    /// Accepted detector aliases.
    pub aliases: Vec<String>,
    /// Installed detector families.
    pub detectors: Vec<String>,
    /// Highest installed analysis tier.
    pub maximum_tier: String,
    /// Installed analyzer or bounded fallback labels.
    pub analyzers: Vec<String>,
}

/// One source-free repository summary included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportRepositoryInventory {
    /// Opaque lowercase hexadecimal repository identity.
    pub repository_id: String,
    /// Deserialization-only reserved root fingerprint.
    ///
    /// Serialization omits this field and v4 validation requires `None` because
    /// an unsalted canonical-path digest remains linkable through path guessing.
    #[serde(default, skip_serializing)]
    pub root_fingerprint_sha256: Option<String>,
    /// Closed detected language labels.
    pub languages: Vec<String>,
    /// Closed completed analysis tier labels.
    pub tiers: Vec<String>,
    /// Source-free repository lifecycle state.
    pub state: String,
    /// Bounded indexed file count.
    pub file_count: u64,
    /// Bounded indexed symbol count.
    pub symbol_count: u64,
    /// Bounded indexed relationship count.
    pub relationship_count: u64,
    /// Number of retained immutable generations.
    pub generation_count: u32,
    /// Physical bytes attributed to this repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_bytes: Option<u64>,
    /// Active immutable generation bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_generation_bytes: Option<u64>,
    /// Direct active-predecessor bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_generation_bytes: Option<u64>,
    /// Other retained immutable generation bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_retained_generation_bytes: Option<u64>,
    /// Source-pool bytes referenced by retained generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pool_bytes: Option<u64>,
    /// Source-pool bytes referenced by multiple retained generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_source_bytes: Option<u64>,
    /// Unpublished temporary bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary_bytes: Option<u64>,
    /// Reclaimable physical bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    /// Live marker and metadata bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_overhead_bytes: Option<u64>,
    /// In-flight repository-budget reservation bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight_reservation_bytes: Option<u64>,
    /// Current repository-budget headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_headroom_bytes: Option<u64>,
}

/// Closed checksum state for one immutable generation summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportChecksumStatus {
    /// Checksums were verified successfully.
    Verified,
    /// Verification found a mismatch.
    Failed,
    /// No verification result is currently available.
    Unknown,
}

/// One source-free generation manifest header included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportGenerationInventory {
    /// Opaque lowercase hexadecimal repository identity.
    pub repository_id: String,
    /// Opaque lowercase hexadecimal generation identity.
    pub generation_id: String,
    /// Stable generation format label.
    pub format_version: String,
    /// Manifest checksum validation status.
    pub checksum_status: SupportChecksumStatus,
    /// Total immutable generation bytes.
    pub disk_bytes: u64,
    /// Source-free generation lifecycle state.
    pub state: String,
}

/// Effective non-secret daemon limits included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportConfigurationInventory {
    /// Configuration schema version.
    pub schema_version: u32,
    /// Global connection limit.
    pub connection_limit: u32,
    /// Per-client connection limit.
    pub client_connection_limit: u32,
    /// Bounded control queue capacity.
    pub control_queue_limit: u32,
    /// Global operation admission limit.
    pub operation_queue_limit: u32,
    /// Per-client operation admission limit.
    pub client_operation_limit: u32,
    /// Concurrent operation worker count.
    pub operation_workers: u32,
    /// Default request timeout in milliseconds.
    pub request_timeout_ms: u64,
    /// Maintenance interval in milliseconds.
    pub maintenance_interval_ms: u64,
    /// Shutdown grace period in milliseconds.
    pub shutdown_grace_ms: u64,
}

/// Source-free catalog and disk bounds included in production support evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportStorageInventory {
    /// Current catalog schema version.
    pub catalog_schema_version: u32,
    /// Current generation format version when the indexing service reported it.
    pub generation_format_version: Option<String>,
    /// Bundled SQLite runtime version.
    pub sqlite_version: String,
    /// Whether the catalog uses persistent storage.
    pub persistent: bool,
    /// Whether SQLite defensive mode is enabled.
    pub defensive: bool,
    /// Whether foreign-key enforcement is enabled.
    pub foreign_keys: bool,
    /// Whether SQLite trusted-schema behavior is enabled.
    pub trusted_schema: bool,
    /// Current allocated catalog bytes.
    pub catalog_allocated_bytes: u64,
    /// Maximum accepted catalog bytes.
    pub maximum_catalog_bytes: u64,
    /// Maximum accepted write-ahead-log bytes.
    pub maximum_wal_bytes: u64,
    /// Maximum accepted shared-memory sidecar bytes.
    pub maximum_shm_bytes: u64,
    /// Total immutable generation bytes reported by the indexing service.
    pub generation_disk_bytes: u64,
    /// Unreclaimed temporary bytes reported by the indexing service.
    pub unreclaimed_temporary_bytes: u64,
    /// Remaining disk margin when available.
    pub disk_margin_bytes: Option<u64>,
    /// Immutable bytes owned by active generations when fully classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_generation_bytes: Option<u64>,
    /// Immutable bytes owned by direct active predecessors when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_generation_bytes: Option<u64>,
    /// Immutable bytes owned by other retained generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_retained_generation_bytes: Option<u64>,
    /// Physical source-pool bytes referenced by retained generations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pool_bytes: Option<u64>,
    /// Physically shared immutable bytes when the storage engine can attribute them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_bytes: Option<u64>,
    /// Bytes protected from reclamation by an explicit durable pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_bytes: Option<u64>,
    /// Bytes currently eligible for safe reclamation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    /// Total physical bytes owned by the selected storage scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_storage_bytes: Option<u64>,
    /// Global physical bytes charged against the durable catalog budget.
    ///
    /// This remains catalog-wide when a support bundle projects one repository,
    /// because catalog admission and its reported headroom are global.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_accounted_bytes: Option<u64>,
    /// Bytes remaining after every admission reserve and safety margin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_margin_bytes: Option<u64>,
    /// Effective maximum retained generations per repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_retention_generations: Option<u32>,
    /// Live marker and repository-metadata bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_overhead_bytes: Option<u64>,
    /// Isolated corrupt or untrusted durable bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_bytes: Option<u64>,
    /// Whether explicit durable pins are implemented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinning_supported: Option<bool>,
    /// Catalog-budget bytes reserved by in-flight publications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight_catalog_reservation_bytes: Option<u64>,
    /// Repository-budget bytes reserved by in-flight publications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight_repository_reservation_bytes: Option<u64>,
    /// Minimum current repository-budget headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_headroom_bytes: Option<u64>,
    /// Current durable-catalog headroom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_headroom_bytes: Option<u64>,
    /// Current filesystem headroom after configured reserves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_headroom_bytes: Option<u64>,
    /// Provenance of the physical storage accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting_state: Option<SupportStorageAccountingState>,
    /// Effective maximum physical bytes per repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_repository_storage_bytes: Option<u64>,
    /// Effective maximum physical bytes in the durable catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_durable_catalog_bytes: Option<u64>,
    /// Effective minimum free bytes preserved on the durable volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_free_disk_bytes: Option<u64>,
    /// Effective source reservation multiplier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reservation_factor: Option<u64>,
    /// Effective semantic-oracle reservation multiplier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_reservation_factor: Option<u64>,
}

/// Provenance of a support inventory's physical storage accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportStorageAccountingState {
    /// Physical storage was deliberately not read.
    Unavailable,
    /// Physical storage was reconciled from Rootlight-owned mutations.
    Reconciled,
    /// Physical storage was rebuilt by a content-verifying scan.
    VerifiedScan,
}

/// Authoritative catalog-wide repository-capacity accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportRepositoryCapacityInventory {
    /// All committed and pending repository registrations.
    pub registered_repository_count: u32,
    /// Repository registrations with publication still in flight.
    pub pending_repository_count: u32,
    /// Committed repositories with an active generation.
    pub active_repository_count: u32,
    /// Retained committed repositories in a failed state.
    pub failed_repository_count: u32,
    /// Committed repositories protected by a durable pin.
    pub pinned_repository_count: u32,
    /// Committed repositories eligible for explicit deletion.
    pub reclaimable_repository_count: u32,
    /// Whether durable repository pinning is implemented.
    pub pinning_supported: bool,
    /// Configured maximum registered repositories.
    pub configured_maximum_repositories: u32,
    /// Effective maximum after durable-generation constraints.
    pub effective_maximum_repositories: u32,
    /// Remaining registrations admitted by the effective maximum.
    pub repository_headroom: u32,
}

/// Complete allow-listed production inventory accepted by the privacy boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupportInventory {
    /// Product, binary, protocol, feature, and sanitized hardware facts.
    pub runtime: SupportRuntimeInventory,
    /// Bounded dependency inventory.
    pub dependencies: Vec<SupportDependencyInventory>,
    /// Bounded adapter inventory.
    pub adapters: Vec<SupportAdapterInventory>,
    /// Authoritative installed language detection and analysis capabilities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<SupportLanguageCapabilityInventory>,
    /// Bounded repository inventory.
    pub repositories: Vec<SupportRepositoryInventory>,
    /// Catalog-wide repository-capacity accounting, independent of detail truncation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_capacity: Option<SupportRepositoryCapacityInventory>,
    /// Bounded generation manifest headers.
    pub generations: Vec<SupportGenerationInventory>,
    /// Effective non-secret configuration.
    pub configuration: SupportConfigurationInventory,
    /// Catalog, SQLite, and disk facts.
    pub storage: SupportStorageInventory,
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
    /// Bounded repository list.
    RepositoryList,
    /// One repository status.
    RepositoryStatus,
    /// Generation-pinned symbol relationship expansion.
    SymbolRelationships,
    /// Generation-pinned bounded flow trace.
    FlowTrace,
    /// Generation-pinned bounded architecture cycle detection.
    ArchitectureCycles,
    /// Generation-pinned bounded dead-code detection.
    CodeDead,
    /// Generation-pinned bounded architecture overview aggregation.
    ArchitectureOverview,
    /// Generation-pinned bounded test selection.
    TestsSelect,
    /// Generation-pinned bounded change impact.
    ChangeImpact,
    /// Generation-pinned bounded change planning.
    PlanChange,
    /// Generation-pinned bounded history comparison.
    HistoryCompare,
    /// Generation-pinned bounded advanced query over a safe typed AST.
    QueryAdvanced,
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
        Self::RepositoryList,
        Self::RepositoryStatus,
        Self::SymbolRelationships,
        Self::FlowTrace,
        Self::ArchitectureCycles,
        Self::CodeDead,
        Self::ArchitectureOverview,
        Self::TestsSelect,
        Self::ChangeImpact,
        Self::PlanChange,
        Self::HistoryCompare,
        Self::QueryAdvanced,
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
            Self::RepositoryList => 13,
            Self::RepositoryStatus => 14,
            Self::SymbolRelationships => 15,
            Self::FlowTrace => 16,
            Self::ArchitectureCycles => 17,
            Self::CodeDead => 18,
            Self::ArchitectureOverview => 19,
            Self::TestsSelect => 20,
            Self::ChangeImpact => 21,
            Self::PlanChange => 22,
            Self::HistoryCompare => 23,
            Self::QueryAdvanced => 24,
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

/// Closed authority category retained for cancellation audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationAuditAuthority {
    /// An authenticated owner submitted the public cancellation request.
    Client,
    /// The owning client connection disappeared or abandoned its request.
    InternalClientDisconnect,
    /// The operation deadline elapsed.
    InternalDeadline,
    /// Daemon shutdown or compensation stopped the operation.
    InternalShutdown,
    /// A parent operation cancelled dependent work.
    InternalParent,
    /// A bounded resource policy stopped the operation.
    InternalResourceLimit,
}

/// Closed result category retained for every cancellation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationAuditOutcome {
    /// Cancellation first became durable.
    Accepted,
    /// An authorized request replayed durable cancellation.
    Replayed,
    /// A foreign client was denied without lifecycle mutation.
    Denied,
    /// Completion or publication cleanup had already closed cancellation.
    TooLate,
    /// No durable operation existed.
    NotFound,
    /// Cancellation failed for an infrastructure reason.
    Failed,
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
    /// One owned shutdown phase completed or its waiter was abandoned.
    ShutdownPhaseCompleted {
        /// Closed shutdown responsibility, without thread or repository names.
        phase: ShutdownPhase,
        /// Completion of the waiter, not necessarily of an abandoned worker.
        outcome: TelemetryOutcome,
        /// Monotonic elapsed microseconds.
        duration_us: u64,
    },
    /// One cancellation authorization and lifecycle decision completed.
    CancellationAttempt {
        /// Domain-separated truncated SHA-256 of the operation identifier.
        operation_digest: [u8; 16],
        /// Closed authenticated or daemon authority category.
        authority: CancellationAuditAuthority,
        /// Closed authorization and lifecycle result.
        outcome: CancellationAuditOutcome,
        /// Stable public failure code, when the attempt did not succeed.
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
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

/// Closed responsibilities drained during daemon shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownPhase {
    /// First-slice service and background worker threads.
    ServiceWorkers,
    /// Accepted connections, admitted submissions, and orchestration workers.
    ControlDrain,
    /// The durable operation journal actor thread.
    Journal,
}

/// Closed completed-span kind retained for local diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// Daemon process startup.
    DaemonStartup,
    /// Daemon graceful shutdown.
    DaemonShutdown,
    /// One daemon shutdown responsibility, also emitted as a structured log.
    DaemonShutdownPhase {
        /// Closed shutdown responsibility.
        phase: ShutdownPhase,
    },
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
    /// Production schema with inventory and bounded terminal operations.
    V4,
    /// Production schema with explicit optional storage-accounting dimensions.
    V5,
    /// Production schema with authoritative installed language capabilities.
    V6,
    /// Production schema with reconciliable durable storage accounting.
    V7,
    /// Production schema with authoritative repository-capacity accounting.
    V8,
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
    /// Recent durable terminal operations required by schema v4.
    pub terminal_operations: Vec<SupportTerminalOperation>,
    /// Production runtime, dependency, adapter, repository, and storage inventory.
    pub inventory: Option<SupportInventory>,
    /// Pre-assembly normalized telemetry for schema v2 and later.
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

    /// Records one bounded source-free cancellation audit decision.
    pub fn record_cancellation_attempt(
        &self,
        operation: [u8; 16],
        authority: CancellationAuditAuthority,
        outcome: CancellationAuditOutcome,
        error_code: Option<ErrorCode>,
    ) {
        let mut hasher = Sha256::new();
        hasher.update(b"rootlight/cancellation-audit/operation/v1\0");
        hasher.update(operation);
        let digest = hasher.finalize();
        let mut operation_digest = [0_u8; 16];
        operation_digest.copy_from_slice(&digest[..16]);
        self.record_log(LogEvent::CancellationAttempt {
            operation_digest,
            authority,
            outcome,
            error_code,
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
        // Sequence allocation, ring order, and external emission share one
        // linearization point so concurrent callers cannot publish out of order.
        let mut logs = self
            .logs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        logs.push(record);
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
        // Completion order, retained order, and sequence order must agree for
        // clients that validate bounded telemetry snapshots.
        let mut traces = self
            .traces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sequence) = self.next_sequence() else {
            return;
        };
        let elapsed = started.elapsed();
        let elapsed_us = duration_us(elapsed);
        let started_uptime_us = duration_us(self.started.elapsed().saturating_sub(elapsed));
        traces.push(CompletedSpan {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            sequence,
            started_uptime_us,
            duration_us: elapsed_us,
            kind,
            outcome,
            error_code,
        });
        drop(traces);
        if let SpanKind::DaemonShutdownPhase { phase } = kind {
            // Shutdown can terminate before a support snapshot is collected.
            // Keep its bounded phase outcome in the ordinary stderr log too.
            self.record_log(LogEvent::ShutdownPhaseCompleted {
                phase,
                outcome,
                duration_us: elapsed_us,
            });
        }
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
        LogEvent::ShutdownPhaseCompleted {
            outcome: TelemetryOutcome::Succeeded,
            ..
        } => (LogSeverity::Info, TelemetryTarget::Daemon),
        LogEvent::ShutdownPhaseCompleted { .. } => (LogSeverity::Warn, TelemetryTarget::Daemon),
        LogEvent::CancellationAttempt {
            outcome: CancellationAuditOutcome::Accepted | CancellationAuditOutcome::Replayed,
            ..
        } => (LogSeverity::Info, TelemetryTarget::Operation),
        LogEvent::CancellationAttempt { .. } => (LogSeverity::Warn, TelemetryTarget::Operation),
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
        SupportBundleSchema::V4 => ProtocolVersion::V1_8,
        SupportBundleSchema::V5 => ProtocolVersion::V1_12,
        SupportBundleSchema::V6 => ProtocolVersion::V1_13,
        SupportBundleSchema::V7 => ProtocolVersion::V1_14,
        SupportBundleSchema::V8 => ProtocolVersion::V1_15,
    };
    if input.protocol_version != expected_protocol {
        return Err(SupportBundleError::ProtocolVersionMismatch);
    }
    match schema {
        SupportBundleSchema::V1 => build_support_bundle_v1(input),
        SupportBundleSchema::V2 => build_support_bundle_v2(input),
        SupportBundleSchema::V3 => build_support_bundle_v3(input),
        SupportBundleSchema::V4 => build_support_bundle_v4(input),
        SupportBundleSchema::V5 => build_support_bundle_v5(input),
        SupportBundleSchema::V6 => build_support_bundle_v6(input),
        SupportBundleSchema::V7 => build_support_bundle_v7(input),
        SupportBundleSchema::V8 => build_support_bundle_v8(input),
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
    let redaction = redaction_entry(SUPPORT_BUNDLE_SCHEMA_VERSION_V3, &OMITTED_DATA_CLASSES_V3)?;
    let telemetry = json_entry_with_limit("telemetry.json", telemetry, MAX_TELEMETRY_ENTRY_BYTES)?;
    let manifest = support_manifest_entry(
        SUPPORT_BUNDLE_SCHEMA_VERSION_V3,
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

fn build_support_bundle_v4(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    if input
        .inventory
        .as_ref()
        .is_some_and(|inventory| inventory.storage.has_extended_accounting())
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    build_production_support_bundle(
        input,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V4,
        &OMITTED_DATA_CLASSES_V4,
    )
}

fn build_support_bundle_v5(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    if input
        .inventory
        .as_ref()
        .is_some_and(|inventory| !inventory.languages.is_empty())
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    build_production_support_bundle(
        input,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V5,
        &OMITTED_DATA_CLASSES_V5,
    )
}

fn build_support_bundle_v6(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    if input
        .inventory
        .as_ref()
        .is_some_and(SupportInventory::has_authoritative_storage_accounting)
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    build_production_support_bundle(
        input,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V6,
        &OMITTED_DATA_CLASSES_V6,
    )
}

fn build_support_bundle_v7(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    build_production_support_bundle(
        input,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V7,
        &OMITTED_DATA_CLASSES_V7,
    )
}

fn build_support_bundle_v8(
    input: &SupportBundleInput,
) -> Result<SupportBundle, SupportBundleError> {
    build_production_support_bundle(
        input,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V8,
        &OMITTED_DATA_CLASSES_V8,
    )
}

fn build_production_support_bundle(
    input: &SupportBundleInput,
    schema_version: u32,
    omitted_data_classes: &[&str],
) -> Result<SupportBundle, SupportBundleError> {
    let telemetry = input
        .telemetry
        .as_ref()
        .ok_or(SupportBundleError::MissingTelemetry)?;
    let inventory = input
        .inventory
        .as_ref()
        .ok_or(SupportBundleError::MissingInventory)?;
    validate_production_input(input, inventory, schema_version)?;
    let diagnostics = json_entry("diagnostics/quick.json", &input.diagnostics)?;
    let health = json_entry("health.json", &input.health)?;
    let inventory = json_entry("inventory.json", inventory)?;
    let operations = json_entry(
        "operations-summary.json",
        &SupportOperationsV4 {
            current: input.operations,
            recent_terminal: input.terminal_operations.clone(),
        },
    )?;
    let redaction = redaction_entry(schema_version, omitted_data_classes)?;
    let telemetry = json_entry_with_limit("telemetry.json", telemetry, MAX_TELEMETRY_ENTRY_BYTES)?;
    let manifest = support_manifest_entry(
        schema_version,
        input,
        [
            &diagnostics,
            &health,
            &inventory,
            &operations,
            &redaction,
            &telemetry,
        ],
    )?;
    finish_support_bundle(&[
        diagnostics,
        health,
        inventory,
        manifest,
        operations,
        redaction,
        telemetry,
    ])
}

fn validate_production_input(
    input: &SupportBundleInput,
    inventory: &SupportInventory,
    schema_version: u32,
) -> Result<(), SupportBundleError> {
    let minimum_protocol_minor = match schema_version {
        SUPPORT_BUNDLE_SCHEMA_VERSION_V8 => 15,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V7 => 14,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V6 => 13,
        SUPPORT_BUNDLE_SCHEMA_VERSION_V5 => 12,
        _ => 8,
    };
    if input.terminal_operations.len() > MAX_SUPPORT_TERMINAL_OPERATIONS
        || inventory.dependencies.len() > MAX_SUPPORT_DEPENDENCIES
        || inventory.adapters.len() > MAX_SUPPORT_ADAPTERS
        || inventory.languages.len() > MAX_SUPPORT_LANGUAGE_CAPABILITIES
        || inventory.repositories.len() > MAX_SUPPORT_REPOSITORIES
        || inventory.generations.len() > MAX_SUPPORT_GENERATIONS
        || inventory.runtime.protocol_major != 1
        || inventory.runtime.protocol_minor < minimum_protocol_minor
        || inventory.runtime.logical_processors == 0
        || inventory.configuration.schema_version == 0
        || inventory.storage.catalog_schema_version == 0
        || inventory.storage.catalog_schema_version != input.health.catalog_schema_version
        || !is_support_label(&inventory.runtime.product_version)
        || !is_support_label(&inventory.runtime.binary_name)
        || !optional_digest_is_valid(inventory.runtime.binary_sha256.as_deref())
        || inventory.runtime.feature_profile.is_empty()
        || !labels_are_valid(&inventory.runtime.feature_profile)
        || !is_support_label(&inventory.storage.sqlite_version)
        || !optional_label_is_valid(inventory.storage.generation_format_version.as_deref())
        || inventory
            .storage
            .active_generation_bytes
            .is_some_and(|bytes| bytes > inventory.storage.generation_disk_bytes)
        || inventory
            .storage
            .predecessor_generation_bytes
            .is_some_and(|bytes| bytes > inventory.storage.generation_disk_bytes)
        || inventory
            .storage
            .effective_retention_generations
            .is_some_and(|generations| generations == 0)
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    for dependency in &inventory.dependencies {
        if !is_support_label(&dependency.name)
            || !is_support_label(&dependency.version)
            || !optional_digest_is_valid(dependency.sha256.as_deref())
        {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    for adapter in &inventory.adapters {
        if !is_support_label(&adapter.name)
            || !optional_label_is_valid(adapter.version.as_deref())
            || adapter.languages.len() > MAX_SUPPORT_RECORD_LABELS
            || !labels_are_valid(&adapter.languages)
            || !optional_digest_is_valid(adapter.binary_sha256.as_deref())
            || !optional_digest_is_valid(adapter.artifact_sha256.as_deref())
        {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    let mut languages = std::collections::BTreeSet::new();
    for language in &inventory.languages {
        if !languages.insert(language.language.as_str())
            || !is_language_capability_label(&language.language)
            || language.suffixes.len() > MAX_SUPPORT_RECORD_LABELS
            || language.aliases.len() > MAX_SUPPORT_RECORD_LABELS
            || language.detectors.is_empty()
            || language.detectors.len() > MAX_SUPPORT_RECORD_LABELS
            || language.analyzers.is_empty()
            || language.analyzers.len() > MAX_SUPPORT_RECORD_LABELS
            || !is_language_capability_label(&language.maximum_tier)
            || !language
                .suffixes
                .iter()
                .all(|label| is_language_capability_label(label))
            || !language
                .aliases
                .iter()
                .all(|label| is_language_capability_label(label))
            || !language
                .detectors
                .iter()
                .all(|label| is_language_capability_label(label))
            || !language
                .analyzers
                .iter()
                .all(|label| is_language_capability_label(label))
        {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    if schema_version >= SUPPORT_BUNDLE_SCHEMA_VERSION_V6 && inventory.languages.is_empty()
        || schema_version < SUPPORT_BUNDLE_SCHEMA_VERSION_V6 && !inventory.languages.is_empty()
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    let mut repository_ids = std::collections::BTreeSet::new();
    for repository in &inventory.repositories {
        if !is_opaque_id(&repository.repository_id)
            || !repository_ids.insert(repository.repository_id.as_str())
            || repository.root_fingerprint_sha256.is_some()
            || repository.languages.len() > MAX_SUPPORT_RECORD_LABELS
            || repository.tiers.len() > MAX_SUPPORT_RECORD_LABELS
            || !labels_are_valid(&repository.languages)
            || !labels_are_valid(&repository.tiers)
            || !is_support_label(&repository.state)
        {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    let mut generation_ids = std::collections::BTreeSet::new();
    for generation in &inventory.generations {
        if !is_opaque_id(&generation.repository_id)
            || !is_opaque_id(&generation.generation_id)
            || !generation_ids.insert(generation.generation_id.as_str())
            || !is_support_label(&generation.format_version)
            || !is_support_label(&generation.state)
        {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    for operation in &input.terminal_operations {
        validate_terminal_operation(operation, schema_version)?;
    }
    if schema_version >= SUPPORT_BUNDLE_SCHEMA_VERSION_V7 {
        validate_authoritative_storage_accounting(inventory)?;
    }
    match (
        schema_version == SUPPORT_BUNDLE_SCHEMA_VERSION_V8,
        inventory.repository_capacity.as_ref(),
    ) {
        (true, Some(capacity)) => validate_repository_capacity(capacity)?,
        (false, None) => {}
        _ => return Err(SupportBundleError::InvalidInventory),
    }
    Ok(())
}

impl SupportStorageInventory {
    const fn has_extended_accounting(&self) -> bool {
        self.active_generation_bytes.is_some()
            || self.predecessor_generation_bytes.is_some()
            || self.shared_bytes.is_some()
            || self.pinned_bytes.is_some()
            || self.reclaimable_bytes.is_some()
            || self.total_storage_bytes.is_some()
            || self.catalog_accounted_bytes.is_some()
            || self.admission_margin_bytes.is_some()
            || self.effective_retention_generations.is_some()
    }
}

impl SupportInventory {
    fn has_authoritative_storage_accounting(&self) -> bool {
        self.storage.has_authoritative_accounting()
            || self
                .repositories
                .iter()
                .any(SupportRepositoryInventory::has_storage_accounting)
    }
}

impl SupportStorageInventory {
    fn has_authoritative_accounting(&self) -> bool {
        self.catalog_accounted_bytes.is_some()
            || self.other_retained_generation_bytes.is_some()
            || self.source_pool_bytes.is_some()
            || self.repository_overhead_bytes.is_some()
            || self.quarantine_bytes.is_some()
            || self.pinning_supported.is_some()
            || self.inflight_catalog_reservation_bytes.is_some()
            || self.inflight_repository_reservation_bytes.is_some()
            || self.repository_headroom_bytes.is_some()
            || self.catalog_headroom_bytes.is_some()
            || self.filesystem_headroom_bytes.is_some()
            || self.accounting_state.is_some()
            || self.maximum_repository_storage_bytes.is_some()
            || self.maximum_durable_catalog_bytes.is_some()
            || self.minimum_free_disk_bytes.is_some()
            || self.source_reservation_factor.is_some()
            || self.oracle_reservation_factor.is_some()
    }
}

impl SupportRepositoryInventory {
    fn has_storage_accounting(&self) -> bool {
        self.storage_bytes.is_some()
            || self.active_generation_bytes.is_some()
            || self.predecessor_generation_bytes.is_some()
            || self.other_retained_generation_bytes.is_some()
            || self.source_pool_bytes.is_some()
            || self.shared_source_bytes.is_some()
            || self.temporary_bytes.is_some()
            || self.reclaimable_bytes.is_some()
            || self.repository_overhead_bytes.is_some()
            || self.inflight_reservation_bytes.is_some()
            || self.repository_headroom_bytes.is_some()
    }
}

fn validate_authoritative_storage_accounting(
    inventory: &SupportInventory,
) -> Result<(), SupportBundleError> {
    let storage = &inventory.storage;
    let Some(accounting_state) = storage.accounting_state else {
        return Err(SupportBundleError::InvalidInventory);
    };
    let (
        Some(maximum_repository_bytes),
        Some(maximum_catalog_bytes),
        Some(minimum_free_bytes),
        Some(retention),
        Some(source_factor),
        Some(oracle_factor),
        Some(pinning_supported),
        Some(pinned_bytes),
    ) = (
        storage.maximum_repository_storage_bytes,
        storage.maximum_durable_catalog_bytes,
        storage.minimum_free_disk_bytes,
        storage.effective_retention_generations,
        storage.source_reservation_factor,
        storage.oracle_reservation_factor,
        storage.pinning_supported,
        storage.pinned_bytes,
    )
    else {
        return Err(SupportBundleError::InvalidInventory);
    };
    if maximum_repository_bytes == 0
        || maximum_catalog_bytes == 0
        || retention == 0
        || source_factor == 0
        || oracle_factor == 0
        || !pinning_supported && pinned_bytes != 0
    {
        return Err(SupportBundleError::InvalidInventory);
    }

    if accounting_state == SupportStorageAccountingState::Unavailable {
        if storage.total_storage_bytes.is_some()
            || storage.active_generation_bytes.is_some()
            || storage.predecessor_generation_bytes.is_some()
            || storage.other_retained_generation_bytes.is_some()
            || storage.source_pool_bytes.is_some()
            || storage.shared_bytes.is_some()
            || storage.reclaimable_bytes.is_some()
            || storage.catalog_accounted_bytes.is_some()
            || storage.repository_overhead_bytes.is_some()
            || storage.quarantine_bytes.is_some()
            || storage.inflight_catalog_reservation_bytes.is_some()
            || storage.inflight_repository_reservation_bytes.is_some()
            || storage.repository_headroom_bytes.is_some()
            || storage.catalog_headroom_bytes.is_some()
            || storage.filesystem_headroom_bytes.is_some()
            || storage.admission_margin_bytes.is_some()
            || inventory
                .repositories
                .iter()
                .any(SupportRepositoryInventory::has_storage_accounting)
        {
            return Err(SupportBundleError::InvalidInventory);
        }
        return Ok(());
    }

    let (
        Some(total),
        Some(catalog_accounted),
        Some(active),
        Some(predecessor),
        Some(other_retained),
        Some(source_pool),
        Some(shared),
        Some(reclaimable),
        Some(overhead),
        Some(quarantine),
        Some(inflight_catalog),
        Some(_inflight_repository),
        Some(repository_headroom),
        Some(catalog_headroom),
        Some(filesystem_headroom),
        Some(admission_headroom),
        Some(available_bytes),
    ) = (
        storage.total_storage_bytes,
        storage.catalog_accounted_bytes,
        storage.active_generation_bytes,
        storage.predecessor_generation_bytes,
        storage.other_retained_generation_bytes,
        storage.source_pool_bytes,
        storage.shared_bytes,
        storage.reclaimable_bytes,
        storage.repository_overhead_bytes,
        storage.quarantine_bytes,
        storage.inflight_catalog_reservation_bytes,
        storage.inflight_repository_reservation_bytes,
        storage.repository_headroom_bytes,
        storage.catalog_headroom_bytes,
        storage.filesystem_headroom_bytes,
        storage.admission_margin_bytes,
        storage.disk_margin_bytes,
    )
    else {
        return Err(SupportBundleError::InvalidInventory);
    };
    let reconciled_total = u128::from(active)
        + u128::from(predecessor)
        + u128::from(other_retained)
        + u128::from(source_pool)
        + u128::from(storage.unreclaimed_temporary_bytes)
        + u128::from(reclaimable)
        + u128::from(overhead)
        + u128::from(quarantine);
    if reconciled_total != u128::from(total)
        || catalog_accounted < total
        || shared > source_pool
        || catalog_headroom
            != maximum_catalog_bytes
                .saturating_sub(catalog_accounted.saturating_add(inflight_catalog))
        || filesystem_headroom
            != available_bytes
                .saturating_sub(minimum_free_bytes)
                .saturating_sub(inflight_catalog)
        || admission_headroom
            != repository_headroom
                .min(catalog_headroom)
                .min(filesystem_headroom)
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    let mut minimum_repository_headroom = maximum_repository_bytes;
    for repository in &inventory.repositories {
        let (
            Some(repository_total),
            Some(active),
            Some(predecessor),
            Some(other_retained),
            Some(source_pool),
            Some(shared),
            Some(temporary),
            Some(reclaimable),
            Some(overhead),
            Some(inflight),
            Some(headroom),
        ) = (
            repository.storage_bytes,
            repository.active_generation_bytes,
            repository.predecessor_generation_bytes,
            repository.other_retained_generation_bytes,
            repository.source_pool_bytes,
            repository.shared_source_bytes,
            repository.temporary_bytes,
            repository.reclaimable_bytes,
            repository.repository_overhead_bytes,
            repository.inflight_reservation_bytes,
            repository.repository_headroom_bytes,
        )
        else {
            return Err(SupportBundleError::InvalidInventory);
        };
        if u128::from(repository_total)
            != u128::from(active)
                + u128::from(predecessor)
                + u128::from(other_retained)
                + u128::from(source_pool)
                + u128::from(temporary)
                + u128::from(reclaimable)
                + u128::from(overhead)
            || shared > source_pool
            || headroom
                != maximum_repository_bytes
                    .saturating_sub(repository_total.saturating_add(inflight))
        {
            return Err(SupportBundleError::InvalidInventory);
        }
        minimum_repository_headroom = minimum_repository_headroom.min(headroom);
    }
    if repository_headroom > minimum_repository_headroom {
        return Err(SupportBundleError::InvalidInventory);
    }
    Ok(())
}

fn validate_terminal_operation(
    operation: &SupportTerminalOperation,
    schema_version: u32,
) -> Result<(), SupportBundleError> {
    if !is_opaque_id(&operation.operation_id)
        || operation
            .repository_id
            .as_deref()
            .is_some_and(|value| !is_opaque_id(value))
        || !optional_label_is_valid(operation.provider.as_deref())
        || operation.progress.total != 0 && operation.progress.completed > operation.progress.total
        || (operation.state == SupportOperationState::Failed) != operation.error.is_some()
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    let Some(error) = operation.error.as_ref() else {
        return Ok(());
    };
    if error.retry_after_ms.is_some_and(|delay| delay > 86_400_000)
        || error.retry_after_ms.is_some() && !error.retryable
        || error
            .repository_id
            .as_deref()
            .is_some_and(|value| !is_opaque_id(value))
        || error
            .operation_id
            .as_deref()
            .is_some_and(|value| value != operation.operation_id)
        || error
            .generation_id
            .as_deref()
            .is_some_and(|value| !is_opaque_id(value))
        || error.details.len() > 32
        || error.next_actions.len() > 8
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    for (key, value) in &error.details {
        if !is_detail_key(key) || !support_detail_is_valid(value) {
            return Err(SupportBundleError::InvalidInventory);
        }
    }
    for action in &error.next_actions {
        match action {
            SupportNextAction::CorrectField { field } if !is_detail_key(field) => {
                return Err(SupportBundleError::InvalidInventory);
            }
            SupportNextAction::UpdateConfiguration { key }
                if schema_version < SUPPORT_BUNDLE_SCHEMA_VERSION_V8 || !is_support_label(key) =>
            {
                return Err(SupportBundleError::InvalidInventory);
            }
            SupportNextAction::DeleteRepository
                if schema_version < SUPPORT_BUNDLE_SCHEMA_VERSION_V8 =>
            {
                return Err(SupportBundleError::InvalidInventory);
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_repository_capacity(
    capacity: &SupportRepositoryCapacityInventory,
) -> Result<(), SupportBundleError> {
    let Some(committed) = capacity
        .active_repository_count
        .checked_add(capacity.failed_repository_count)
    else {
        return Err(SupportBundleError::InvalidInventory);
    };
    let Some(registered) = committed.checked_add(capacity.pending_repository_count) else {
        return Err(SupportBundleError::InvalidInventory);
    };
    let Some(headroom) = capacity
        .effective_maximum_repositories
        .checked_sub(capacity.registered_repository_count)
    else {
        return Err(SupportBundleError::InvalidInventory);
    };
    let Some(protected_or_reclaimable) = capacity
        .pinned_repository_count
        .checked_add(capacity.reclaimable_repository_count)
    else {
        return Err(SupportBundleError::InvalidInventory);
    };
    if capacity.configured_maximum_repositories == 0
        || capacity.effective_maximum_repositories == 0
        || capacity.effective_maximum_repositories > capacity.configured_maximum_repositories
        || registered != capacity.registered_repository_count
        || headroom != capacity.repository_headroom
        || capacity.pinned_repository_count > committed
        || capacity.reclaimable_repository_count > committed
        || protected_or_reclaimable > committed
        || !capacity.pinning_supported
            && (capacity.pinned_repository_count != 0
                || capacity.reclaimable_repository_count != committed)
    {
        return Err(SupportBundleError::InvalidInventory);
    }
    Ok(())
}

fn support_detail_is_valid(value: &SupportDetailValue) -> bool {
    match value {
        SupportDetailValue::Boolean(_)
        | SupportDetailValue::Integer(_)
        | SupportDetailValue::Unsigned(_) => true,
        SupportDetailValue::Repository(value)
        | SupportDetailValue::Generation(value)
        | SupportDetailValue::Operation(value) => is_opaque_id(value),
        SupportDetailValue::Label(value) => is_support_label(value),
    }
}

fn labels_are_valid(labels: &[String]) -> bool {
    labels.iter().all(|value| is_support_label(value))
}

fn optional_label_is_valid(value: Option<&str>) -> bool {
    value.is_none_or(is_support_label)
}

fn optional_digest_is_valid(value: Option<&str>) -> bool {
    value.is_none_or(is_sha256)
}

fn is_detail_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_support_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn is_language_capability_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
        })
}

fn is_opaque_id(value: &str) -> bool {
    matches!(value.len(), 32 | 40)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        LogEvent::CancellationAttempt { .. } | LogEvent::ShutdownPhaseCompleted { .. } => false,
    }
}

const fn span_kind_supported_by_v2(kind: SpanKind) -> bool {
    match kind {
        SpanKind::DaemonShutdownPhase { .. } => false,
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
        // Formatting must not displace valid evidence that fits the same byte limit.
        // Preserve existing pretty output whenever possible; never remove JSON values.
        bytes = serde_json::to_vec(value).map_err(SupportBundleError::SerializeJson)?;
        bytes.push(b'\n');
    }
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
    /// A telemetry-bearing schema was selected without a normalized snapshot.
    #[error("support bundle telemetry is required for this schema")]
    MissingTelemetry,
    /// A production schema was selected without inventory.
    #[error("support bundle inventory is required for this schema")]
    MissingInventory,
    /// Production inventory violated a reviewed size or privacy bound.
    #[error("support bundle inventory violates its bounded schema")]
    InvalidInventory,
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
    use std::{io::Read as _, sync::Barrier, thread};

    use super::*;

    #[test]
    fn json_entry_compacts_formatting_without_changing_values_or_limits() {
        let value = serde_json::json!({
            "records": [{"label": "alpha  beta\n\t\"λ\"", "count": 7}, {"known": null}]
        });
        let mut compact = serde_json::to_vec(&value).expect("fixture serializes");
        compact.push(b'\n');
        assert!(
            serde_json::to_vec_pretty(&value)
                .expect("fixture serializes")
                .len()
                > compact.len()
        );
        let entry = json_entry_with_limit("fixture.json", &value, compact.len())
            .expect("formatting alone must not exhaust the entry budget");
        assert_eq!(entry.bytes, compact);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&entry.bytes).expect("entry decodes"),
            value
        );
        for maximum in [0, compact.len() - 1] {
            assert!(matches!(
                json_entry_with_limit("fixture.json", &value, maximum),
                Err(SupportBundleError::EntryTooLarge {
                    name: "fixture.json"
                })
            ));
        }
    }

    #[test]
    fn json_entry_preserves_pretty_bytes_when_they_fit() {
        let value = serde_json::json!({"records": [1, 2, 3]});
        let mut expected = serde_json::to_vec_pretty(&value).expect("fixture serializes");
        expected.push(b'\n');
        let entry = json_entry_with_limit("fixture.json", &value, expected.len())
            .expect("exact pretty byte boundary fits");
        assert_eq!(entry.bytes, expected);
    }

    #[test]
    fn schema_v8_preserves_large_inventory_within_the_entry_budget() {
        let mut input = production_input();
        make_schema_v8_input(&mut input);
        input.terminal_operations.clear();
        let inventory = input.inventory.as_mut().expect("fixture inventory exists");
        let mut repository = inventory.repositories[0].clone();
        repository.generation_count = 2;
        repository.predecessor_generation_bytes = Some(4_096);
        repository.storage_bytes = Some(9_344);
        repository.repository_headroom_bytes = Some(1_039_104);
        let generation = inventory.generations[0].clone();
        let count = u32::try_from(MAX_SUPPORT_REPOSITORIES).expect("support count fits u32");
        inventory.repositories = (1..=count)
            .map(|id| SupportRepositoryInventory {
                repository_id: format!("{id:032x}"),
                ..repository.clone()
            })
            .collect();
        let generation_count =
            u32::try_from(MAX_SUPPORT_GENERATIONS).expect("generation count fits u32");
        assert_eq!(generation_count, count * 2);
        inventory.generations = (1..=generation_count)
            .map(|id| SupportGenerationInventory {
                repository_id: format!("{:032x}", (id - 1) % count + 1),
                generation_id: format!("{id:032x}"),
                state: if id <= count { "active" } else { "superseded" }.to_owned(),
                ..generation.clone()
            })
            .collect();
        let capacity = inventory
            .repository_capacity
            .as_mut()
            .expect("capacity exists");
        capacity.registered_repository_count = count;
        capacity.active_repository_count = count;
        capacity.pending_repository_count = 0;
        capacity.reclaimable_repository_count = count;
        capacity.repository_headroom = capacity.effective_maximum_repositories - count;
        let count = u64::from(count);
        let storage = &mut inventory.storage;
        storage.generation_disk_bytes = 8_192 * count;
        storage.active_generation_bytes = Some(4_096 * count);
        storage.predecessor_generation_bytes = Some(4_096 * count);
        storage.source_pool_bytes = Some(1_024 * count);
        storage.repository_overhead_bytes = Some(128 * count);
        let total = 9_344 * count;
        let inflight = 128 * count;
        storage.total_storage_bytes = Some(total);
        storage.catalog_accounted_bytes = Some(total);
        storage.inflight_catalog_reservation_bytes = Some(inflight);
        let catalog_headroom = 2_097_152 - total - inflight;
        let filesystem_headroom = 3_145_728 - 1_048_576 - inflight;
        storage.catalog_headroom_bytes = Some(catalog_headroom);
        storage.filesystem_headroom_bytes = Some(filesystem_headroom);
        storage.repository_headroom_bytes = Some(1_039_104);
        storage.admission_margin_bytes =
            Some(1_039_104.min(catalog_headroom).min(filesystem_headroom));
        assert!(
            serde_json::to_vec_pretty(inventory)
                .expect("inventory serializes")
                .len()
                > MAX_SUPPORT_ENTRY_BYTES
        );
        assert!(
            serde_json::to_vec(inventory)
                .expect("inventory serializes")
                .len()
                < MAX_SUPPORT_ENTRY_BYTES
        );
        let expected = inventory.clone();
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V8)
            .expect("valid inventory fits without removing records");
        assert_eq!(
            bundle,
            build_support_bundle_for_schema(&input, SupportBundleSchema::V8)
                .expect("deterministic rebuild")
        );
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("archive opens");
        let mut bytes = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory exists")
            .read_to_end(&mut bytes)
            .expect("inventory reads");
        assert!(bytes.len() <= MAX_SUPPORT_ENTRY_BYTES);
        assert_eq!(
            serde_json::from_slice::<SupportInventory>(&bytes).expect("inventory decodes"),
            expected
        );
        input
            .inventory
            .as_mut()
            .expect("inventory exists")
            .storage
            .total_storage_bytes = Some(total + 1);
        assert!(matches!(
            build_support_bundle_for_schema(&input, SupportBundleSchema::V8),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

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
            terminal_operations: Vec::new(),
            inventory: None,
            telemetry: None,
        }
    }

    fn production_input() -> SupportBundleInput {
        let mut input = input();
        input.protocol_version = ProtocolVersion::V1_12;
        input.telemetry = Some(Telemetry::default().snapshot());
        input.terminal_operations = vec![SupportTerminalOperation {
            operation_id: "11".repeat(16),
            repository_id: Some("22".repeat(16)),
            kind: SupportOperationKind::RepositoryIndex,
            state: SupportOperationState::Failed,
            stage: SupportOperationStage::Executing,
            revision: 3,
            progress: SupportOperationProgress {
                completed: 5,
                total: 10,
            },
            provider: Some("tree-sitter".to_owned()),
            error: Some(SupportTerminalError {
                code: ErrorCode::ResourceExhausted,
                retryable: false,
                retry_after_ms: None,
                repository_id: Some("22".repeat(16)),
                operation_id: Some("11".repeat(16)),
                generation_id: None,
                details: std::collections::BTreeMap::from([
                    ("limit".to_owned(), SupportDetailValue::Unsigned(1_000_000)),
                    (
                        "provider".to_owned(),
                        SupportDetailValue::Label("tree-sitter".to_owned()),
                    ),
                ]),
                next_actions: vec![
                    SupportNextAction::InspectOperation,
                    SupportNextAction::CollectSupportBundle,
                ],
            }),
        }];
        input.inventory = Some(SupportInventory {
            runtime: SupportRuntimeInventory {
                product_version: "0.1.0".to_owned(),
                binary_name: "rootlight-daemon".to_owned(),
                binary_sha256: Some("aa".repeat(32)),
                feature_profile: vec!["standard".to_owned()],
                protocol_major: 1,
                protocol_minor: 12,
                logical_processors: 8,
                physical_memory_bytes: None,
            },
            dependencies: vec![SupportDependencyInventory {
                name: "sqlite".to_owned(),
                version: "3.51.3".to_owned(),
                sha256: None,
            }],
            adapters: vec![SupportAdapterInventory {
                name: "tree-sitter".to_owned(),
                version: Some("builtin".to_owned()),
                languages: vec!["rust".to_owned()],
                available: true,
                isolated: false,
                binary_sha256: None,
                artifact_sha256: Some("bb".repeat(32)),
            }],
            languages: Vec::new(),
            repositories: vec![SupportRepositoryInventory {
                repository_id: "22".repeat(16),
                root_fingerprint_sha256: None,
                languages: vec!["rust".to_owned()],
                tiers: vec!["structural".to_owned()],
                state: "ready".to_owned(),
                file_count: 12,
                symbol_count: 40,
                relationship_count: 75,
                generation_count: 1,
                storage_bytes: None,
                active_generation_bytes: None,
                predecessor_generation_bytes: None,
                other_retained_generation_bytes: None,
                source_pool_bytes: None,
                shared_source_bytes: None,
                temporary_bytes: None,
                reclaimable_bytes: None,
                repository_overhead_bytes: None,
                inflight_reservation_bytes: None,
                repository_headroom_bytes: None,
            }],
            repository_capacity: None,
            generations: vec![SupportGenerationInventory {
                repository_id: "22".repeat(16),
                generation_id: "33".repeat(16),
                format_version: "1.2".to_owned(),
                checksum_status: SupportChecksumStatus::Verified,
                disk_bytes: 4096,
                state: "active".to_owned(),
            }],
            configuration: SupportConfigurationInventory {
                schema_version: 1,
                connection_limit: 128,
                client_connection_limit: 8,
                control_queue_limit: 64,
                operation_queue_limit: 256,
                client_operation_limit: 32,
                operation_workers: 4,
                request_timeout_ms: 5_000,
                maintenance_interval_ms: 1_000,
                shutdown_grace_ms: 5_000,
            },
            storage: SupportStorageInventory {
                catalog_schema_version: 2,
                generation_format_version: Some("1.2".to_owned()),
                sqlite_version: "3.51.3".to_owned(),
                persistent: true,
                defensive: true,
                foreign_keys: true,
                trusted_schema: false,
                catalog_allocated_bytes: 8192,
                maximum_catalog_bytes: 1024 * 1024,
                maximum_wal_bytes: 256 * 1024,
                maximum_shm_bytes: 64 * 1024,
                generation_disk_bytes: 4096,
                unreclaimed_temporary_bytes: 0,
                disk_margin_bytes: Some(1024 * 1024),
                active_generation_bytes: Some(4096),
                predecessor_generation_bytes: None,
                other_retained_generation_bytes: None,
                source_pool_bytes: None,
                shared_bytes: None,
                pinned_bytes: None,
                reclaimable_bytes: None,
                total_storage_bytes: None,
                catalog_accounted_bytes: None,
                admission_margin_bytes: None,
                effective_retention_generations: None,
                repository_overhead_bytes: None,
                quarantine_bytes: None,
                pinning_supported: None,
                inflight_catalog_reservation_bytes: None,
                inflight_repository_reservation_bytes: None,
                repository_headroom_bytes: None,
                catalog_headroom_bytes: None,
                filesystem_headroom_bytes: None,
                accounting_state: None,
                maximum_repository_storage_bytes: None,
                maximum_durable_catalog_bytes: None,
                minimum_free_disk_bytes: None,
                source_reservation_factor: None,
                oracle_reservation_factor: None,
            },
        });
        input
    }

    fn make_schema_v4_input(input: &mut SupportBundleInput) {
        input.protocol_version = ProtocolVersion::V1_8;
        let storage = &mut input
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .storage;
        storage.active_generation_bytes = None;
        storage.predecessor_generation_bytes = None;
        storage.shared_bytes = None;
        storage.pinned_bytes = None;
        storage.reclaimable_bytes = None;
        storage.total_storage_bytes = None;
        storage.admission_margin_bytes = None;
        storage.effective_retention_generations = None;
    }

    fn make_schema_v6_input(input: &mut SupportBundleInput) {
        input.protocol_version = ProtocolVersion::V1_13;
        let inventory = input
            .inventory
            .as_mut()
            .expect("production inventory exists");
        inventory.runtime.protocol_minor = 13;
        inventory.languages = vec![SupportLanguageCapabilityInventory {
            language: "rust".to_owned(),
            suffixes: vec![".rs".to_owned()],
            aliases: Vec::new(),
            detectors: vec!["extension".to_owned()],
            maximum_tier: "tier_d".to_owned(),
            analyzers: vec!["treesitter".to_owned()],
        }];
    }

    fn make_schema_v7_input(input: &mut SupportBundleInput) {
        make_schema_v6_input(input);
        input.protocol_version = ProtocolVersion::V1_14;
        let inventory = input
            .inventory
            .as_mut()
            .expect("production inventory exists");
        inventory.runtime.protocol_minor = 14;
        let repository = &mut inventory.repositories[0];
        repository.storage_bytes = Some(5_248);
        repository.active_generation_bytes = Some(4_096);
        repository.predecessor_generation_bytes = Some(0);
        repository.other_retained_generation_bytes = Some(0);
        repository.source_pool_bytes = Some(1_024);
        repository.shared_source_bytes = Some(0);
        repository.temporary_bytes = Some(0);
        repository.reclaimable_bytes = Some(0);
        repository.repository_overhead_bytes = Some(128);
        repository.inflight_reservation_bytes = Some(128);
        repository.repository_headroom_bytes = Some(1_043_200);
        let storage = &mut inventory.storage;
        storage.active_generation_bytes = Some(4_096);
        storage.predecessor_generation_bytes = Some(0);
        storage.other_retained_generation_bytes = Some(0);
        storage.source_pool_bytes = Some(1_024);
        storage.shared_bytes = Some(0);
        storage.pinned_bytes = Some(0);
        storage.reclaimable_bytes = Some(0);
        storage.total_storage_bytes = Some(5_248);
        storage.catalog_accounted_bytes = Some(5_248);
        storage.admission_margin_bytes = Some(1_043_200);
        storage.effective_retention_generations = Some(2);
        storage.repository_overhead_bytes = Some(128);
        storage.quarantine_bytes = Some(0);
        storage.pinning_supported = Some(false);
        storage.inflight_catalog_reservation_bytes = Some(256);
        storage.inflight_repository_reservation_bytes = Some(128);
        storage.repository_headroom_bytes = Some(1_043_200);
        storage.catalog_headroom_bytes = Some(2_091_648);
        storage.filesystem_headroom_bytes = Some(2_096_896);
        storage.accounting_state = Some(SupportStorageAccountingState::Reconciled);
        storage.maximum_repository_storage_bytes = Some(1_048_576);
        storage.maximum_durable_catalog_bytes = Some(2_097_152);
        storage.minimum_free_disk_bytes = Some(1_048_576);
        storage.source_reservation_factor = Some(2);
        storage.oracle_reservation_factor = Some(3);
        storage.disk_margin_bytes = Some(3_145_728);
    }

    fn make_schema_v8_input(input: &mut SupportBundleInput) {
        make_schema_v7_input(input);
        input.protocol_version = ProtocolVersion::V1_15;
        let inventory = input
            .inventory
            .as_mut()
            .expect("production inventory exists");
        inventory.runtime.protocol_minor = 15;
        inventory.repository_capacity = Some(SupportRepositoryCapacityInventory {
            registered_repository_count: 2,
            pending_repository_count: 1,
            active_repository_count: 1,
            failed_repository_count: 0,
            pinned_repository_count: 0,
            reclaimable_repository_count: 1,
            pinning_supported: false,
            configured_maximum_repositories: 4_096,
            effective_maximum_repositories: 4_096,
            repository_headroom: 4_094,
        });
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
    fn concurrent_logs_preserve_publication_sequence() {
        const THREADS: usize = 8;
        const RECORDS_PER_THREAD: usize = 256;

        let telemetry = Arc::new(Telemetry::default());
        let barrier = Arc::new(Barrier::new(THREADS));
        let writers = (0..THREADS)
            .map(|_| {
                let telemetry = Arc::clone(&telemetry);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..RECORDS_PER_THREAD {
                        telemetry.record_lifecycle(DaemonLifecycle::Ready);
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("telemetry writer completes");
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.logs.len(), RECENT_LOG_CAPACITY);
        assert!(
            snapshot
                .logs
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(
            snapshot.logs.last().map(|record| record.sequence),
            Some(u64::try_from(THREADS * RECORDS_PER_THREAD).expect("test record count fits u64"))
        );
    }

    #[test]
    fn concurrent_spans_preserve_completion_sequence() {
        const THREADS: usize = 8;
        const SPANS_PER_THREAD: usize = 256;

        let telemetry = Arc::new(Telemetry::default());
        let barrier = Arc::new(Barrier::new(THREADS));
        let writers = (0..THREADS)
            .map(|_| {
                let telemetry = Arc::clone(&telemetry);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..SPANS_PER_THREAD {
                        telemetry
                            .start_span(SpanKind::IpcNegotiation)
                            .finish(TelemetryOutcome::Succeeded, None);
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("telemetry writer completes");
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.traces.len(), RECENT_TRACE_CAPACITY);
        assert!(
            snapshot
                .traces
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert_eq!(
            snapshot.traces.last().map(|span| span.sequence),
            Some(u64::try_from(THREADS * SPANS_PER_THREAD).expect("test span count fits u64"))
        );
    }

    #[test]
    fn full_telemetry_rings_fit_the_support_entry_bound() {
        let telemetry = Arc::new(Telemetry::default());
        for index in 0..=RECENT_LOG_CAPACITY {
            telemetry.record_cancellation_attempt(
                [u8::try_from(index).expect("test index fits u8"); 16],
                CancellationAuditAuthority::InternalResourceLimit,
                CancellationAuditOutcome::Failed,
                Some(ErrorCode::ResourceExhausted),
            );
        }
        for _ in 0..=RECENT_TRACE_CAPACITY {
            telemetry
                .start_span(SpanKind::IpcRequest {
                    method: ControlMethod::QueryAdvanced,
                })
                .finish(
                    TelemetryOutcome::Abandoned,
                    Some(ErrorCode::ResourceExhausted),
                );
        }

        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.logs.len(), RECENT_LOG_CAPACITY);
        assert_eq!(snapshot.traces.len(), RECENT_TRACE_CAPACITY);
        let mut encoded = serde_json::to_vec_pretty(&snapshot).expect("telemetry serializes");
        encoded.push(b'\n');
        assert!(
            encoded.len() <= MAX_TELEMETRY_ENTRY_BYTES,
            "bounded telemetry needs {} bytes, limit is {MAX_TELEMETRY_ENTRY_BYTES}",
            encoded.len()
        );

        let mut input = input();
        input.protocol_version = ProtocolVersion::V1_5;
        input.telemetry = Some(snapshot);
        build_support_bundle_for_schema(&input, SupportBundleSchema::V3)
            .expect("full bounded telemetry builds a support archive");
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
    fn shutdown_phase_spans_retain_source_free_completion_and_abandonment() {
        for phase in [
            ShutdownPhase::ServiceWorkers,
            ShutdownPhase::ControlDrain,
            ShutdownPhase::Journal,
        ] {
            for outcome in [
                TelemetryOutcome::Succeeded,
                TelemetryOutcome::Failed,
                TelemetryOutcome::TimedOut,
                TelemetryOutcome::Abandoned,
            ] {
                let telemetry = Arc::new(Telemetry::default());
                let span = telemetry.start_span(SpanKind::DaemonShutdownPhase { phase });
                if outcome == TelemetryOutcome::Abandoned {
                    drop(span);
                } else {
                    span.finish(outcome, None);
                }
                let snapshot = telemetry.snapshot();
                assert_eq!(snapshot.logs.len(), 1);
                assert_eq!(snapshot.traces.len(), 1);
                let trace = &snapshot.traces[0];
                let record = &snapshot.logs[0];
                assert_eq!(trace.kind, SpanKind::DaemonShutdownPhase { phase });
                assert_eq!(trace.outcome, outcome);
                assert_eq!(record.target, TelemetryTarget::Daemon);
                assert_eq!(
                    record.severity,
                    if outcome == TelemetryOutcome::Succeeded {
                        LogSeverity::Info
                    } else {
                        LogSeverity::Warn
                    }
                );
                assert_eq!(
                    record.event,
                    LogEvent::ShutdownPhaseCompleted {
                        phase,
                        outcome,
                        duration_us: trace.duration_us
                    }
                );
                assert!(record.sequence > trace.sequence);
                let bytes = serde_json::to_vec(record).expect("shutdown record serializes");
                assert!(bytes.len() < MAX_STRUCTURED_LOG_LINE_BYTES);
                assert_eq!(
                    serde_json::from_slice::<StructuredLogRecord>(&bytes)
                        .expect("shutdown record round trips"),
                    *record
                );
                let projected = project_telemetry_v2(&snapshot);
                assert!(projected.logs.is_empty());
                assert!(projected.traces.is_empty());
            }
        }
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
    fn schema_v4_support_archive_is_deterministic_complete_and_source_free() {
        let mut input = production_input();
        make_schema_v4_input(&mut input);
        let mut extended = production_input();
        extended.protocol_version = ProtocolVersion::V1_8;
        assert!(matches!(
            build_support_bundle_for_schema(&extended, SupportBundleSchema::V4),
            Err(SupportBundleError::InvalidInventory)
        ));
        let first = build_support_bundle_for_schema(&input, SupportBundleSchema::V4)
            .expect("schema v4 support bundle builds");
        let second = build_support_bundle_for_schema(&input, SupportBundleSchema::V4)
            .expect("schema v4 support bundle rebuilds");
        assert_eq!(first, second);

        let mut archive =
            zip::ZipArchive::new(Cursor::new(first.archive())).expect("support ZIP opens");
        let names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .expect("entry opens")
                    .name()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, SUPPORT_ENTRY_NAMES_V4);

        let mut operations = Vec::new();
        archive
            .by_name("operations-summary.json")
            .expect("operations entry opens")
            .read_to_end(&mut operations)
            .expect("operations entry reads");
        let operations: SupportOperationsV4 =
            serde_json::from_slice(&operations).expect("operations entry decodes");
        assert_eq!(operations.recent_terminal, input.terminal_operations);
        assert_eq!(
            operations.recent_terminal[0]
                .error
                .as_ref()
                .map(|error| error.code),
            Some(ErrorCode::ResourceExhausted)
        );

        let archive_text = String::from_utf8_lossy(first.archive());
        for forbidden in [
            "PRIVATE_SOURCE_BODY",
            "C:\\Users\\private\\repo",
            "/home/private/repo",
            "sk-secret-token",
        ] {
            assert!(!archive_text.contains(forbidden));
        }

        let mut additive_protocol = input.clone();
        additive_protocol
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .runtime
            .protocol_minor = 9;
        build_support_bundle_for_schema(&additive_protocol, SupportBundleSchema::V4)
            .expect("schema v4 accepts an additive daemon protocol minor");

        let mut obsolete_protocol = input;
        obsolete_protocol
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .runtime
            .protocol_minor = 7;
        assert!(matches!(
            build_support_bundle_for_schema(&obsolete_protocol, SupportBundleSchema::V4),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v5_round_trips_known_storage_accounting_without_inventing_unknowns() {
        let input = production_input();
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V5)
            .expect("schema v5 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut inventory = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut inventory)
            .expect("inventory entry reads");
        let inventory: SupportInventory =
            serde_json::from_slice(&inventory).expect("inventory entry decodes");

        assert_eq!(inventory.storage.active_generation_bytes, Some(4096));
        assert_eq!(inventory.storage.predecessor_generation_bytes, None);
        assert_eq!(inventory.storage.shared_bytes, None);
        assert_eq!(inventory.storage.total_storage_bytes, None);

        let mut legacy = serde_json::to_value(&inventory.storage).expect("storage serializes");
        legacy
            .as_object_mut()
            .expect("storage is an object")
            .remove("active_generation_bytes");
        let legacy: SupportStorageInventory =
            serde_json::from_value(legacy).expect("legacy storage inventory still decodes");
        assert_eq!(legacy.active_generation_bytes, None);
        assert_eq!(legacy.effective_retention_generations, None);

        let mut obsolete_protocol = input;
        obsolete_protocol
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .runtime
            .protocol_minor = 11;
        assert!(matches!(
            build_support_bundle_for_schema(&obsolete_protocol, SupportBundleSchema::V5),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v6_round_trips_bounded_language_capabilities() {
        let mut input = production_input();
        make_schema_v6_input(&mut input);
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V6)
            .expect("schema v6 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut inventory = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut inventory)
            .expect("inventory entry reads");
        let inventory: SupportInventory =
            serde_json::from_slice(&inventory).expect("inventory entry decodes");

        assert_eq!(inventory.languages.len(), 1);
        assert_eq!(inventory.languages[0].language, "rust");
        assert_eq!(inventory.languages[0].suffixes, [".rs"]);
        assert_eq!(inventory.languages[0].maximum_tier, "tier_d");

        let mut missing_languages = input;
        missing_languages
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .languages
            .clear();
        assert!(matches!(
            build_support_bundle_for_schema(&missing_languages, SupportBundleSchema::V6),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v7_reconciles_storage_and_preserves_v6_compatibility() {
        let mut input = production_input();
        make_schema_v7_input(&mut input);
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V7)
            .expect("reconciled schema v7 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut bytes = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut bytes)
            .expect("inventory entry reads");
        let inventory: SupportInventory =
            serde_json::from_slice(&bytes).expect("inventory entry decodes");
        assert_eq!(inventory.storage.total_storage_bytes, Some(5_248));
        assert_eq!(inventory.storage.catalog_accounted_bytes, Some(5_248));
        assert_eq!(inventory.storage.source_pool_bytes, Some(1_024));
        assert_eq!(inventory.storage.shared_bytes, Some(0));
        assert_eq!(
            inventory.storage.accounting_state,
            Some(SupportStorageAccountingState::Reconciled)
        );
        assert_eq!(
            inventory.storage.admission_margin_bytes,
            inventory.storage.repository_headroom_bytes
        );
        assert_eq!(inventory.repository_capacity, None);

        let mut leaked_v7 = input.clone();
        leaked_v7.protocol_version = ProtocolVersion::V1_13;
        leaked_v7
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .runtime
            .protocol_minor = 13;
        assert!(matches!(
            build_support_bundle_for_schema(&leaked_v7, SupportBundleSchema::V6),
            Err(SupportBundleError::InvalidInventory)
        ));
        let mut corrupted = input.clone();
        corrupted
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .storage
            .total_storage_bytes = Some(5_249);
        assert!(matches!(
            build_support_bundle_for_schema(&corrupted, SupportBundleSchema::V7),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut missing_catalog_total = input.clone();
        missing_catalog_total
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .storage
            .catalog_accounted_bytes = None;
        assert!(matches!(
            build_support_bundle_for_schema(&missing_catalog_total, SupportBundleSchema::V7),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut impossible_catalog_total = input;
        impossible_catalog_total
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .storage
            .catalog_accounted_bytes = Some(5_247);
        assert!(matches!(
            build_support_bundle_for_schema(&impossible_catalog_total, SupportBundleSchema::V7),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v8_validates_repository_capacity_and_v7_actions() {
        let mut input = production_input();
        make_schema_v8_input(&mut input);
        let actions = &mut input.terminal_operations[0]
            .error
            .as_mut()
            .expect("failed operation has an error")
            .next_actions;
        actions.push(SupportNextAction::UpdateConfiguration {
            key: "storage.maximum_repositories".to_owned(),
        });
        actions.push(SupportNextAction::DeleteRepository);

        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V8)
            .expect("schema v8 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut bytes = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut bytes)
            .expect("inventory entry reads");
        let inventory: SupportInventory =
            serde_json::from_slice(&bytes).expect("inventory entry decodes");
        assert_eq!(
            inventory
                .repository_capacity
                .expect("capacity aggregate is present")
                .repository_headroom,
            4_094
        );

        let mut leaked_capacity = input.clone();
        leaked_capacity.protocol_version = ProtocolVersion::V1_14;
        let leaked_capacity_inventory = leaked_capacity
            .inventory
            .as_mut()
            .expect("production inventory exists");
        leaked_capacity_inventory.runtime.protocol_minor = 14;
        leaked_capacity.terminal_operations[0]
            .error
            .as_mut()
            .expect("failed operation has an error")
            .next_actions
            .truncate(2);
        assert!(matches!(
            build_support_bundle_for_schema(&leaked_capacity, SupportBundleSchema::V7),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut leaked_v8 = input.clone();
        leaked_v8.protocol_version = ProtocolVersion::V1_14;
        let leaked_inventory = leaked_v8
            .inventory
            .as_mut()
            .expect("production inventory exists");
        leaked_inventory.runtime.protocol_minor = 14;
        leaked_inventory.repository_capacity = None;
        assert!(matches!(
            build_support_bundle_for_schema(&leaked_v8, SupportBundleSchema::V7),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut missing_capacity = input.clone();
        missing_capacity
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repository_capacity = None;
        assert!(matches!(
            build_support_bundle_for_schema(&missing_capacity, SupportBundleSchema::V8),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut inconsistent = input.clone();
        inconsistent
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repository_capacity
            .as_mut()
            .expect("capacity aggregate exists")
            .registered_repository_count = 3;
        assert!(matches!(
            build_support_bundle_for_schema(&inconsistent, SupportBundleSchema::V8),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut invalid_headroom = input.clone();
        invalid_headroom
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repository_capacity
            .as_mut()
            .expect("capacity aggregate exists")
            .repository_headroom = 4_093;
        assert!(matches!(
            build_support_bundle_for_schema(&invalid_headroom, SupportBundleSchema::V8),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut unsupported_pin = input;
        unsupported_pin
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repository_capacity
            .as_mut()
            .expect("capacity aggregate exists")
            .pinned_repository_count = 1;
        assert!(matches!(
            build_support_bundle_for_schema(&unsupported_pin, SupportBundleSchema::V8),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v6_inventory_decodes_without_v7_fields() {
        let mut input = production_input();
        make_schema_v6_input(&mut input);
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V6)
            .expect("schema v6 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut bytes = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut bytes)
            .expect("inventory entry reads");
        let inventory: SupportInventory =
            serde_json::from_slice(&bytes).expect("v6 inventory decodes through current types");
        assert_eq!(inventory.storage.accounting_state, None);
        assert_eq!(inventory.storage.catalog_accounted_bytes, None);
        assert_eq!(inventory.storage.source_pool_bytes, None);
        assert_eq!(inventory.repositories[0].storage_bytes, None);

        input
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .storage
            .catalog_accounted_bytes = Some(0);
        assert!(matches!(
            build_support_bundle_for_schema(&input, SupportBundleSchema::V6),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v4_rejects_path_shaped_inventory_and_unbounded_operations() {
        let mut path_shaped = production_input();
        make_schema_v4_input(&mut path_shaped);
        path_shaped
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repositories[0]
            .state = "C:\\private\\repo".to_owned();
        assert!(matches!(
            build_support_bundle_for_schema(&path_shaped, SupportBundleSchema::V4),
            Err(SupportBundleError::InvalidInventory)
        ));

        let mut unbounded = production_input();
        make_schema_v4_input(&mut unbounded);
        let record = unbounded.terminal_operations[0].clone();
        unbounded
            .terminal_operations
            .resize(MAX_SUPPORT_TERMINAL_OPERATIONS + 1, record);
        assert!(matches!(
            build_support_bundle_for_schema(&unbounded, SupportBundleSchema::V4),
            Err(SupportBundleError::InvalidInventory)
        ));
    }

    #[test]
    fn schema_v4_omits_linkable_repository_root_fingerprints() {
        let mut input = production_input();
        make_schema_v4_input(&mut input);
        let bundle = build_support_bundle_for_schema(&input, SupportBundleSchema::V4)
            .expect("privacy-safe schema v4 support bundle builds");
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bundle.archive())).expect("support ZIP opens");
        let mut inventory = Vec::new();
        archive
            .by_name("inventory.json")
            .expect("inventory entry opens")
            .read_to_end(&mut inventory)
            .expect("inventory entry reads");
        let inventory: serde_json::Value =
            serde_json::from_slice(&inventory).expect("inventory entry decodes");
        assert!(
            inventory["repositories"][0]
                .get("root_fingerprint_sha256")
                .is_none()
        );

        let mut linkable = production_input();
        make_schema_v4_input(&mut linkable);
        linkable
            .inventory
            .as_mut()
            .expect("production inventory exists")
            .repositories[0]
            .root_fingerprint_sha256 = Some("cc".repeat(32));
        assert!(matches!(
            build_support_bundle_for_schema(&linkable, SupportBundleSchema::V4),
            Err(SupportBundleError::InvalidInventory)
        ));
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
    fn cancellation_audit_digests_ids_and_retains_closed_outcomes() {
        let telemetry = Telemetry::default();
        let operation = [0x5a; 16];
        telemetry.record_cancellation_attempt(
            operation,
            CancellationAuditAuthority::Client,
            CancellationAuditOutcome::Denied,
            Some(ErrorCode::PermissionDenied),
        );

        let snapshot = telemetry.snapshot();
        let record = snapshot.logs.first().expect("cancellation audit retained");
        assert_eq!(record.severity, LogSeverity::Warn);
        assert_eq!(record.target, TelemetryTarget::Operation);
        let LogEvent::CancellationAttempt {
            operation_digest,
            authority,
            outcome,
            error_code,
        } = record.event
        else {
            panic!("cancellation audit event is retained");
        };
        assert_ne!(operation_digest, operation);
        assert_eq!(authority, CancellationAuditAuthority::Client);
        assert_eq!(outcome, CancellationAuditOutcome::Denied);
        assert_eq!(error_code, Some(ErrorCode::PermissionDenied));

        let bytes = serde_json::to_vec(record).expect("audit record serializes");
        assert!(bytes.len() < MAX_STRUCTURED_LOG_LINE_BYTES);
        let json = String::from_utf8(bytes).expect("audit record is utf-8 JSON");
        for forbidden in ["owner", "plan_hash", "path", "source", "journal"] {
            assert!(!json.contains(forbidden));
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
