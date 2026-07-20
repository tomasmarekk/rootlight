//! Thin synchronous and asynchronous client for Rootlight's private daemon control protocol.
//!
//! The client validates negotiation, request identifiers, instance nonces, and
//! stable protocol errors before exposing typed control results to applications.
//! Asynchronous calls require a Tokio runtime with time and I/O enabled.

#![forbid(unsafe_code)]

use std::{
    io::{self, Cursor, Read as _, Write as _},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, PublicValue, SafeLabel};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ipc::{
    Endpoint, FrameCodec, IpcError, connect, connect_async, read_response, read_response_async,
    read_server_hello, read_server_hello_async, write_client_hello, write_client_hello_async,
    write_request, write_request_async,
};
use rootlight_observability::{
    CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION, ControlMethod, DURATION_BUCKET_UPPER_US,
    DiagnosticsQuickSnapshot as SupportDiagnosticsQuick, HealthSnapshot as SupportHealth,
    OperationsSummary as SupportOperations, PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION,
    RECENT_LOG_CAPACITY, RECENT_TRACE_CAPACITY, RedactionReport, SUPPORT_BUNDLE_SCHEMA_VERSION,
    SUPPORT_ENTRY_NAMES, SUPPORT_ENTRY_NAMES_V2, SUPPORT_ENTRY_NAMES_V3, SupportBundleInput,
    SupportBundleSchema, SupportManifest, TELEMETRY_SCHEMA_VERSION, TelemetrySnapshot,
    build_support_bundle_for_schema,
};
use rootlight_protocol::{
    CURRENT_PROTOCOL_MINOR, MINIMUM_PROTOCOL_MINOR,
    generated::{common::v1 as common, daemon::v1 as daemon},
};
use rootlight_runtime::{LaunchLock, RuntimePaths};
use sha2::{Digest as _, Sha256};
use tokio::time::Instant as TokioInstant;
use zip::CompressionMethod;

const CLIENT_CAPABILITIES: &[&str] = &[
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
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_IO_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_SUPPORT_ARCHIVE_BYTES: usize = 768 * 1024;
const MAX_SUPPORT_ENTRY_BYTES: usize = 128 * 1024;
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(10);
const START_CHILD_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const START_CHILD_RETAIN_ATTEMPTS: usize = 3;
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

/// Closed status for one daemon subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

/// Closed bounded host resource-pressure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// Cached startup or explicit catalog validation status.
    pub catalog_status: HealthStatus,
    /// Current operation catalog schema version.
    pub catalog_schema_version: u32,
    /// Generation storage status.
    pub generation_status: HealthStatus,
    /// Adapter subsystem status.
    pub adapter_status: HealthStatus,
    /// Watcher subsystem status.
    pub watcher_status: HealthStatus,
    /// Latest bounded host-pressure classification.
    pub resource_pressure: ResourcePressure,
    /// Private local endpoint ownership and publication status.
    pub endpoint_status: HealthStatus,
    /// Current discovery-record schema version.
    pub endpoint_schema_version: u32,
}

/// Closed outcome for one bounded diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DiagnosticResult {
    /// Closed check outcome.
    pub outcome: DiagnosticOutcome,
    /// Monotonic elapsed time rounded to milliseconds.
    pub duration_ms: u32,
    /// Stable source-redacted public failure, when applicable.
    pub error: Option<PublicError>,
}

/// Bounded quick-diagnostics response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DiagnosticsQuick {
    /// Diagnostics schema version.
    pub schema_version: u32,
    /// Aggregate source-free status.
    pub overall_status: HealthStatus,
    /// Current catalog quick-check result.
    pub catalog: DiagnosticResult,
}

/// Validated bounded source-free support archive.
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
    /// Normalized bounded telemetry included by schema v2.
    pub telemetry: Option<TelemetrySnapshot>,
}

/// Client-facing operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Deterministic infrastructure work used to prove control-plane lifecycle.
    ControlProbe,
    /// Repository indexing that may publish an immutable generation.
    RepositoryIndex,
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

/// Selects the active or one immutable repository generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationSelector {
    /// Resolve the active generation at request admission.
    Active,
    /// Query one explicit immutable generation.
    Generation(GenerationId),
}

/// Action applied to one repository-index operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryOperationAction {
    /// Read the latest durable operation state.
    Get,
    /// Request cooperative cancellation.
    Cancel,
}

/// Bounded lexical matching mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocateMode {
    /// Match the complete identifier.
    Exact,
    /// Match an identifier prefix.
    Prefix,
    /// Match normalized lexical text.
    Text,
    /// Match a bounded safe regular expression.
    SafeRegex,
    /// Match a bounded glob.
    Glob,
}

/// Aggregate language-analysis support tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisTier {
    /// Compiler-grade or equivalent evidence.
    TierA,
    /// Build-aware structural evidence.
    TierB,
    /// Parser-backed structural evidence.
    TierC,
    /// Lexical or heuristic evidence.
    TierD,
}

/// Aggregate completeness of one query response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// All admitted inputs were analyzed for the reported scope.
    Complete,
    /// A declared bound excluded part of the scope.
    Bounded,
    /// The result represents a declared sample.
    Sampled,
    /// Parser recovery or unavailable evidence prevents a stronger claim.
    Unknown,
}

/// One immutable generation-bound source selection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceReference {
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    start_byte: u64,
    end_byte: u64,
    content_hash: ContentHash,
    start_line: Option<u64>,
    end_line: Option<u64>,
}

impl SourceReference {
    /// Creates a checked half-open byte selection with an optional inclusive line hint.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidSourceReference`] for an inverted byte
    /// range, a partial line hint, line zero, or an inverted line range.
    pub fn new(
        repository: RepositoryId,
        generation: GenerationId,
        file: FileId,
        bytes: std::ops::Range<u64>,
        content_hash: ContentHash,
        lines: Option<std::ops::RangeInclusive<u64>>,
    ) -> Result<Self, ClientError> {
        let (start_line, end_line) = match lines {
            Some(lines) if *lines.start() > 0 && lines.start() <= lines.end() => {
                (Some(*lines.start()), Some(*lines.end()))
            }
            Some(_) => return Err(ClientError::InvalidSourceReference),
            None => (None, None),
        };
        if bytes.start > bytes.end {
            return Err(ClientError::InvalidSourceReference);
        }
        Ok(Self {
            repository,
            generation,
            file,
            start_byte: bytes.start,
            end_byte: bytes.end,
            content_hash,
            start_line,
            end_line,
        })
    }

    /// Returns the repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the half-open byte range.
    #[must_use]
    pub const fn byte_range(&self) -> std::ops::Range<u64> {
        self.start_byte..self.end_byte
    }

    /// Returns the immutable content digest.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns the optional inclusive line hint.
    #[must_use]
    pub const fn line_range(&self) -> Option<std::ops::RangeInclusive<u64>> {
        match (self.start_line, self.end_line) {
            (Some(start), Some(end)) => Some(start..=end),
            _ => None,
        }
    }
}

/// Measured bounded usage for one first-slice query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct QueryUsage {
    /// Rows examined.
    pub rows: u64,
    /// Edges examined.
    pub edges: u64,
    /// Results returned.
    pub results: u64,
    /// Source bytes returned.
    pub source_bytes: u64,
    /// Encoded JSON bytes measured by the daemon.
    pub json_bytes: u64,
    /// Estimated tokenizer usage.
    pub estimated_tokens: u64,
    /// Monotonic query duration.
    pub elapsed_micros: u64,
}

/// Repository, generation, coverage, and usage correlation for one query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QueryContext {
    /// Repository identity.
    pub repository: RepositoryId,
    /// Resolved immutable generation.
    pub generation: GenerationId,
    /// Parent immutable generation, when present.
    pub parent_generation: Option<GenerationId>,
    /// Whether the resolved generation is currently active.
    pub active_generation: bool,
    /// Weakest relevant analysis tier.
    pub tier: AnalysisTier,
    /// Aggregate result completeness.
    pub coverage_status: CoverageStatus,
    /// Inputs skipped for the relevant scope.
    pub skipped_inputs: u64,
    /// Measured query usage.
    pub usage: QueryUsage,
}

/// Successful repository-index publication.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryIndex {
    /// Repository identity.
    pub repository: RepositoryId,
    /// Durable operation identity.
    pub operation: OperationId,
    /// Terminal operation state.
    pub state: OperationState,
    /// Durable operation revision.
    pub revision: u64,
    /// Previous generation, when present.
    pub parent_generation: Option<GenerationId>,
    /// Newly published immutable generation after successful completion.
    pub published_generation: Option<GenerationId>,
    /// Discovered repository inputs.
    pub discovered_inputs: u64,
    /// Rust source files included in the generation.
    pub indexed_files: u64,
    /// Indexed semantic entities.
    pub entities: u64,
    /// Monotonic indexing duration.
    pub elapsed_micros: u64,
}

/// Durable repository-index operation state and process-local evidence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryOperationStatus {
    /// Durable operation state.
    pub operation: OperationStatus,
    /// Published generation after successful completion.
    pub published_generation: Option<GenerationId>,
    /// Best-effort operation start time.
    pub started_unix_ms: u64,
    /// Peak resident memory measured for this operation.
    pub peak_rss_bytes: u64,
    /// Bytes written for this operation.
    pub written_bytes: u64,
    /// Repository inputs examined.
    pub files_examined: u64,
    /// Bounded polling delay for a nonterminal operation.
    pub retry_after_ms: Option<u32>,
}

/// One generation-pinned lexical result.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocateHit {
    /// Stable symbol identity.
    pub symbol: SymbolId,
    /// Stable file identity.
    pub file: FileId,
    /// Untrusted repository identifier text.
    pub identifier: String,
    /// Untrusted repository-qualified name.
    pub qualified_name: String,
    /// Untrusted repository-relative path.
    pub path: String,
    /// Stable normalized entity kind.
    pub kind: String,
    /// Stable normalized language label.
    pub language: String,
    /// Evidence tier for this hit.
    pub tier: AnalysisTier,
    /// Whether generated-code policy applies.
    pub generated: bool,
    /// Fixed-point relevance score in the range 0 through 1000.
    pub score: u32,
    /// Optional immutable source selection.
    pub source: Option<SourceReference>,
}

impl std::fmt::Debug for LocateHit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocateHit")
            .field("symbol", &self.symbol)
            .field("file", &self.file)
            .field("tier", &self.tier)
            .field("generated", &self.generated)
            .field("score", &self.score)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// Bounded lexical results for one generation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeLocate {
    /// Checked query correlation.
    pub context: QueryContext,
    /// Deterministically ordered hits.
    pub hits: Vec<LocateHit>,
    /// Candidates matching before the result cap.
    pub matched_candidates: u64,
    /// Whether the response was cut by a bound.
    pub truncated: bool,
}

/// One compact generation-pinned symbol explanation.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SymbolExplanation {
    /// Stable symbol identity.
    pub symbol: SymbolId,
    /// Stable normalized entity kind.
    pub kind: String,
    /// Untrusted repository display name.
    pub display_name: String,
    /// Optional untrusted repository signature.
    pub signature: Option<String>,
    /// Immutable definition selection.
    pub definition: SourceReference,
    /// Exact outgoing call edges.
    pub outbound_exact: u64,
    /// Candidate outgoing call edges.
    pub outbound_candidates: u64,
    /// Exact incoming call edges.
    pub inbound_exact: u64,
    /// Candidate incoming call edges.
    pub inbound_candidates: u64,
    /// Exact reference count.
    pub references_exact: u64,
    /// Stable provider label.
    pub provider: String,
    /// Stable evidence label.
    pub evidence: String,
    /// Fixed-point confidence in the range 0 through 1000.
    pub confidence: u32,
}

impl std::fmt::Debug for SymbolExplanation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SymbolExplanation")
            .field("symbol", &self.symbol)
            .field("definition", &self.definition)
            .field("outbound_exact", &self.outbound_exact)
            .field("outbound_candidates", &self.outbound_candidates)
            .field("inbound_exact", &self.inbound_exact)
            .field("inbound_candidates", &self.inbound_candidates)
            .field("references_exact", &self.references_exact)
            .field("confidence", &self.confidence)
            .finish_non_exhaustive()
    }
}

/// Bounded explanations and unresolved identities for one generation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SymbolExplain {
    /// Checked query correlation.
    pub context: QueryContext,
    /// Resolved explanations in request-subsequence order.
    pub symbols: Vec<SymbolExplanation>,
    /// Unresolved identities in request-subsequence order.
    pub unresolved_symbols: Vec<SymbolId>,
    /// Whether the response was cut by a bound.
    pub truncated: bool,
}

/// One verified UTF-8 source chunk.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceChunk {
    /// Exact immutable source selection.
    pub source: SourceReference,
    /// Untrusted repository-relative path.
    pub path: String,
    /// Returned half-open byte range.
    pub start_byte: u64,
    /// Returned half-open byte range end.
    pub end_byte: u64,
    /// Returned first one-based line.
    pub start_line: u64,
    /// Returned final one-based line.
    pub end_line: u64,
    /// Untrusted repository source text.
    pub content: String,
    /// Verified content digest.
    pub content_hash: ContentHash,
    /// Stable normalized language label.
    pub language: String,
    /// Whether generated-code policy applies.
    pub generated: bool,
}

impl std::fmt::Debug for SourceChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceChunk")
            .field("source", &self.source)
            .field("start_byte", &self.start_byte)
            .field("end_byte", &self.end_byte)
            .field("start_line", &self.start_line)
            .field("end_line", &self.end_line)
            .field("content_bytes", &self.content.len())
            .field("content_hash", &self.content_hash)
            .field("language", &self.language)
            .field("generated", &self.generated)
            .finish_non_exhaustive()
    }
}

/// Bounded verified source response.
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceRead {
    /// Checked query correlation.
    pub context: QueryContext,
    /// Verified source chunks in request order.
    pub chunks: Vec<SourceChunk>,
    /// Total returned source bytes.
    pub total_source_bytes: u64,
    /// Whether the response was cut by a bound.
    pub truncated: bool,
}

impl std::fmt::Debug for SourceRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceRead")
            .field("context", &self.context)
            .field("chunk_count", &self.chunks.len())
            .field("total_source_bytes", &self.total_source_bytes)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// One repository entry in the bounded repository list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryListEntry {
    /// Process-local repository identity.
    pub repository_id: RepositoryId,
    /// Active immutable generation for the repository.
    pub active_generation: GenerationId,
    /// Indexed languages.
    pub languages: Vec<String>,
    /// Structural freshness label, such as `current`.
    pub structural_freshness: String,
    /// Semantic freshness label, such as `current`.
    pub semantic_freshness: String,
    /// Repository state label, such as `ready`.
    pub state: String,
}

/// Bounded list of repositories known to the daemon process.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryList {
    /// Known repositories in deterministic order.
    pub repositories: Vec<RepositoryListEntry>,
}

/// One language-scoped coverage entry for a repository generation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryCoverageEntry {
    /// Stable normalized language label.
    pub language: String,
    /// Aggregate analysis tier label, such as `tier_a`.
    pub tier: String,
    /// Aggregate completeness label, such as `complete` or `bounded`.
    pub status: String,
    /// Inputs admitted by deterministic discovery.
    pub discovered_files: u64,
    /// Files committed into normalized IR.
    pub indexed_files: u64,
}

/// One repository's active generation, freshness, and coverage.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryStatus {
    /// Process-local repository identity.
    pub repository_id: RepositoryId,
    /// Active immutable generation for the repository.
    pub active_generation: GenerationId,
    /// Optional predecessor generation.
    pub parent_generation: Option<GenerationId>,
    /// Structural freshness label, such as `current`.
    pub structural_freshness: String,
    /// Semantic freshness label, such as `current`.
    pub semantic_freshness: String,
    /// Repository state label, such as `ready`.
    pub state: String,
    /// Language-scoped coverage entries.
    pub coverage: Vec<RepositoryCoverageEntry>,
}

/// Validated total deadline budget for one asynchronous daemon request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestTimeout(Duration);

impl RequestTimeout {
    /// Smallest caller-level asynchronous request budget.
    pub const MINIMUM: Duration = Duration::from_millis(10);
    /// Largest caller-level asynchronous request budget.
    pub const MAXIMUM: Duration = Duration::from_secs(30);

    /// Validates a caller-level request budget.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidRequestTimeout`] when `duration` is outside
    /// the inclusive 10 millisecond through 30 second protocol range.
    pub fn new(duration: Duration) -> Result<Self, ClientError> {
        if !(Self::MINIMUM..=Self::MAXIMUM).contains(&duration)
            || u32::try_from(duration.as_millis()).is_err()
        {
            return Err(ClientError::InvalidRequestTimeout);
        }
        Ok(Self(duration))
    }

    /// Returns the validated relative request budget.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }
}

impl TryFrom<Duration> for RequestTimeout {
    type Error = ClientError;

    fn try_from(duration: Duration) -> Result<Self, Self::Error> {
        Self::new(duration)
    }
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

/// Exact child-process ownership retained by a lifecycle startup winner.
///
/// The daemon is a process-tree leaf: it starts threads but no descendant
/// processes. Retaining and reaping this handle therefore owns the complete
/// lifecycle. That invariant must move to a process group or job object before
/// the daemon is allowed to spawn descendants. If bounded cleanup cannot prove
/// that the child was reaped, the exact handle is deliberately retained rather
/// than risking PID-based cleanup of an unrelated process.
#[derive(Debug)]
pub struct OwnedDaemon {
    paths: RuntimePaths,
    identity: ReadyDaemonIdentity,
    child: Option<Child>,
}

impl OwnedDaemon {
    /// Requests supervised shutdown and reaps the exact retained child.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when discovery no longer identifies the retained
    /// child, shutdown IO fails, or bounded graceful exit requires forced cleanup.
    pub fn shutdown(mut self) -> Result<(), ClientError> {
        let child = self.child.as_mut().ok_or(ClientError::DaemonLaunchFailed)?;
        let discovery = self.paths.discover().map_err(ClientError::Runtime)?;
        let observed = ReadyDaemonIdentity::from_discovery(&discovery);
        if observed != self.identity || child.id() != self.identity.pid {
            self.terminate_or_retain();
            return Err(ClientError::DaemonLaunchFailed);
        }
        let stdin = child
            .stdin
            .as_mut()
            .ok_or(ClientError::DaemonLaunchFailed)?;
        stdin
            .write_all(b"shutdown\n")
            .map_err(ClientError::LaunchIo)?;
        drop(child.stdin.take());
        let deadline = Instant::now()
            .checked_add(DEFAULT_START_TIMEOUT)
            .ok_or(ClientError::InvalidRequestTimeout)?;
        loop {
            if let Some(status) = child.try_wait().map_err(ClientError::LaunchIo)? {
                self.child = None;
                return if status.success() {
                    Ok(())
                } else {
                    Err(ClientError::DaemonLaunchFailed)
                };
            }
            if Instant::now() >= deadline {
                self.terminate_or_retain();
                return Err(ClientError::DaemonLaunchCleanupTimedOut);
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }

    fn terminate_or_retain(&mut self) {
        if let Some(child) = self.child.take() {
            terminate_or_retain_startup_process(child, || {});
        }
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        self.terminate_or_retain();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupOwnership {
    Detached,
    Retained,
}

#[derive(Debug)]
struct StartupConnection {
    client: Client,
    owned: Option<OwnedDaemon>,
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
        Self::connect_or_start_with_ownership(
            paths,
            client_instance_id,
            policy,
            StartupOwnership::Detached,
        )
        .map(|connection| connection.client)
    }

    /// Resolves a daemon while retaining exact ownership when this call starts it.
    ///
    /// The optional [`OwnedDaemon`] is returned only to the winning startup caller.
    /// It is intended for lifecycle harnesses that must stop and reap the exact
    /// daemon they launched without signaling a discovery PID.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for runtime validation, discovery, launch-lock,
    /// sibling-spawn, timeout, negotiation, or readiness failures.
    pub fn connect_or_start_owned(
        paths: &RuntimePaths,
        client_instance_id: [u8; 16],
        policy: ConnectPolicy,
    ) -> Result<(Self, Option<OwnedDaemon>), ClientError> {
        Self::connect_or_start_with_ownership(
            paths,
            client_instance_id,
            policy,
            StartupOwnership::Retained,
        )
        .map(|connection| (connection.client, connection.owned))
    }

    fn connect_or_start_with_ownership(
        paths: &RuntimePaths,
        client_instance_id: [u8; 16],
        policy: ConnectPolicy,
        ownership: StartupOwnership,
    ) -> Result<StartupConnection, ClientError> {
        match paths.client_directories_absent() {
            Ok(true) => {
                return match policy {
                    ConnectPolicy::ExistingOnly => Err(ClientError::DaemonUnavailable),
                    ConnectPolicy::StartIfMissing => {
                        paths.prepare_owner().map_err(ClientError::Runtime)?;
                        coordinate_start(paths, client_instance_id, ownership)
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
                return coordinate_start(paths, client_instance_id, ownership);
            }
            Err(error) => return Err(error),
        };
        match probe {
            ProbeOutcome::Ready(ready) => {
                return Ok(StartupConnection {
                    client: ready.client,
                    owned: None,
                });
            }
            ProbeOutcome::Unavailable if policy == ConnectPolicy::ExistingOnly => {
                return Err(ClientError::DaemonUnavailable);
            }
            ProbeOutcome::Unavailable => {}
        }
        coordinate_start(paths, client_instance_id, ownership)
    }

    /// Creates a client bound to one discovered daemon and validated client-declared identity.
    #[must_use]
    pub fn new(endpoint: Endpoint, instance_nonce: [u8; 16], client_instance_id: [u8; 16]) -> Self {
        // Keep this infallible constructor bounded even if its static tuning ever
        // drifts outside `FrameCodec`'s validated limits.
        let codec =
            FrameCodec::new(rootlight_ipc::MAX_FRAME_BYTES, REQUEST_IO_TIMEOUT).unwrap_or_default();
        Self {
            endpoint,
            instance_nonce,
            client_instance_id,
            codec,
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
        let (response, selected_protocol_minor) = self.request_with_protocol(
            daemon::request_envelope::Request::Health(daemon::HealthRequest {}),
        )?;
        match response {
            daemon::response_envelope::Response::Health(health) => {
                parse_health(health, selected_protocol_minor)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Runs bounded source-free quick diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for unavailable protocol support or malformed results.
    pub fn diagnostics_quick(&self) -> Result<DiagnosticsQuick, ClientError> {
        match self.request(daemon::request_envelope::Request::DiagnosticsQuick(
            daemon::DiagnosticsQuickRequest {},
        ))? {
            daemon::response_envelope::Response::DiagnosticsQuick(response) => {
                parse_diagnostics_quick(response)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Builds one bounded source-free support archive.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for unavailable protocol support, malformed bounds,
    /// a digest mismatch, or a response that claims to contain source.
    pub fn support_bundle(&self) -> Result<SupportBundle, ClientError> {
        let (response, selected_protocol_minor) = self.request_with_protocol(
            daemon::request_envelope::Request::SupportBundle(daemon::SupportBundleRequest {}),
        )?;
        match response {
            daemon::response_envelope::Response::SupportBundle(response) => {
                parse_support_bundle(response, selected_protocol_minor)
            }
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

    /// Submits work attached to this validated client-declared identity lease.
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

    /// Legacy compatibility stub for the unsupported P1 lease-renewal operation.
    ///
    /// This method never contacts the daemon. A nonzero expiry deterministically
    /// returns an `UnsupportedCapability` public error.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::InvalidOperationLease`] for a zero expiry and
    /// [`ClientError::Public`] with `UnsupportedCapability` otherwise.
    pub fn operation_renew_lease(
        &self,
        operation: OperationId,
        lease_expires_unix_ms: u64,
    ) -> Result<OperationStatus, ClientError> {
        if lease_expires_unix_ms == 0 {
            return Err(ClientError::InvalidOperationLease);
        }
        let error = lease_renewal_unsupported(operation)?;
        Err(ClientError::Public(Box::new(error)))
    }

    fn submit_operation_request(
        &self,
        request: daemon::OperationSubmitRequest,
    ) -> Result<OperationStatus, ClientError> {
        let operation = parse_operation(request.operation.clone())?;
        match self.request(daemon::request_envelope::Request::OperationSubmit(request))? {
            daemon::response_envelope::Response::OperationSubmit(response) => {
                parse_expected_operation_status(response.operation, operation)
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
                parse_expected_operation_status(response.operation, operation)
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
                parse_expected_operation_status(response.operation, operation)?,
            )),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Indexes one bounded repository root and publishes one immutable generation.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid request bounds, unavailable protocol
    /// support, transport failure, or a malformed or uncorrelated response.
    pub fn repository_index(
        &self,
        root: &str,
        operation: OperationId,
        detached: bool,
    ) -> Result<RepositoryIndex, ClientError> {
        match self.request(build_repository_index_request(root, operation, detached)?)? {
            daemon::response_envelope::Response::RepositoryIndex(response) => {
                parse_repository_index(response, operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously indexes one bounded repository root within a total request budget.
    ///
    /// Dropping the returned future closes its one-request stream, allowing the
    /// daemon to cancel work attached to that connection.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid request bounds, unavailable protocol
    /// support, transport failure, timeout, or a malformed or uncorrelated response.
    pub async fn repository_index_async(
        &self,
        root: &str,
        operation: OperationId,
        detached: bool,
        timeout: RequestTimeout,
    ) -> Result<RepositoryIndex, ClientError> {
        match self
            .request_async(
                build_repository_index_request(root, operation, detached)?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::RepositoryIndex(response) => {
                parse_repository_index(response, operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reads or cooperatively cancels one repository-index operation.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid wait bounds, unavailable protocol
    /// support, transport failure, or a malformed or uncorrelated response.
    pub fn repository_operation_status(
        &self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
    ) -> Result<RepositoryOperationStatus, ClientError> {
        match self.request(build_repository_operation_status_request(
            operation,
            action,
            wait_ms,
            after_revision,
        )?)? {
            daemon::response_envelope::Response::RepositoryOperationStatus(response) => {
                parse_repository_operation_status(response, operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously reads or cancels one repository-index operation.
    ///
    /// Dropping the returned future closes its one-request stream. The independent
    /// `wait_ms` long-poll bound remains part of the daemon request, while `timeout`
    /// bounds the complete client exchange.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid wait bounds, unavailable protocol
    /// support, transport failure, timeout, or a malformed or uncorrelated response.
    pub async fn repository_operation_status_async(
        &self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> Result<RepositoryOperationStatus, ClientError> {
        match self
            .request_async(
                build_repository_operation_status_request(
                    operation,
                    action,
                    wait_ms,
                    after_revision,
                )?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::RepositoryOperationStatus(response) => {
                parse_repository_operation_status(response, operation)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Executes one bounded generation-pinned lexical lookup.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid query bounds, unavailable protocol
    /// support, transport failure, or a malformed or uncorrelated response.
    pub fn code_locate(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: &str,
        mode: LocateMode,
        maximum_results: u32,
    ) -> Result<CodeLocate, ClientError> {
        match self.request(build_code_locate_request(
            repository,
            generation,
            query,
            mode,
            maximum_results,
        )?)? {
            daemon::response_envelope::Response::CodeLocate(response) => {
                parse_code_locate(response, repository, generation, maximum_results)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously executes one bounded generation-pinned lexical lookup.
    ///
    /// Dropping the returned future closes its one-request stream, allowing the
    /// daemon to cancel the attached lookup.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid query bounds, unavailable protocol
    /// support, transport failure, timeout, or a malformed or uncorrelated response.
    pub async fn code_locate_async(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: &str,
        mode: LocateMode,
        maximum_results: u32,
        timeout: RequestTimeout,
    ) -> Result<CodeLocate, ClientError> {
        match self
            .request_async(
                build_code_locate_request(repository, generation, query, mode, maximum_results)?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::CodeLocate(response) => {
                parse_code_locate(response, repository, generation, maximum_results)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Explains a bounded set of generation-pinned symbols.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid symbol bounds, unavailable protocol
    /// support, transport failure, or a malformed or uncorrelated response.
    pub fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: &[SymbolId],
    ) -> Result<SymbolExplain, ClientError> {
        match self.request(build_symbol_explain_request(
            repository, generation, symbols,
        )?)? {
            daemon::response_envelope::Response::SymbolExplain(response) => {
                parse_symbol_explain(response, repository, generation, symbols)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously explains a bounded set of generation-pinned symbols.
    ///
    /// Dropping the returned future closes its one-request stream, allowing the
    /// daemon to cancel the attached explanation.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid symbol bounds, unavailable protocol
    /// support, transport failure, timeout, or a malformed or uncorrelated response.
    pub async fn symbol_explain_async(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: &[SymbolId],
        timeout: RequestTimeout,
    ) -> Result<SymbolExplain, ClientError> {
        match self
            .request_async(
                build_symbol_explain_request(repository, generation, symbols)?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::SymbolExplain(response) => {
                parse_symbol_explain(response, repository, generation, symbols)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reads a bounded ordered set of immutable source selections.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid reference bounds or correlation,
    /// unavailable protocol support, transport failure, or a malformed response.
    pub fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
    ) -> Result<SourceRead, ClientError> {
        match self.request(build_source_read_request(
            repository, generation, references,
        )?)? {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_read(response, repository, generation, references)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously reads a bounded ordered set of immutable source selections.
    ///
    /// Dropping the returned future closes its one-request stream, allowing the
    /// daemon to cancel the attached source read.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid reference bounds or correlation,
    /// unavailable protocol support, transport failure, timeout, or a malformed response.
    pub async fn source_read_async(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &[SourceReference],
        timeout: RequestTimeout,
    ) -> Result<SourceRead, ClientError> {
        match self
            .request_async(
                build_source_read_request(repository, generation, references)?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::SourceRead(response) => {
                parse_source_read(response, repository, generation, references)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Lists repositories known to this daemon process.
    ///
    /// The optional `max_results` bounds the returned list; the optional
    /// `query` is accepted for forward compatibility but repositories are
    /// opaque process-local identities, so it does not filter today.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid list bounds, unavailable protocol
    /// support, transport failure, or a malformed or uncorrelated response.
    pub fn repository_list(
        &self,
        max_results: Option<u32>,
        query: Option<&str>,
    ) -> Result<RepositoryList, ClientError> {
        match self.request(build_repository_list_request(max_results, query)?)? {
            daemon::response_envelope::Response::RepositoryList(response) => {
                parse_repository_list(response, max_results)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously lists repositories known to this daemon process.
    ///
    /// Dropping the returned future closes its one-request stream.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for invalid list bounds, unavailable protocol
    /// support, transport failure, timeout, or a malformed or uncorrelated
    /// response.
    pub async fn repository_list_async(
        &self,
        max_results: Option<u32>,
        query: Option<&str>,
        timeout: RequestTimeout,
    ) -> Result<RepositoryList, ClientError> {
        match self
            .request_async(build_repository_list_request(max_results, query)?, timeout)
            .await?
        {
            daemon::response_envelope::Response::RepositoryList(response) => {
                parse_repository_list(response, max_results)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Reads the active generation status of one repository.
    ///
    /// The response always reports the repository's active generation; the
    /// selector is validated but does not change which generation is reported.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for unavailable protocol support, transport
    /// failure, or a malformed or uncorrelated response.
    pub fn repository_status(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
    ) -> Result<RepositoryStatus, ClientError> {
        match self.request(build_repository_status_request(repository, generation)?)? {
            daemon::response_envelope::Response::RepositoryStatus(response) => {
                parse_repository_status(response, repository)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Asynchronously reads the active generation status of one repository.
    ///
    /// Dropping the returned future closes its one-request stream.
    ///
    /// # Panics
    ///
    /// Panics if polled without Tokio's time or I/O drivers enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] for unavailable protocol support, transport
    /// failure, timeout, or a malformed or uncorrelated response.
    pub async fn repository_status_async(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        timeout: RequestTimeout,
    ) -> Result<RepositoryStatus, ClientError> {
        match self
            .request_async(
                build_repository_status_request(repository, generation)?,
                timeout,
            )
            .await?
        {
            daemon::response_envelope::Response::RepositoryStatus(response) => {
                parse_repository_status(response, repository)
            }
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn request(
        &self,
        request: daemon::request_envelope::Request,
    ) -> Result<daemon::response_envelope::Response, ClientError> {
        self.request_with_protocol(request)
            .map(|(response, _)| response)
    }

    fn request_with_protocol(
        &self,
        request: daemon::request_envelope::Request,
    ) -> Result<(daemon::response_envelope::Response, u32), ClientError> {
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
        match correlated_response(response, request_id)? {
            daemon::response_envelope::Response::Error(error) => {
                Err(ClientError::Public(Box::new(parse_public_error(error)?)))
            }
            response => Ok((response, selected_protocol_minor)),
        }
    }

    async fn request_async(
        &self,
        request: daemon::request_envelope::Request,
        timeout: RequestTimeout,
    ) -> Result<daemon::response_envelope::Response, ClientError> {
        let deadline = TokioInstant::now()
            .checked_add(timeout.duration())
            .ok_or(ClientError::InvalidRequestTimeout)?;
        let codec = FrameCodec::new(rootlight_ipc::MAX_FRAME_BYTES, timeout.duration())?;
        match tokio::time::timeout_at(deadline, self.request_async_until(request, deadline, codec))
            .await
        {
            Ok(response) => finish_async_request(deadline, TokioInstant::now(), response),
            Err(_) => Err(ClientError::RequestTimedOut),
        }
    }

    async fn request_async_until(
        &self,
        request: daemon::request_envelope::Request,
        deadline: TokioInstant,
        codec: FrameCodec,
    ) -> Result<daemon::response_envelope::Response, ClientError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        if request_id == 0 {
            return Err(ClientError::RequestIdExhausted);
        }

        // The unsplit stream stays owned by this future so cancellation by drop
        // closes the daemon peer instead of leaving detached transport work.
        let mut stream = connect_async(&self.endpoint).await?;
        write_client_hello_async(
            codec,
            &mut stream,
            &client_hello(self.instance_nonce, self.client_instance_id),
        )
        .await?;
        let hello = read_server_hello_async(codec, &mut stream).await?;
        let selected_protocol_minor = validate_server_hello(&hello, self.instance_nonce)?;
        ensure_request_supported(&request, selected_protocol_minor)?;
        let timeout_ms = remaining_timeout_ms(deadline, TokioInstant::now())?;
        write_request_async(
            codec,
            &mut stream,
            &daemon::RequestEnvelope {
                request_id,
                instance_nonce: self.instance_nonce.to_vec(),
                timeout_ms: Some(timeout_ms),
                request: Some(request),
            },
        )
        .await?;
        let response = read_response_async(codec, &mut stream).await?;
        match correlated_response(response, request_id)? {
            daemon::response_envelope::Response::Error(error) => {
                Err(ClientError::Public(Box::new(parse_public_error(error)?)))
            }
            response => Ok(response),
        }
    }
}

fn correlated_response(
    response: daemon::ResponseEnvelope,
    request_id: u64,
) -> Result<daemon::response_envelope::Response, ClientError> {
    if response.request_id != request_id {
        return Err(ClientError::MismatchedRequestId);
    }
    response.response.ok_or(ClientError::MissingResponse)
}

fn coordinate_start(
    paths: &RuntimePaths,
    client_instance_id: [u8; 16],
    ownership: StartupOwnership,
) -> Result<StartupConnection, ClientError> {
    let deadline = startup_deadline()?;
    loop {
        match paths.acquire_launch_lock() {
            Ok(launch) => {
                let probe = probe_ready_client(paths, client_instance_id);
                if let Ok(ProbeOutcome::Ready(ready)) = probe {
                    return Ok(StartupConnection {
                        client: ready.client,
                        owned: None,
                    });
                }
                if let Err(error) = probe
                    && !startup_probe_retryable(&error)
                {
                    return Err(error);
                }
                let startup = CoordinatedStartup::spawn(launch, ownership, paths)?;
                return wait_for_ready_daemon(paths, client_instance_id, deadline, startup);
            }
            Err(rootlight_runtime::RuntimeError::LaunchBusy) => {
                if let ProbeOutcome::Ready(ready) = probe_ready_client(paths, client_instance_id)? {
                    return Ok(StartupConnection {
                        client: ready.client,
                        owned: None,
                    });
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
    mut startup: CoordinatedStartup,
) -> Result<StartupConnection, ClientError> {
    loop {
        if startup.child_exited()? {
            return Err(ClientError::DaemonLaunchFailed);
        }
        let probe = probe_ready_client(paths, client_instance_id);
        if let Ok(ProbeOutcome::Ready(ready)) = probe {
            if startup.matches_running(ready.identity)? {
                return startup.finish(paths.clone(), ready);
            }
            startup.terminate()?;
            return Ok(StartupConnection {
                client: ready.client,
                owned: None,
            });
        }
        if let Err(error) = probe
            && !startup_probe_retryable(&error)
        {
            startup.terminate()?;
            return Err(error);
        }
        if Instant::now() >= deadline {
            startup.terminate()?;
            return Err(ClientError::DaemonStartTimedOut);
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }
}

#[derive(Debug)]
struct CoordinatedStartup {
    authority: Option<LaunchLock>,
    ownership: StartupOwnership,
    child: Option<Child>,
}

impl CoordinatedStartup {
    fn spawn(
        authority: LaunchLock,
        ownership: StartupOwnership,
        paths: &RuntimePaths,
    ) -> Result<Self, ClientError> {
        let child = spawn_coordinated_daemon(ownership, paths)?;
        Ok(Self {
            authority: Some(authority),
            ownership,
            child: Some(child),
        })
    }

    fn matches_running(&mut self, identity: ReadyDaemonIdentity) -> Result<bool, ClientError> {
        // Recheck after the authenticated readiness probe: if the child exited
        // during that probe, Windows may already be free to reuse its PID.
        if self.child_exited()? {
            return Ok(false);
        }
        Ok(self
            .child
            .as_ref()
            .is_some_and(|child| child.id() == identity.pid))
    }

    fn finish(
        mut self,
        paths: RuntimePaths,
        ready: ReadyDaemon,
    ) -> Result<StartupConnection, ClientError> {
        let child = self.child.take().ok_or(ClientError::DaemonLaunchFailed)?;
        let owned = match self.ownership {
            StartupOwnership::Detached => {
                drop(child);
                None
            }
            StartupOwnership::Retained => Some(OwnedDaemon {
                paths,
                identity: ready.identity,
                child: Some(child),
            }),
        };
        Ok(StartupConnection {
            client: ready.client,
            owned,
        })
    }

    fn child_exited(&mut self) -> Result<bool, ClientError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(true);
        };
        if child.try_wait().map_err(ClientError::LaunchIo)?.is_none() {
            return Ok(false);
        }
        self.child = None;
        Ok(true)
    }

    fn terminate(&mut self) -> Result<(), ClientError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        terminate_startup_child(child)?;
        self.child = None;
        Ok(())
    }
}

impl Drop for CoordinatedStartup {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        let authority = self.authority.take();
        terminate_or_retain_startup_process(child, move || {
            if let Some(authority) = authority {
                std::mem::forget(authority);
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupCleanup {
    Reaped,
    Retained,
}

fn terminate_or_retain_startup_process(
    process: impl StartupProcess,
    retain_authority: impl FnOnce(),
) -> StartupCleanup {
    terminate_or_retain_startup_process_with(
        process,
        START_CHILD_RETAIN_ATTEMPTS,
        || std::thread::sleep(START_POLL_INTERVAL),
        retain_authority,
    )
}

fn terminate_or_retain_startup_process_with(
    mut process: impl StartupProcess,
    max_attempts: usize,
    mut pause: impl FnMut(),
    retain_authority: impl FnOnce(),
) -> StartupCleanup {
    for attempt in 0..max_attempts {
        if matches!(process.try_exited(), Ok(true)) {
            return StartupCleanup::Reaped;
        }
        let _ = process.terminate();
        if matches!(process.try_exited(), Ok(true)) {
            return StartupCleanup::Reaped;
        }
        if attempt.saturating_add(1) < max_attempts {
            pause();
        }
    }
    // INTENTIONAL: losing the handles would release startup authority without
    // proof that the exact child exited. Bounded retention is safer than PID reuse.
    std::mem::forget(process);
    retain_authority();
    StartupCleanup::Retained
}

fn terminate_startup_child(child: &mut Child) -> Result<(), ClientError> {
    let deadline = Instant::now()
        .checked_add(START_CHILD_STOP_TIMEOUT)
        .ok_or(ClientError::InvalidRequestTimeout)?;
    terminate_startup_process_until(child, || Instant::now() >= deadline)
}

trait StartupProcess {
    fn try_exited(&mut self) -> io::Result<bool>;
    fn terminate(&mut self) -> io::Result<()>;
}

impl StartupProcess for Child {
    fn try_exited(&mut self) -> io::Result<bool> {
        self.try_wait().map(|status| status.is_some())
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.kill()
    }
}

fn terminate_startup_process_until(
    process: &mut impl StartupProcess,
    mut deadline_expired: impl FnMut() -> bool,
) -> Result<(), ClientError> {
    if process.try_exited().map_err(ClientError::LaunchIo)? {
        return Ok(());
    }
    if let Err(source) = process.terminate() {
        if process.try_exited().map_err(ClientError::LaunchIo)? {
            return Ok(());
        }
        return Err(ClientError::LaunchIo(source));
    }
    loop {
        if process.try_exited().map_err(ClientError::LaunchIo)? {
            return Ok(());
        }
        if deadline_expired() {
            return Err(ClientError::DaemonLaunchCleanupTimedOut);
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }
}

#[derive(Debug)]
enum ProbeOutcome {
    Ready(ReadyDaemon),
    Unavailable,
}

#[derive(Debug)]
struct ReadyDaemon {
    client: Client,
    identity: ReadyDaemonIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyDaemonIdentity {
    pid: u32,
    instance_nonce: [u8; 16],
}

impl ReadyDaemonIdentity {
    fn from_discovery(discovery: &rootlight_runtime::DiscoveryRecord) -> Self {
        Self {
            pid: discovery.pid(),
            instance_nonce: discovery.instance_nonce(),
        }
    }
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
    classify_health_probe(
        client,
        ReadyDaemonIdentity::from_discovery(&discovery),
        health,
    )
}

fn classify_health_probe(
    client: Client,
    identity: ReadyDaemonIdentity,
    health: Result<Health, ClientError>,
) -> Result<ProbeOutcome, ClientError> {
    match health {
        Ok(health) if health.ready && health.lifecycle == DaemonLifecycle::Ready => {
            Ok(ProbeOutcome::Ready(ReadyDaemon { client, identity }))
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

fn spawn_coordinated_daemon(
    ownership: StartupOwnership,
    paths: &RuntimePaths,
) -> Result<Child, ClientError> {
    let executable = std::env::current_exe().map_err(ClientError::LaunchIo)?;
    let directory = executable
        .parent()
        .ok_or(ClientError::DaemonExecutableMissing)?;
    let daemon = directory.join(sibling_daemon_name());
    if !daemon.is_file() {
        return Err(ClientError::DaemonExecutableMissing);
    }
    let mut command = Command::new(daemon);
    let (argument, stdin) = match ownership {
        StartupOwnership::Detached => ("--coordinated-start", Stdio::null()),
        StartupOwnership::Retained => ("--coordinated-stdio", Stdio::piped()),
    };
    command
        .arg(argument)
        .env("ROOTLIGHT_STATE_DIR", paths.state_dir())
        .env("ROOTLIGHT_RUNTIME_DIR", paths.runtime_dir())
        .stdin(stdin)
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
    let required_minor = match request {
        daemon::request_envelope::Request::RepositoryIndex(_)
        | daemon::request_envelope::Request::RepositoryOperationStatus(_)
        | daemon::request_envelope::Request::CodeLocate(_)
        | daemon::request_envelope::Request::SymbolExplain(_)
        | daemon::request_envelope::Request::SourceRead(_)
        | daemon::request_envelope::Request::RepositoryList(_)
        | daemon::request_envelope::Request::RepositoryStatus(_) => 5,
        daemon::request_envelope::Request::DiagnosticsQuick(_)
        | daemon::request_envelope::Request::SupportBundle(_) => 3,
        daemon::request_envelope::Request::OperationLeaseRenew(_) => {
            return Err(ClientError::ProtocolFeatureUnavailable);
        }
        daemon::request_envelope::Request::OperationSubmit(request)
            if request.deadline_unix_ms.is_some()
                || request.lease_expires_unix_ms.is_some()
                || !request.detached =>
        {
            2
        }
        daemon::request_envelope::Request::Health(_)
        | daemon::request_envelope::Request::OperationStatus(_)
        | daemon::request_envelope::Request::OperationCancel(_)
        | daemon::request_envelope::Request::OperationSubmit(_) => 1,
    };
    if selected_protocol_minor < required_minor {
        Err(ClientError::ProtocolFeatureUnavailable)
    } else {
        Ok(())
    }
}

fn parse_health(
    health: daemon::HealthResponse,
    selected_protocol_minor: u32,
) -> Result<Health, ClientError> {
    let legacy = selected_protocol_minor < 3;
    Ok(Health {
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
        catalog_status: parse_health_status(
            health.catalog_status,
            legacy.then_some(if health.journal_healthy {
                HealthStatus::Healthy
            } else {
                HealthStatus::Failed
            }),
        )?,
        catalog_schema_version: health.catalog_schema_version,
        generation_status: parse_health_status(
            health.generation_status,
            legacy.then_some(HealthStatus::NotConfigured),
        )?,
        adapter_status: parse_health_status(
            health.adapter_status,
            legacy.then_some(HealthStatus::NotConfigured),
        )?,
        watcher_status: parse_health_status(
            health.watcher_status,
            legacy.then_some(HealthStatus::NotConfigured),
        )?,
        resource_pressure: parse_resource_pressure(
            health.resource_pressure,
            legacy.then_some(ResourcePressure::Unknown),
        )?,
        endpoint_status: parse_health_status(
            health.endpoint_status,
            legacy.then_some(HealthStatus::Unavailable),
        )?,
        endpoint_schema_version: health.endpoint_schema_version,
    })
}

fn parse_health_status(
    value: i32,
    legacy_default: Option<HealthStatus>,
) -> Result<HealthStatus, ClientError> {
    match daemon::HealthStatus::try_from(value).map_err(|_| ClientError::InvalidHealthStatus)? {
        daemon::HealthStatus::Healthy => Ok(HealthStatus::Healthy),
        daemon::HealthStatus::Degraded => Ok(HealthStatus::Degraded),
        daemon::HealthStatus::Unavailable => Ok(HealthStatus::Unavailable),
        daemon::HealthStatus::NotConfigured => Ok(HealthStatus::NotConfigured),
        daemon::HealthStatus::Failed => Ok(HealthStatus::Failed),
        daemon::HealthStatus::Unspecified => legacy_default.ok_or(ClientError::InvalidHealthStatus),
    }
}

fn parse_resource_pressure(
    value: i32,
    legacy_default: Option<ResourcePressure>,
) -> Result<ResourcePressure, ClientError> {
    match daemon::ResourcePressure::try_from(value)
        .map_err(|_| ClientError::InvalidResourcePressure)?
    {
        daemon::ResourcePressure::Normal => Ok(ResourcePressure::Normal),
        daemon::ResourcePressure::Elevated => Ok(ResourcePressure::Elevated),
        daemon::ResourcePressure::High => Ok(ResourcePressure::High),
        daemon::ResourcePressure::Critical => Ok(ResourcePressure::Critical),
        daemon::ResourcePressure::Unknown => Ok(ResourcePressure::Unknown),
        daemon::ResourcePressure::Unspecified => {
            legacy_default.ok_or(ClientError::InvalidResourcePressure)
        }
    }
}

fn parse_diagnostics_quick(
    response: daemon::DiagnosticsQuickResponse,
) -> Result<DiagnosticsQuick, ClientError> {
    if response.schema_version != 1 || response.results.len() != 1 {
        return Err(ClientError::InvalidDiagnostics);
    }
    let result = response
        .results
        .into_iter()
        .next()
        .ok_or(ClientError::InvalidDiagnostics)?;
    if daemon::DiagnosticCheck::try_from(result.check)
        .map_err(|_| ClientError::InvalidDiagnostics)?
        != daemon::DiagnosticCheck::CatalogQuickCheck
    {
        return Err(ClientError::InvalidDiagnostics);
    }
    let outcome = match daemon::DiagnosticOutcome::try_from(result.outcome)
        .map_err(|_| ClientError::InvalidDiagnostics)?
    {
        daemon::DiagnosticOutcome::Passed => DiagnosticOutcome::Passed,
        daemon::DiagnosticOutcome::Failed => DiagnosticOutcome::Failed,
        daemon::DiagnosticOutcome::TimedOut => DiagnosticOutcome::TimedOut,
        daemon::DiagnosticOutcome::Unavailable => DiagnosticOutcome::Unavailable,
        daemon::DiagnosticOutcome::Unspecified => return Err(ClientError::InvalidDiagnostics),
    };
    Ok(DiagnosticsQuick {
        schema_version: response.schema_version,
        overall_status: parse_health_status(response.overall_status, None)?,
        catalog: DiagnosticResult {
            outcome,
            duration_ms: result.duration_ms,
            error: result.error.map(parse_public_error).transpose()?,
        },
    })
}

fn parse_support_bundle(
    response: daemon::SupportBundleResponse,
    selected_protocol_minor: u32,
) -> Result<SupportBundle, ClientError> {
    let expected_schema = if selected_protocol_minor >= 5 {
        CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION
    } else if selected_protocol_minor >= 4 {
        PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION
    } else {
        SUPPORT_BUNDLE_SCHEMA_VERSION
    };
    if response.schema_version != expected_schema
        || response.contains_source
        || response.archive.len() > MAX_SUPPORT_ARCHIVE_BYTES
        || response.archive_bytes
            != u64::try_from(response.archive.len())
                .map_err(|_| ClientError::InvalidSupportBundle)?
    {
        return Err(ClientError::InvalidSupportBundle);
    }
    let sha256: [u8; 32] = response
        .sha256
        .try_into()
        .map_err(|_| ClientError::InvalidSupportBundle)?;
    if <[u8; 32]>::from(Sha256::digest(&response.archive)) != sha256 {
        return Err(ClientError::InvalidSupportBundle);
    }
    let telemetry = validate_support_archive(&response.archive, response.schema_version)?;
    Ok(SupportBundle {
        schema_version: response.schema_version,
        archive: response.archive,
        sha256,
        archive_bytes: response.archive_bytes,
        contains_source: false,
        telemetry,
    })
}

fn validate_support_archive(
    archive: &[u8],
    schema_version: u32,
) -> Result<Option<TelemetrySnapshot>, ClientError> {
    let expected_names: &[&str] = match schema_version {
        SUPPORT_BUNDLE_SCHEMA_VERSION => &SUPPORT_ENTRY_NAMES,
        PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION => &SUPPORT_ENTRY_NAMES_V2,
        CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION => &SUPPORT_ENTRY_NAMES_V3,
        _ => return Err(ClientError::InvalidSupportBundle),
    };
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|_| ClientError::InvalidSupportBundle)?;
    if zip.len() != expected_names.len() {
        return Err(ClientError::InvalidSupportBundle);
    }
    let maximum =
        u64::try_from(MAX_SUPPORT_ENTRY_BYTES).map_err(|_| ClientError::InvalidSupportBundle)?;
    let mut entries = std::collections::BTreeMap::new();
    for (index, expected_name) in expected_names.iter().copied().enumerate() {
        let mut entry = zip
            .by_index(index)
            .map_err(|_| ClientError::InvalidSupportBundle)?;
        if entry.name() != expected_name
            || entry.is_dir()
            || entry.compression() != CompressionMethod::Stored
            || entry.size() > maximum
        {
            return Err(ClientError::InvalidSupportBundle);
        }
        let mut bounded = entry.by_ref().take(maximum.saturating_add(1));
        let mut contents = Vec::new();
        bounded
            .read_to_end(&mut contents)
            .map_err(|_| ClientError::InvalidSupportBundle)?;
        if contents.len() > MAX_SUPPORT_ENTRY_BYTES {
            return Err(ClientError::InvalidSupportBundle);
        }
        entries.insert(expected_name, contents);
    }
    let diagnostics: SupportDiagnosticsQuick =
        decode_support_entry(&entries, "diagnostics/quick.json")?;
    let health: SupportHealth = decode_support_entry(&entries, "health.json")?;
    let operations: SupportOperations = decode_support_entry(&entries, "operations-summary.json")?;
    let manifest: SupportManifest = decode_support_entry(&entries, "manifest.json")?;
    let redaction: RedactionReport = decode_support_entry(&entries, "redaction-report.json")?;
    let telemetry = if schema_version != SUPPORT_BUNDLE_SCHEMA_VERSION {
        Some(decode_support_entry(&entries, "telemetry.json")?)
    } else {
        None
    };
    validate_support_semantics(
        &entries,
        &diagnostics,
        &health,
        &operations,
        &manifest,
        &redaction,
        telemetry.as_ref(),
    )?;
    let canonical = build_support_bundle_for_schema(
        &SupportBundleInput {
            protocol_version: manifest.protocol_version,
            operating_system: manifest.operating_system,
            architecture: manifest.architecture,
            health,
            diagnostics,
            operations,
            telemetry: telemetry.clone(),
        },
        if schema_version == SUPPORT_BUNDLE_SCHEMA_VERSION {
            SupportBundleSchema::V1
        } else if schema_version == PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION {
            SupportBundleSchema::V2
        } else {
            SupportBundleSchema::V3
        },
    )
    .map_err(|_| ClientError::InvalidSupportBundle)?;
    if canonical.archive() != archive {
        return Err(ClientError::InvalidSupportBundle);
    }
    Ok(telemetry)
}

fn decode_support_entry<T: serde::de::DeserializeOwned>(
    entries: &std::collections::BTreeMap<&str, Vec<u8>>,
    name: &str,
) -> Result<T, ClientError> {
    serde_json::from_slice(entries.get(name).ok_or(ClientError::InvalidSupportBundle)?)
        .map_err(|_| ClientError::InvalidSupportBundle)
}

fn validate_support_semantics(
    entries: &std::collections::BTreeMap<&str, Vec<u8>>,
    diagnostics: &SupportDiagnosticsQuick,
    health: &SupportHealth,
    operations: &SupportOperations,
    manifest: &SupportManifest,
    redaction: &RedactionReport,
    telemetry: Option<&TelemetrySnapshot>,
) -> Result<(), ClientError> {
    let schema_version = manifest.schema_version;
    let expected_omissions = if schema_version == SUPPORT_BUNDLE_SCHEMA_VERSION {
        rootlight_observability::OMITTED_DATA_CLASSES.as_slice()
    } else if matches!(
        schema_version,
        PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION | CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION
    ) {
        rootlight_observability::OMITTED_DATA_CLASSES_V2.as_slice()
    } else {
        return Err(ClientError::InvalidSupportBundle);
    };
    if diagnostics.schema_version != SUPPORT_BUNDLE_SCHEMA_VERSION
        || health.catalog_schema_version == 0
        || health.endpoint_schema_version == 0
        || operations
            .queued
            .checked_add(operations.running)
            .and_then(|count| count.checked_add(operations.cancelling))
            .is_none()
        || manifest.contains_source
        || redaction.schema_version != schema_version
        || redaction.contains_source
        || redaction.omitted_data_classes
            != expected_omissions
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        || (schema_version == SUPPORT_BUNDLE_SCHEMA_VERSION && telemetry.is_some())
        || (schema_version != SUPPORT_BUNDLE_SCHEMA_VERSION && telemetry.is_none())
    {
        return Err(ClientError::InvalidSupportBundle);
    }
    if let Some(telemetry) = telemetry {
        validate_telemetry_snapshot(telemetry, schema_version)?;
    }
    let expected_manifest_names: &[&str] = if schema_version == SUPPORT_BUNDLE_SCHEMA_VERSION {
        &[
            "diagnostics/quick.json",
            "health.json",
            "operations-summary.json",
            "redaction-report.json",
        ]
    } else {
        &[
            "diagnostics/quick.json",
            "health.json",
            "operations-summary.json",
            "redaction-report.json",
            "telemetry.json",
        ]
    };
    if manifest.entries.len() != expected_manifest_names.len() {
        return Err(ClientError::InvalidSupportBundle);
    }
    for (record, expected_name) in manifest
        .entries
        .iter()
        .zip(expected_manifest_names.iter().copied())
    {
        let bytes = entries
            .get(expected_name)
            .ok_or(ClientError::InvalidSupportBundle)?;
        if record.name != expected_name
            || record.bytes
                != u64::try_from(bytes.len()).map_err(|_| ClientError::InvalidSupportBundle)?
            || record.sha256 != hex_sha256(bytes)?
        {
            return Err(ClientError::InvalidSupportBundle);
        }
    }
    Ok(())
}

fn validate_telemetry_snapshot(
    telemetry: &TelemetrySnapshot,
    support_schema_version: u32,
) -> Result<(), ClientError> {
    let log_capacity =
        u32::try_from(RECENT_LOG_CAPACITY).map_err(|_| ClientError::InvalidSupportBundle)?;
    let trace_capacity =
        u32::try_from(RECENT_TRACE_CAPACITY).map_err(|_| ClientError::InvalidSupportBundle)?;
    if telemetry.schema_version != TELEMETRY_SCHEMA_VERSION
        || telemetry.log_capacity != log_capacity
        || telemetry.trace_capacity != trace_capacity
        || telemetry.logs.len() > RECENT_LOG_CAPACITY
        || telemetry.traces.len() > RECENT_TRACE_CAPACITY
        || telemetry.metrics.schema_version != TELEMETRY_SCHEMA_VERSION
        || telemetry.metrics.ipc_requests.len()
            != expected_control_methods(support_schema_version).len()
        || !sequences_increase(&telemetry.logs, |record| record.sequence)
        || !sequences_increase(&telemetry.traces, |span| span.sequence)
    {
        return Err(ClientError::InvalidSupportBundle);
    }
    for (metric, method) in telemetry.metrics.ipc_requests.iter().zip(
        expected_control_methods(support_schema_version)
            .iter()
            .copied(),
    ) {
        let bucket_total = metric
            .duration_us
            .bucket_counts
            .iter()
            .try_fold(0_u64, |total, count| total.checked_add(*count))
            .ok_or(ClientError::InvalidSupportBundle)?;
        let outcome_total = [
            metric.succeeded_total,
            metric.rejected_total,
            metric.timed_out_total,
            metric.cancelled_total,
            metric.failed_total,
            metric.abandoned_total,
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| total.checked_add(count))
        .ok_or(ClientError::InvalidSupportBundle)?;
        if metric.method != method
            || metric.duration_us.upper_bounds_us != DURATION_BUCKET_UPPER_US
            || bucket_total != metric.duration_us.count
            || outcome_total != metric.duration_us.count
        {
            return Err(ClientError::InvalidSupportBundle);
        }
    }
    Ok(())
}

fn expected_control_methods(schema_version: u32) -> &'static [ControlMethod] {
    const V2_METHODS: &[ControlMethod] = &[
        ControlMethod::Unknown,
        ControlMethod::Health,
        ControlMethod::DiagnosticsQuick,
        ControlMethod::SupportBundle,
        ControlMethod::OperationSubmit,
        ControlMethod::OperationStatus,
        ControlMethod::OperationCancel,
        ControlMethod::OperationLeaseRenew,
    ];
    if schema_version == PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION {
        V2_METHODS
    } else {
        &ControlMethod::ALL
    }
}

fn sequences_increase<T>(records: &[T], sequence: impl Fn(&T) -> u64) -> bool {
    records
        .windows(2)
        .all(|pair| sequence(&pair[0]) < sequence(&pair[1]))
}

fn hex_sha256(bytes: &[u8]) -> Result<String, ClientError> {
    use std::fmt::Write as _;

    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|_| ClientError::InvalidSupportBundle)?;
    }
    Ok(encoded)
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
    let operation = parse_operation(status.operation)?;
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
        daemon::OperationKind::RepositoryIndex => OperationKind::RepositoryIndex,
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
    let error = status.error.map(parse_public_error).transpose()?;
    if (state == OperationState::Failed) != error.is_some()
        || error
            .as_ref()
            .and_then(PublicError::operation)
            .is_some_and(|error_operation| error_operation != operation)
        || (state == OperationState::Interrupted)
            == (recovery_class == RecoveryClass::NotApplicable)
        || status.deadline_unix_ms == Some(0)
        || status.lease_expires_unix_ms == Some(0)
        || status.detached == status.lease_expires_unix_ms.is_some()
        || (status.total_units != 0 && status.completed_units > status.total_units)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let plan_hash: [u8; 32] = status
        .plan_hash
        .try_into()
        .map_err(|_| ClientError::InvalidPlanHash)?;
    Ok(OperationStatus {
        operation,
        state,
        revision: status.revision,
        completed_units: status.completed_units,
        total_units: status.total_units,
        error,
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

fn parse_expected_operation_status(
    status: Option<daemon::OperationStatus>,
    expected: OperationId,
) -> Result<OperationStatus, ClientError> {
    let status = parse_operation_status(status)?;
    if status.operation != expected {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(status)
}

const fn first_slice_schema() -> common::ContractVersion {
    common::ContractVersion { major: 1, minor: 0 }
}

fn require_first_slice_response_schema(
    schema: Option<common::ContractVersion>,
) -> Result<(), ClientError> {
    if schema == Some(first_slice_schema()) {
        Ok(())
    } else {
        Err(ClientError::InvalidResponseSchema)
    }
}

fn build_repository_index_request(
    root: &str,
    operation: OperationId,
    detached: bool,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if root.is_empty() || root.len() > 4096 || root.as_bytes().contains(&0) {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(daemon::request_envelope::Request::RepositoryIndex(
        daemon::RepositoryIndexRequest {
            schema_version: Some(first_slice_schema()),
            root: root.to_owned(),
            operation: Some(operation_to_wire(operation)),
            detached,
        },
    ))
}

fn build_repository_operation_status_request(
    operation: OperationId,
    action: RepositoryOperationAction,
    wait_ms: Option<u32>,
    after_revision: Option<u64>,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if wait_ms.is_some_and(|wait| wait > 30_000) {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(
        daemon::request_envelope::Request::RepositoryOperationStatus(
            daemon::RepositoryOperationStatusRequest {
                schema_version: Some(first_slice_schema()),
                operation: Some(operation_to_wire(operation)),
                action: match action {
                    RepositoryOperationAction::Get => {
                        daemon::RepositoryOperationAction::RepositoryOperationGet as i32
                    }
                    RepositoryOperationAction::Cancel => {
                        daemon::RepositoryOperationAction::RepositoryOperationCancel as i32
                    }
                },
                wait_ms,
                after_revision,
            },
        ),
    )
}

fn build_code_locate_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    query: &str,
    mode: LocateMode,
    maximum_results: u32,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if query.is_empty() || query.len() > 2048 || !(1..=200).contains(&maximum_results) {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(daemon::request_envelope::Request::CodeLocate(
        daemon::CodeLocateRequest {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(repository)),
            generation: Some(generation_selector_to_wire(generation)),
            query: query.to_owned(),
            mode: locate_mode_to_wire(mode) as i32,
            maximum_results,
        },
    ))
}

fn build_symbol_explain_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    symbols: &[SymbolId],
) -> Result<daemon::request_envelope::Request, ClientError> {
    if symbols.is_empty()
        || symbols.len() > 16
        || symbols
            .iter()
            .enumerate()
            .any(|(index, symbol)| symbols[..index].contains(symbol))
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(daemon::request_envelope::Request::SymbolExplain(
        daemon::SymbolExplainRequest {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(repository)),
            generation: Some(generation_selector_to_wire(generation)),
            symbols: symbols.iter().copied().map(symbol_to_wire).collect(),
        },
    ))
}

fn build_source_read_request(
    repository: RepositoryId,
    generation: GenerationSelector,
    references: &[SourceReference],
) -> Result<daemon::request_envelope::Request, ClientError> {
    if references.is_empty()
        || references.len() > 32
        || references.iter().enumerate().any(|(index, reference)| {
            reference.repository != repository
                || matches!(
                    generation,
                    GenerationSelector::Generation(selected)
                        if reference.generation != selected
                )
                || references[..index].contains(reference)
        })
        || references
            .windows(2)
            .any(|pair| pair[0].generation != pair[1].generation)
    {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(daemon::request_envelope::Request::SourceRead(
        daemon::SourceReadRequest {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(repository)),
            generation: Some(generation_selector_to_wire(generation)),
            references: references.iter().map(source_reference_to_wire).collect(),
        },
    ))
}

fn build_repository_list_request(
    max_results: Option<u32>,
    query: Option<&str>,
) -> Result<daemon::request_envelope::Request, ClientError> {
    if max_results.is_some_and(|max| !(1..=1000).contains(&max)) {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    if query.is_some_and(|query| query.len() > 2048 || query.as_bytes().contains(&0)) {
        return Err(ClientError::InvalidFirstSliceRequest);
    }
    Ok(daemon::request_envelope::Request::RepositoryList(
        daemon::RepositoryListRequest {
            max_results,
            query: query.map(str::to_owned),
        },
    ))
}

fn build_repository_status_request(
    repository: RepositoryId,
    generation: GenerationSelector,
) -> Result<daemon::request_envelope::Request, ClientError> {
    Ok(daemon::request_envelope::Request::RepositoryStatus(
        daemon::RepositoryStatusRequest {
            repository: Some(repository_to_wire(repository)),
            generation: Some(generation_selector_to_wire(generation)),
        },
    ))
}

fn repository_to_wire(repository: RepositoryId) -> common::RepositoryId {
    common::RepositoryId {
        value: repository.as_bytes().to_vec(),
    }
}

fn generation_to_wire(generation: GenerationId) -> common::GenerationId {
    common::GenerationId {
        value: generation.as_bytes().to_vec(),
    }
}

fn symbol_to_wire(symbol: SymbolId) -> common::SymbolId {
    common::SymbolId {
        value: symbol.as_bytes().to_vec(),
    }
}

fn file_to_wire(file: FileId) -> common::FileId {
    common::FileId {
        value: file.as_bytes().to_vec(),
    }
}

fn content_hash_to_wire(content_hash: ContentHash) -> common::ContentHash {
    common::ContentHash {
        value: content_hash.as_bytes().to_vec(),
    }
}

fn generation_selector_to_wire(selector: GenerationSelector) -> daemon::GenerationSelector {
    daemon::GenerationSelector {
        selector: Some(match selector {
            GenerationSelector::Active => daemon::generation_selector::Selector::Active(true),
            GenerationSelector::Generation(generation) => {
                daemon::generation_selector::Selector::Generation(generation_to_wire(generation))
            }
        }),
    }
}

const fn locate_mode_to_wire(mode: LocateMode) -> daemon::FirstSliceLocateMode {
    match mode {
        LocateMode::Exact => daemon::FirstSliceLocateMode::FirstSliceLocateExact,
        LocateMode::Prefix => daemon::FirstSliceLocateMode::FirstSliceLocatePrefix,
        LocateMode::Text => daemon::FirstSliceLocateMode::FirstSliceLocateText,
        LocateMode::SafeRegex => daemon::FirstSliceLocateMode::FirstSliceLocateSafeRegex,
        LocateMode::Glob => daemon::FirstSliceLocateMode::FirstSliceLocateGlob,
    }
}

fn source_reference_to_wire(reference: &SourceReference) -> daemon::FirstSliceSourceRef {
    daemon::FirstSliceSourceRef {
        repository: Some(repository_to_wire(reference.repository)),
        generation: Some(generation_to_wire(reference.generation)),
        file: Some(file_to_wire(reference.file)),
        start_byte: reference.start_byte,
        end_byte: reference.end_byte,
        content_hash: Some(content_hash_to_wire(reference.content_hash)),
        start_line: reference.start_line,
        end_line: reference.end_line,
    }
}

fn parse_repository_index(
    response: daemon::RepositoryIndexResponse,
    expected_operation: OperationId,
) -> Result<RepositoryIndex, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let operation = parse_operation(response.operation)?;
    let state = parse_operation_state(response.state)?;
    let published_generation = response
        .published_generation
        .map(parse_generation)
        .transpose()?;
    let parent_generation = response
        .parent_generation
        .map(parse_generation)
        .transpose()?;
    if operation != expected_operation
        || response.indexed_files > response.discovered_inputs
        || parent_generation.is_some() && parent_generation == published_generation
        || match state {
            OperationState::Succeeded => published_generation.is_none(),
            OperationState::Queued
            | OperationState::Running
            | OperationState::Cancelling
            | OperationState::Failed
            | OperationState::Interrupted
            | OperationState::Cancelled => published_generation.is_some(),
        }
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(RepositoryIndex {
        repository: parse_repository(
            response
                .repository
                .ok_or(ClientError::InvalidResponseCorrelation)?,
        )?,
        operation,
        state,
        revision: response.revision,
        parent_generation,
        published_generation,
        discovered_inputs: response.discovered_inputs,
        indexed_files: response.indexed_files,
        entities: response.entities,
        elapsed_micros: response.elapsed_micros,
    })
}

fn parse_repository_operation_status(
    response: daemon::RepositoryOperationStatusResponse,
    expected_operation: OperationId,
) -> Result<RepositoryOperationStatus, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let operation = parse_expected_operation_status(response.operation, expected_operation)?;
    if operation.kind != OperationKind::RepositoryIndex {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let published_generation = response
        .published_generation
        .map(parse_generation)
        .transpose()?;
    let coherent = match operation.state {
        OperationState::Queued | OperationState::Running | OperationState::Cancelling => {
            published_generation.is_none()
        }
        OperationState::Succeeded => {
            published_generation.is_some() && response.retry_after_ms.is_none()
        }
        OperationState::Failed | OperationState::Interrupted | OperationState::Cancelled => {
            published_generation.is_none() && response.retry_after_ms.is_none()
        }
    };
    if !coherent {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(RepositoryOperationStatus {
        operation,
        published_generation,
        started_unix_ms: response.started_unix_ms,
        peak_rss_bytes: response.peak_rss_bytes,
        written_bytes: response.written_bytes,
        files_examined: response.files_examined,
        retry_after_ms: response.retry_after_ms,
    })
}

fn parse_repository_list(
    response: daemon::RepositoryListResponse,
    max_results: Option<u32>,
) -> Result<RepositoryList, ClientError> {
    let mut repositories = Vec::with_capacity(response.repositories.len());
    for entry in response.repositories {
        let repository_id =
            parse_repository(entry.repository.ok_or(ClientError::InvalidIdentifier)?)?;
        let active_generation = parse_generation(
            entry
                .active_generation
                .ok_or(ClientError::InvalidIdentifier)?,
        )?;
        repositories.push(RepositoryListEntry {
            repository_id,
            active_generation,
            languages: entry.languages,
            structural_freshness: entry.structural_freshness,
            semantic_freshness: entry.semantic_freshness,
            state: entry.state,
        });
    }
    // The daemon bounds the list; a longer reply is a correlation failure.
    if let Some(max) = max_results
        && repositories.len() > usize::try_from(max).unwrap_or(usize::MAX)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(RepositoryList { repositories })
}

fn parse_repository_status(
    response: daemon::RepositoryStatusResponse,
    expected_repository: RepositoryId,
) -> Result<RepositoryStatus, ClientError> {
    let repository_id =
        parse_repository(response.repository.ok_or(ClientError::InvalidIdentifier)?)?;
    if repository_id != expected_repository {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let active_generation = parse_generation(
        response
            .active_generation
            .ok_or(ClientError::InvalidIdentifier)?,
    )?;
    let parent_generation = response
        .parent_generation
        .map(parse_generation)
        .transpose()?;
    let coverage = response
        .coverage
        .into_iter()
        .map(|entry| RepositoryCoverageEntry {
            language: entry.language,
            tier: entry.tier,
            status: entry.status,
            discovered_files: entry.discovered_files,
            indexed_files: entry.indexed_files,
        })
        .collect();
    Ok(RepositoryStatus {
        repository_id,
        active_generation,
        parent_generation,
        structural_freshness: response.structural_freshness,
        semantic_freshness: response.semantic_freshness,
        state: response.state,
        coverage,
    })
}

fn parse_code_locate(
    response: daemon::CodeLocateResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    maximum_results: u32,
) -> Result<CodeLocate, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let context = parse_query_context(response.context, repository, selector)?;
    let returned_results =
        u64::try_from(response.hits.len()).map_err(|_| ClientError::InvalidResponseCorrelation)?;
    if response.hits.len()
        > usize::try_from(maximum_results).map_err(|_| ClientError::InvalidResponseCorrelation)?
        || response.matched_candidates < returned_results
        || !response.truncated && response.matched_candidates != returned_results
        || context.usage.results < returned_results
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut hits = Vec::new();
    hits.try_reserve_exact(response.hits.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for hit in response.hits {
        let file = parse_file(hit.file)?;
        let source = hit
            .source
            .map(|source| parse_source_reference(source, &context))
            .transpose()?;
        if hit.score > 1_000 || source.as_ref().is_some_and(|source| source.file != file) {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        hits.push(LocateHit {
            symbol: parse_symbol(hit.symbol)?,
            file,
            identifier: hit.identifier,
            qualified_name: hit.qualified_name,
            path: hit.path,
            kind: hit.kind,
            language: hit.language,
            tier: parse_analysis_tier(hit.tier)?,
            generated: hit.generated,
            score: hit.score,
            source,
        });
    }
    Ok(CodeLocate {
        context,
        hits,
        matched_candidates: response.matched_candidates,
        truncated: response.truncated,
    })
}

fn parse_symbol_explain(
    response: daemon::SymbolExplainResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    requested: &[SymbolId],
) -> Result<SymbolExplain, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let context = parse_query_context(response.context, repository, selector)?;
    let total = response
        .symbols
        .len()
        .checked_add(response.unresolved_symbols.len())
        .ok_or(ClientError::InvalidResponseCorrelation)?;
    if total > requested.len() || (!response.truncated && total != requested.len()) {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut symbols = Vec::new();
    symbols
        .try_reserve_exact(response.symbols.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for explanation in response.symbols {
        let symbol = parse_symbol(explanation.symbol)?;
        let definition = parse_source_reference(
            explanation
                .definition
                .ok_or(ClientError::InvalidResponseCorrelation)?,
            &context,
        )?;
        if explanation.confidence > 1_000
            || symbols
                .iter()
                .any(|prior: &SymbolExplanation| prior.symbol == symbol)
            || !requested.contains(&symbol)
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        symbols.push(SymbolExplanation {
            symbol,
            kind: explanation.kind,
            display_name: explanation.display_name,
            signature: explanation.signature,
            definition,
            outbound_exact: explanation.outbound_exact,
            outbound_candidates: explanation.outbound_candidates,
            inbound_exact: explanation.inbound_exact,
            inbound_candidates: explanation.inbound_candidates,
            references_exact: explanation.references_exact,
            provider: explanation.provider,
            evidence: explanation.evidence,
            confidence: explanation.confidence,
        });
    }
    let mut unresolved_symbols = Vec::new();
    unresolved_symbols
        .try_reserve_exact(response.unresolved_symbols.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    for unresolved in response.unresolved_symbols {
        let unresolved = parse_symbol(Some(unresolved))?;
        if !requested.contains(&unresolved)
            || unresolved_symbols.contains(&unresolved)
            || symbols.iter().any(|resolved| resolved.symbol == unresolved)
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        unresolved_symbols.push(unresolved);
    }
    if !ids_form_subsequence(symbols.iter().map(|item| item.symbol), requested)
        || !ids_form_subsequence(unresolved_symbols.iter().copied(), requested)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(SymbolExplain {
        context,
        symbols,
        unresolved_symbols,
        truncated: response.truncated,
    })
}

fn parse_source_read(
    response: daemon::SourceReadResponse,
    repository: RepositoryId,
    selector: GenerationSelector,
    requested: &[SourceReference],
) -> Result<SourceRead, ClientError> {
    require_first_slice_response_schema(response.schema_version)?;
    let context = parse_query_context(response.context, repository, selector)?;
    if context.generation != requested[0].generation
        || response.chunks.len() > requested.len()
        || (!response.truncated && response.chunks.len() != requested.len())
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(response.chunks.len())
        .map_err(|_| ClientError::ResponseAllocationFailed)?;
    let mut source_bytes = 0_u64;
    for (chunk, expected) in response.chunks.into_iter().zip(requested) {
        let source = parse_source_reference(
            chunk
                .source
                .ok_or(ClientError::InvalidResponseCorrelation)?,
            &context,
        )?;
        let content_hash = parse_content_hash(chunk.content_hash)?;
        let length = chunk
            .end_byte
            .checked_sub(chunk.start_byte)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        if &source != expected
            || chunk.start_byte > source.start_byte
            || chunk.end_byte < source.end_byte
            || chunk.start_line == 0
            || chunk.start_line > chunk.end_line
            || source
                .start_line
                .is_some_and(|line| chunk.start_line > line)
            || source.end_line.is_some_and(|line| chunk.end_line < line)
            || content_hash != source.content_hash
            || u64::try_from(chunk.content.len())
                .map_err(|_| ClientError::InvalidResponseCorrelation)?
                != length
        {
            return Err(ClientError::InvalidResponseCorrelation);
        }
        source_bytes = source_bytes
            .checked_add(length)
            .ok_or(ClientError::InvalidResponseCorrelation)?;
        chunks.push(SourceChunk {
            source,
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.content,
            content_hash,
            language: chunk.language,
            generated: chunk.generated,
        });
    }
    if source_bytes != response.total_source_bytes
        || source_bytes != context.usage.source_bytes
        || context.usage.results
            != u64::try_from(chunks.len()).map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(SourceRead {
        context,
        chunks,
        total_source_bytes: response.total_source_bytes,
        truncated: response.truncated,
    })
}

fn parse_query_context(
    context: Option<daemon::FirstSliceQueryContext>,
    repository: RepositoryId,
    selector: GenerationSelector,
) -> Result<QueryContext, ClientError> {
    let context = context.ok_or(ClientError::InvalidResponseCorrelation)?;
    let observed_repository = parse_repository(
        context
            .repository
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    let generation = parse_generation(
        context
            .generation
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    let parent_generation = context
        .parent_generation
        .map(parse_generation)
        .transpose()?;
    let selector_matches = match selector {
        GenerationSelector::Active => context.active_generation,
        GenerationSelector::Generation(selected) => generation == selected,
    };
    if observed_repository != repository
        || !selector_matches
        || parent_generation == Some(generation)
    {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    Ok(QueryContext {
        repository,
        generation,
        parent_generation,
        active_generation: context.active_generation,
        tier: parse_analysis_tier(context.tier)?,
        coverage_status: parse_coverage_status(context.coverage_status)?,
        skipped_inputs: context.skipped_inputs,
        usage: parse_query_usage(context.usage)?,
    })
}

fn parse_query_usage(
    usage: Option<daemon::FirstSliceQueryUsage>,
) -> Result<QueryUsage, ClientError> {
    let usage = usage.ok_or(ClientError::InvalidResponseCorrelation)?;
    Ok(QueryUsage {
        rows: usage.rows,
        edges: usage.edges,
        results: usage.results,
        source_bytes: usage.source_bytes,
        json_bytes: usage.json_bytes,
        estimated_tokens: usage.estimated_tokens,
        elapsed_micros: usage.elapsed_micros,
    })
}

fn parse_source_reference(
    reference: daemon::FirstSliceSourceRef,
    context: &QueryContext,
) -> Result<SourceReference, ClientError> {
    let repository = parse_repository(
        reference
            .repository
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    let generation = parse_generation(
        reference
            .generation
            .ok_or(ClientError::InvalidResponseCorrelation)?,
    )?;
    if repository != context.repository || generation != context.generation {
        return Err(ClientError::InvalidResponseCorrelation);
    }
    SourceReference::new(
        repository,
        generation,
        parse_file(reference.file)?,
        reference.start_byte..reference.end_byte,
        parse_content_hash(reference.content_hash)?,
        match (reference.start_line, reference.end_line) {
            (None, None) => None,
            (Some(start), Some(end)) => Some(start..=end),
            _ => return Err(ClientError::InvalidResponseCorrelation),
        },
    )
    .map_err(|_| ClientError::InvalidResponseCorrelation)
}

fn parse_analysis_tier(value: i32) -> Result<AnalysisTier, ClientError> {
    match daemon::FirstSliceAnalysisTier::try_from(value)
        .map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        daemon::FirstSliceAnalysisTier::FirstSliceTierA => Ok(AnalysisTier::TierA),
        daemon::FirstSliceAnalysisTier::FirstSliceTierB => Ok(AnalysisTier::TierB),
        daemon::FirstSliceAnalysisTier::FirstSliceTierC => Ok(AnalysisTier::TierC),
        daemon::FirstSliceAnalysisTier::FirstSliceTierD => Ok(AnalysisTier::TierD),
        daemon::FirstSliceAnalysisTier::Unspecified => Err(ClientError::InvalidResponseCorrelation),
    }
}

fn parse_coverage_status(value: i32) -> Result<CoverageStatus, ClientError> {
    match daemon::FirstSliceCoverageStatus::try_from(value)
        .map_err(|_| ClientError::InvalidResponseCorrelation)?
    {
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete => {
            Ok(CoverageStatus::Complete)
        }
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageBounded => Ok(CoverageStatus::Bounded),
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageSampled => Ok(CoverageStatus::Sampled),
        daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown => Ok(CoverageStatus::Unknown),
        daemon::FirstSliceCoverageStatus::Unspecified => {
            Err(ClientError::InvalidResponseCorrelation)
        }
    }
}

fn parse_operation_state(value: i32) -> Result<OperationState, ClientError> {
    match daemon::OperationState::try_from(value).map_err(|_| ClientError::InvalidOperationState)? {
        daemon::OperationState::Queued => Ok(OperationState::Queued),
        daemon::OperationState::Running => Ok(OperationState::Running),
        daemon::OperationState::Cancelling => Ok(OperationState::Cancelling),
        daemon::OperationState::Succeeded => Ok(OperationState::Succeeded),
        daemon::OperationState::Failed => Ok(OperationState::Failed),
        daemon::OperationState::Interrupted => Ok(OperationState::Interrupted),
        daemon::OperationState::Cancelled => Ok(OperationState::Cancelled),
        daemon::OperationState::Unspecified => Err(ClientError::InvalidOperationState),
    }
}

fn parse_file(file: Option<common::FileId>) -> Result<FileId, ClientError> {
    let bytes: [u8; 20] = file
        .ok_or(ClientError::InvalidResponseCorrelation)?
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(FileId::from_bytes(bytes))
}

fn parse_symbol(symbol: Option<common::SymbolId>) -> Result<SymbolId, ClientError> {
    let bytes: [u8; 20] = symbol
        .ok_or(ClientError::InvalidResponseCorrelation)?
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(SymbolId::from_bytes(bytes))
}

fn parse_content_hash(
    content_hash: Option<common::ContentHash>,
) -> Result<ContentHash, ClientError> {
    let bytes: [u8; 32] = content_hash
        .ok_or(ClientError::InvalidResponseCorrelation)?
        .value
        .try_into()
        .map_err(|_| ClientError::InvalidIdentifier)?;
    Ok(ContentHash::from_bytes(bytes))
}

fn ids_form_subsequence(
    candidates: impl IntoIterator<Item = SymbolId>,
    requested: &[SymbolId],
) -> bool {
    let mut start = 0;
    for candidate in candidates {
        let Some(offset) = requested[start..]
            .iter()
            .position(|requested| *requested == candidate)
        else {
            return false;
        };
        start += offset + 1;
    }
    true
}

fn lease_renewal_unsupported(operation: OperationId) -> Result<PublicError, ClientError> {
    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "operation lease renewal is unsupported",
    )
    .operation(operation)
    .build()
    .map_err(|_| ClientError::InvalidPublicError)
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

fn finish_async_request<T>(
    deadline: TokioInstant,
    observed_at: TokioInstant,
    response: Result<T, ClientError>,
) -> Result<T, ClientError> {
    if observed_at >= deadline {
        return Err(ClientError::RequestTimedOut);
    }
    response
}

fn remaining_timeout_ms(
    deadline: TokioInstant,
    observed_at: TokioInstant,
) -> Result<u32, ClientError> {
    let remaining = deadline
        .checked_duration_since(observed_at)
        .ok_or(ClientError::RequestTimedOut)?;
    let rounded_milliseconds = remaining
        .as_millis()
        .checked_add(u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
        .ok_or(ClientError::InvalidRequestTimeout)?;
    let milliseconds =
        u32::try_from(rounded_milliseconds).map_err(|_| ClientError::InvalidRequestTimeout)?;
    if milliseconds == 0 {
        return Err(ClientError::RequestTimedOut);
    }
    Ok(milliseconds)
}

fn unix_time_ms() -> Result<u64, ClientError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ClientError::InvalidSystemClock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| ClientError::InvalidSystemClock)
}

fn parse_public_error(error: common::PublicError) -> Result<PublicError, ClientError> {
    if error.retry_after_ms.is_some() && !error.retryable {
        return Err(ClientError::InvalidPublicError);
    }
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
    /// A first-slice request violated a closed bound or identity invariant.
    #[error("daemon first-slice request is invalid")]
    InvalidFirstSliceRequest,
    /// A source reference violated its closed range contract.
    #[error("daemon source reference is invalid")]
    InvalidSourceReference,
    /// A first-slice response selected an unsupported schema.
    #[error("daemon response schema is invalid")]
    InvalidResponseSchema,
    /// A response did not correlate with the submitted request.
    #[error("daemon response correlation is invalid")]
    InvalidResponseCorrelation,
    /// A bounded response allocation could not be reserved.
    #[error("daemon response resources are exhausted")]
    ResponseAllocationFailed,
    /// Operation payload was missing.
    #[error("daemon operation status is missing")]
    MissingOperation,
    /// Daemon lifecycle was unspecified or unknown.
    #[error("daemon lifecycle is invalid")]
    InvalidDaemonLifecycle,
    /// A subsystem health status was unknown.
    #[error("daemon health status is invalid")]
    InvalidHealthStatus,
    /// A resource-pressure value was unknown.
    #[error("daemon resource pressure is invalid")]
    InvalidResourcePressure,
    /// Quick-diagnostics wire content violated its closed schema.
    #[error("daemon diagnostics response is invalid")]
    InvalidDiagnostics,
    /// Support-bundle bounds, digest, or privacy declaration was invalid.
    #[error("daemon support bundle is invalid")]
    InvalidSupportBundle,
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
    /// An asynchronous daemon request exceeded its total caller-level budget.
    #[error("daemon request timed out")]
    RequestTimedOut,
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
    /// A failed sibling daemon did not exit within the bounded cleanup deadline.
    #[error("daemon startup cleanup timed out")]
    DaemonLaunchCleanupTimedOut,
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
    use rootlight_ipc::{
        AsyncLocalListener, AsyncLocalStream, read_client_hello_async, read_request_async,
        write_response_async, write_server_hello_async,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
    };
    use tokio::io::AsyncReadExt as _;

    const STARTUP_CHILD_ROOT_ENV: &str = "ROOTLIGHT_TEST_STARTUP_CHILD_ROOT";

    fn test_endpoint(label: &str) -> Endpoint {
        #[cfg(unix)]
        let path = std::env::temp_dir().join(format!("rootlight-client-{label}.sock"));
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(r"\\.\pipe\rootlight-client-{label}"));
        Endpoint::new(path).expect("test endpoint validates")
    }

    fn async_test_endpoint(label: &str) -> (tempfile::TempDir, Endpoint) {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        #[cfg(unix)]
        let path = {
            let _ = label;
            use std::os::unix::fs::PermissionsExt as _;

            // Listener security must not depend on the CI runner's ambient umask.
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temporary endpoint parent becomes private");
            temporary.path().join("daemon.sock")
        };
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(
            r"\\.\pipe\rootlight-client-{label}-{}",
            temporary
                .path()
                .file_name()
                .expect("temporary directory has a name")
                .to_string_lossy()
        ));
        (
            temporary,
            Endpoint::new(path).expect("async test endpoint validates"),
        )
    }

    fn async_server_hello(instance_nonce: [u8; 16]) -> daemon::ServerHello {
        daemon::ServerHello {
            selected_protocol: Some(common::ContractVersion {
                major: 1,
                minor: CURRENT_PROTOCOL_MINOR,
            }),
            capabilities: CLIENT_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
            error: None,
            instance_nonce: instance_nonce.to_vec(),
        }
    }

    async fn receive_async_request(
        listener: &AsyncLocalListener,
        instance_nonce: [u8; 16],
    ) -> (AsyncLocalStream, daemon::RequestEnvelope) {
        let mut stream = listener
            .accept_timeout(Duration::from_secs(2))
            .await
            .expect("async client connects");
        let hello = read_client_hello_async(FrameCodec::default(), &mut stream)
            .await
            .expect("client hello decodes");
        assert_eq!(hello.expected_instance_nonce, instance_nonce);
        write_server_hello_async(
            FrameCodec::default(),
            &mut stream,
            &async_server_hello(instance_nonce),
        )
        .await
        .expect("server hello writes");
        let request = read_request_async(FrameCodec::default(), &mut stream)
            .await
            .expect("client request decodes");
        (stream, request)
    }

    async fn serve_async_responses(
        listener: AsyncLocalListener,
        instance_nonce: [u8; 16],
        responses: Vec<daemon::response_envelope::Response>,
    ) -> Vec<daemon::RequestEnvelope> {
        let mut requests = Vec::with_capacity(responses.len());
        for response in responses {
            let (mut stream, request) = receive_async_request(&listener, instance_nonce).await;
            write_response_async(
                FrameCodec::default(),
                &mut stream,
                &daemon::ResponseEnvelope {
                    request_id: request.request_id,
                    response: Some(response),
                },
            )
            .await
            .expect("server response writes");
            requests.push(request);
        }
        requests
    }

    fn test_repository() -> RepositoryId {
        RepositoryId::from_bytes([1; 16])
    }

    fn test_generation() -> GenerationId {
        GenerationId::from_bytes([2; 20])
    }

    fn test_source(file_byte: u8, start: u64, end: u64) -> SourceReference {
        SourceReference::new(
            test_repository(),
            test_generation(),
            FileId::from_bytes([file_byte; 20]),
            start..end,
            ContentHash::from_bytes([file_byte; 32]),
            Some(1..=1),
        )
        .expect("test source reference validates")
    }

    fn wire_query_context(results: u64, source_bytes: u64) -> daemon::FirstSliceQueryContext {
        daemon::FirstSliceQueryContext {
            repository: Some(repository_to_wire(test_repository())),
            generation: Some(generation_to_wire(test_generation())),
            parent_generation: Some(generation_to_wire(GenerationId::from_bytes([3; 20]))),
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

    fn wire_source(reference: &SourceReference) -> daemon::FirstSliceSourceRef {
        source_reference_to_wire(reference)
    }

    fn wire_operation(operation: OperationId) -> daemon::OperationStatus {
        daemon::OperationStatus {
            operation: Some(operation_to_wire(operation)),
            state: daemon::OperationState::Succeeded as i32,
            revision: 2,
            completed_units: 1,
            total_units: 1,
            error: None,
            kind: daemon::OperationKind::RepositoryIndex as i32,
            stage: daemon::OperationStage::Cleanup as i32,
            plan_hash: vec![4; 32],
            detached: true,
            cancellation_requested: false,
            deadline_unix_ms: Some(10),
            lease_expires_unix_ms: None,
            recovery_class: daemon::RecoveryClass::NotApplicable as i32,
        }
    }

    fn wire_failure(message: &str) -> common::PublicError {
        common::PublicError {
            code: common::ErrorCode::Internal as i32,
            message: message.to_owned(),
            retryable: false,
            retry_after_ms: None,
            repository: None,
            operation: None,
            generation: None,
            details: Default::default(),
            next_actions: Vec::new(),
        }
    }

    fn wire_repository_index(operation: OperationId) -> daemon::RepositoryIndexResponse {
        daemon::RepositoryIndexResponse {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(test_repository())),
            operation: Some(operation_to_wire(operation)),
            state: daemon::OperationState::Succeeded as i32,
            revision: 1,
            parent_generation: None,
            published_generation: Some(generation_to_wire(test_generation())),
            discovered_inputs: 1,
            indexed_files: 1,
            entities: 1,
            elapsed_micros: 10,
        }
    }

    fn wire_repository_status(operation: OperationId) -> daemon::RepositoryOperationStatusResponse {
        daemon::RepositoryOperationStatusResponse {
            schema_version: Some(first_slice_schema()),
            operation: Some(wire_operation(operation)),
            published_generation: Some(generation_to_wire(test_generation())),
            started_unix_ms: 1,
            peak_rss_bytes: 0,
            written_bytes: 0,
            files_examined: 1,
            retry_after_ms: None,
        }
    }

    fn wire_code_locate(symbol: SymbolId, source: &SourceReference) -> daemon::CodeLocateResponse {
        daemon::CodeLocateResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(1, 0)),
            hits: vec![daemon::FirstSliceLocateHit {
                symbol: Some(symbol_to_wire(symbol)),
                file: Some(file_to_wire(source.file)),
                identifier: "answer".to_owned(),
                qualified_name: "answer".to_owned(),
                path: "src/lib.rs".to_owned(),
                kind: "function".to_owned(),
                language: "rust".to_owned(),
                tier: daemon::FirstSliceAnalysisTier::FirstSliceTierC as i32,
                generated: false,
                score: 1_000,
                source: Some(wire_source(source)),
            }],
            matched_candidates: 1,
            truncated: false,
        }
    }

    fn wire_symbol_explain(
        symbol: SymbolId,
        source: &SourceReference,
    ) -> daemon::SymbolExplainResponse {
        daemon::SymbolExplainResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(1, 0)),
            symbols: vec![daemon::FirstSliceSymbolExplanation {
                symbol: Some(symbol_to_wire(symbol)),
                kind: "function".to_owned(),
                display_name: "answer".to_owned(),
                signature: None,
                definition: Some(wire_source(source)),
                outbound_exact: 0,
                outbound_candidates: 0,
                inbound_exact: 0,
                inbound_candidates: 0,
                references_exact: 0,
                provider: "tree-sitter".to_owned(),
                evidence: "parser".to_owned(),
                confidence: 1_000,
            }],
            unresolved_symbols: Vec::new(),
            truncated: false,
        }
    }

    fn wire_source_read(source: &SourceReference) -> daemon::SourceReadResponse {
        daemon::SourceReadResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(1, 3)),
            chunks: vec![daemon::FirstSliceSourceChunk {
                source: Some(wire_source(source)),
                path: "src/lib.rs".to_owned(),
                start_byte: source.start_byte,
                end_byte: source.end_byte,
                start_line: 1,
                end_line: 1,
                content: "abc".to_owned(),
                content_hash: Some(content_hash_to_wire(source.content_hash)),
                language: "rust".to_owned(),
                generated: false,
            }],
            total_source_bytes: 3,
            truncated: false,
        }
    }

    #[test]
    fn async_request_timeout_bounds_and_errors_are_source_free() {
        assert!(matches!(
            RequestTimeout::new(Duration::from_millis(9)),
            Err(ClientError::InvalidRequestTimeout)
        ));
        assert_eq!(
            RequestTimeout::new(Duration::from_millis(10))
                .expect("minimum request timeout validates")
                .duration(),
            Duration::from_millis(10)
        );
        assert_eq!(
            RequestTimeout::new(Duration::from_secs(30))
                .expect("maximum request timeout validates")
                .duration(),
            Duration::from_secs(30)
        );
        assert!(matches!(
            RequestTimeout::new(Duration::from_millis(30_001)),
            Err(ClientError::InvalidRequestTimeout)
        ));

        for error in [
            ClientError::InvalidRequestTimeout,
            ClientError::RequestTimedOut,
            ClientError::InvalidFirstSliceRequest,
            ClientError::MismatchedRequestId,
        ] {
            assert!(std::error::Error::source(&error).is_none());
            assert!(!error.to_string().contains("secret.rs"));
        }
    }

    #[test]
    fn async_deadline_is_fail_closed_and_wire_timeout_rounds_up() {
        let observed_at = TokioInstant::from_std(Instant::now());
        let exact_deadline = observed_at
            .checked_add(Duration::from_millis(10))
            .expect("test deadline is representable");
        let fractional_deadline = observed_at
            .checked_add(Duration::from_nanos(10_000_001))
            .expect("test deadline is representable");
        let submillisecond_deadline = observed_at
            .checked_add(Duration::from_nanos(1))
            .expect("test deadline is representable");

        assert_eq!(
            remaining_timeout_ms(exact_deadline, observed_at)
                .expect("exact milliseconds remain valid"),
            10
        );
        assert_eq!(
            remaining_timeout_ms(fractional_deadline, observed_at)
                .expect("fractional milliseconds remain valid"),
            11
        );
        assert_eq!(
            remaining_timeout_ms(submillisecond_deadline, observed_at)
                .expect("a positive submillisecond budget remains valid"),
            1
        );
        assert!(matches!(
            remaining_timeout_ms(observed_at, observed_at),
            Err(ClientError::RequestTimedOut)
        ));

        assert_eq!(
            finish_async_request(fractional_deadline, observed_at, Ok(7_u8))
                .expect("a result observed before the deadline is accepted"),
            7
        );
        assert!(matches!(
            finish_async_request(
                fractional_deadline,
                observed_at,
                Err::<u8, _>(ClientError::Ipc(IpcError::TimedOut)),
            ),
            Err(ClientError::Ipc(IpcError::TimedOut))
        ));
        assert!(matches!(
            finish_async_request(fractional_deadline, fractional_deadline, Ok(7_u8)),
            Err(ClientError::RequestTimedOut)
        ));
        assert!(matches!(
            finish_async_request(
                fractional_deadline,
                fractional_deadline
                    .checked_add(Duration::from_nanos(1))
                    .expect("test observation is representable"),
                Err::<u8, _>(ClientError::Ipc(IpcError::TimedOut)),
            ),
            Err(ClientError::RequestTimedOut)
        ));
    }

    #[tokio::test]
    async fn async_first_slice_calls_match_checked_sync_decoders() {
        let (_temporary, endpoint) = async_test_endpoint("parity");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let instance_nonce = [7; 16];
        let client_instance_id = [8; 16];
        let client = Client::new(endpoint, instance_nonce, client_instance_id);
        let timeout =
            RequestTimeout::new(Duration::from_secs(5)).expect("request timeout validates");
        let operation = OperationId::from_bytes([9; 16]);
        let symbol = SymbolId::from_bytes([5; 20]);
        let source = test_source(4, 0, 3);

        let index_wire = wire_repository_index(operation);
        let status_wire = wire_repository_status(operation);
        let locate_wire = wire_code_locate(symbol, &source);
        let explain_wire = wire_symbol_explain(symbol, &source);
        let source_wire = wire_source_read(&source);
        let expected_index =
            parse_repository_index(index_wire.clone(), operation).expect("index fixture parses");
        let expected_status = parse_repository_operation_status(status_wire.clone(), operation)
            .expect("status fixture parses");
        let expected_locate = parse_code_locate(
            locate_wire.clone(),
            test_repository(),
            GenerationSelector::Active,
            1,
        )
        .expect("locate fixture parses");
        let expected_explain = parse_symbol_explain(
            explain_wire.clone(),
            test_repository(),
            GenerationSelector::Active,
            &[symbol],
        )
        .expect("explain fixture parses");
        let expected_source = parse_source_read(
            source_wire.clone(),
            test_repository(),
            GenerationSelector::Active,
            std::slice::from_ref(&source),
        )
        .expect("source fixture parses");

        let server = tokio::spawn(serve_async_responses(
            listener,
            instance_nonce,
            vec![
                daemon::response_envelope::Response::RepositoryIndex(index_wire),
                daemon::response_envelope::Response::RepositoryOperationStatus(status_wire),
                daemon::response_envelope::Response::CodeLocate(locate_wire),
                daemon::response_envelope::Response::SymbolExplain(explain_wire),
                daemon::response_envelope::Response::SourceRead(source_wire),
            ],
        ));

        assert_eq!(
            client
                .repository_index_async("C:/fixture", operation, true, timeout)
                .await
                .expect("async repository index succeeds"),
            expected_index
        );
        assert_eq!(
            client
                .repository_operation_status_async(
                    operation,
                    RepositoryOperationAction::Get,
                    None,
                    None,
                    timeout,
                )
                .await
                .expect("async operation status succeeds"),
            expected_status
        );
        assert_eq!(
            client
                .code_locate_async(
                    test_repository(),
                    GenerationSelector::Active,
                    "answer",
                    LocateMode::Exact,
                    1,
                    timeout,
                )
                .await
                .expect("async code locate succeeds"),
            expected_locate
        );
        assert_eq!(
            client
                .symbol_explain_async(
                    test_repository(),
                    GenerationSelector::Active,
                    &[symbol],
                    timeout,
                )
                .await
                .expect("async symbol explain succeeds"),
            expected_explain
        );
        assert_eq!(
            client
                .source_read_async(
                    test_repository(),
                    GenerationSelector::Active,
                    std::slice::from_ref(&source),
                    timeout,
                )
                .await
                .expect("async source read succeeds"),
            expected_source
        );

        let requests = server.await.expect("server task joins");
        assert_eq!(
            requests
                .iter()
                .map(|request| request.request_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(requests.iter().all(|request| {
            request.instance_nonce == instance_nonce
                && request
                    .timeout_ms
                    .is_some_and(|milliseconds| (1..=5_000).contains(&milliseconds))
        }));
        assert!(matches!(
            requests[0].request.as_ref(),
            Some(daemon::request_envelope::Request::RepositoryIndex(request))
                if request.operation == Some(operation_to_wire(operation))
                    && request.root == "C:/fixture"
        ));
        assert!(matches!(
            requests[1].request.as_ref(),
            Some(daemon::request_envelope::Request::RepositoryOperationStatus(request))
                if request.operation == Some(operation_to_wire(operation))
        ));
        assert!(matches!(
            requests[2].request.as_ref(),
            Some(daemon::request_envelope::Request::CodeLocate(request))
                if request.query == "answer"
        ));
        assert!(matches!(
            requests[3].request.as_ref(),
            Some(daemon::request_envelope::Request::SymbolExplain(request))
                if request.symbols == vec![symbol_to_wire(symbol)]
        ));
        assert!(matches!(
            requests[4].request.as_ref(),
            Some(daemon::request_envelope::Request::SourceRead(request))
                if request.references == vec![wire_source(&source)]
        ));
    }

    #[tokio::test]
    async fn async_request_and_operation_correlation_fail_closed() {
        let (_temporary, endpoint) = async_test_endpoint("correlation");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let instance_nonce = [3; 16];
        let client = Client::new(endpoint, instance_nonce, [4; 16]);
        let operation = OperationId::from_bytes([5; 16]);
        let foreign_operation = OperationId::from_bytes([6; 16]);
        let timeout =
            RequestTimeout::new(Duration::from_secs(5)).expect("request timeout validates");

        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            let (mut stream, first) = receive_async_request(&listener, instance_nonce).await;
            write_response_async(
                FrameCodec::default(),
                &mut stream,
                &daemon::ResponseEnvelope {
                    request_id: first.request_id + 1,
                    response: Some(daemon::response_envelope::Response::RepositoryIndex(
                        wire_repository_index(operation),
                    )),
                },
            )
            .await
            .expect("mismatched response writes");
            requests.push(first);

            let (mut stream, second) = receive_async_request(&listener, instance_nonce).await;
            write_response_async(
                FrameCodec::default(),
                &mut stream,
                &daemon::ResponseEnvelope {
                    request_id: second.request_id,
                    response: Some(daemon::response_envelope::Response::RepositoryIndex(
                        wire_repository_index(foreign_operation),
                    )),
                },
            )
            .await
            .expect("foreign operation response writes");
            requests.push(second);
            requests
        });

        assert!(matches!(
            client
                .repository_index_async("C:/fixture", operation, true, timeout)
                .await,
            Err(ClientError::MismatchedRequestId)
        ));
        assert!(matches!(
            client
                .repository_index_async("C:/fixture", operation, true, timeout)
                .await,
            Err(ClientError::InvalidResponseCorrelation)
        ));

        let requests = server.await.expect("server task joins");
        assert_eq!(requests[0].request_id, 1);
        assert_eq!(requests[1].request_id, 2);
        assert!(requests.iter().all(|request| matches!(
            request.request.as_ref(),
            Some(daemon::request_envelope::Request::RepositoryIndex(index))
                if index.operation == Some(operation_to_wire(operation))
        )));
    }

    #[tokio::test]
    async fn dropping_async_request_closes_the_unsplit_peer_stream() {
        let (_temporary, endpoint) = async_test_endpoint("drop");
        let listener = AsyncLocalListener::bind(endpoint.clone()).expect("listener binds");
        let instance_nonce = [10; 16];
        let client = Client::new(endpoint, instance_nonce, [11; 16]);
        let timeout =
            RequestTimeout::new(Duration::from_secs(30)).expect("request timeout validates");
        let (received_sender, received_receiver) = tokio::sync::oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut stream, request) = receive_async_request(&listener, instance_nonce).await;
            received_sender
                .send(request)
                .expect("client request observation is delivered");
            let mut unexpected = [0_u8; 1];
            match stream.read(&mut unexpected).await {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::NotConnected
                            | io::ErrorKind::UnexpectedEof
                    ) => {}
                Ok(_) => panic!("client sent bytes after its one request"),
                Err(error) => panic!("peer close returned unexpected error: {error}"),
            }
        });

        let request = tokio::spawn(async move {
            client
                .code_locate_async(
                    test_repository(),
                    GenerationSelector::Active,
                    "answer",
                    LocateMode::Exact,
                    1,
                    timeout,
                )
                .await
        });
        let observed = received_receiver
            .await
            .expect("server observes the complete request");
        assert_eq!(observed.request_id, 1);
        request.abort();
        assert!(
            request
                .await
                .expect_err("request task is cancelled")
                .is_cancelled()
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("peer close observation stays bounded")
            .expect("server observes peer close");
    }

    fn spawn_cleanup_child(root: &std::path::Path) -> (Child, RuntimePaths) {
        let child_paths = RuntimePaths::new(root.join("state"), root.join("runtime"))
            .expect("child runtime paths are valid");
        let marker = root.join("ready");
        let mut child = Command::new(std::env::current_exe().expect("test executable resolves"))
            .args([
                "--exact",
                "tests::coordinated_startup_cleanup_child",
                "--nocapture",
            ])
            .env(STARTUP_CHILD_ROOT_ENV, root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cleanup child starts");
        let marker_deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("marker deadline is representable");
        while !marker.is_file() {
            assert!(
                child
                    .try_wait()
                    .expect("cleanup child status reads")
                    .is_none(),
                "cleanup child exited before acquiring its proof lock"
            );
            assert!(
                Instant::now() < marker_deadline,
                "cleanup child did not acquire its proof lock"
            );
            std::thread::sleep(START_POLL_INTERVAL);
        }
        (child, child_paths)
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
    fn coordinated_startup_retains_authority_and_reaps_failed_child() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("owner-state"),
            temporary.path().join("owner-runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let authority = paths
            .acquire_launch_lock()
            .expect("startup authority is acquired");

        let child_root = temporary.path().join("child");
        let child_paths = RuntimePaths::new(child_root.join("state"), child_root.join("runtime"))
            .expect("child runtime paths are valid");
        let marker = child_root.join("ready");
        let mut child = Command::new(std::env::current_exe().expect("test executable resolves"))
            .args([
                "--exact",
                "tests::coordinated_startup_cleanup_child",
                "--nocapture",
            ])
            .env(STARTUP_CHILD_ROOT_ENV, &child_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("cleanup child starts");
        let marker_deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("marker deadline is representable");
        while !marker.is_file() {
            assert!(
                child
                    .try_wait()
                    .expect("cleanup child status reads")
                    .is_none(),
                "cleanup child exited before acquiring its proof lock"
            );
            assert!(
                Instant::now() < marker_deadline,
                "cleanup child did not acquire its proof lock"
            );
            std::thread::sleep(START_POLL_INTERVAL);
        }

        let startup = CoordinatedStartup {
            authority: Some(authority),
            ownership: StartupOwnership::Detached,
            child: Some(child),
        };
        let contender_paths = paths.clone();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let waiter = std::thread::spawn(move || {
            started_tx
                .send(())
                .expect("startup wait observation begins");
            wait_for_ready_daemon(
                &contender_paths,
                [91; 16],
                Instant::now()
                    .checked_add(Duration::from_millis(250))
                    .expect("startup deadline is representable"),
                startup,
            )
        });
        started_rx
            .recv()
            .expect("startup wait observation is synchronized");
        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(rootlight_runtime::RuntimeError::LaunchBusy)
        ));
        assert!(matches!(
            waiter
                .join()
                .expect("startup wait thread joins")
                .expect_err("missing discovery must time out"),
            ClientError::DaemonStartTimedOut
        ));

        paths
            .acquire_launch_lock()
            .expect("startup authority releases after bounded cleanup");
        child_paths
            .acquire_launch_lock()
            .expect("terminated child releases its proof lock");
    }

    #[test]
    fn coordinated_startup_drop_reaps_exact_child_during_unwind() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("owner-state"),
            temporary.path().join("owner-runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let authority = paths
            .acquire_launch_lock()
            .expect("startup authority is acquired");
        let (child, child_paths) = spawn_cleanup_child(&temporary.path().join("child-unwind"));
        let child_id = child.id();
        let mut startup = CoordinatedStartup {
            authority: Some(authority),
            ownership: StartupOwnership::Detached,
            child: Some(child),
        };
        assert!(
            startup
                .matches_running(ReadyDaemonIdentity {
                    pid: child_id,
                    instance_nonce: [4; 16],
                })
                .expect("exact child remains observable")
        );
        assert!(
            !startup
                .matches_running(ReadyDaemonIdentity {
                    pid: child_id.saturating_add(1),
                    instance_nonce: [4; 16],
                })
                .expect("foreign identity is rejected")
        );

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _startup = startup;
            panic!("exercise startup unwind cleanup");
        }));
        assert!(unwind.is_err());
        paths
            .acquire_launch_lock()
            .expect("startup authority releases after exact child cleanup");
        child_paths
            .acquire_launch_lock()
            .expect("unwind cleanup reaps the exact child");
    }

    #[test]
    fn coordinated_startup_never_matches_an_exited_child() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("owner-state"),
            temporary.path().join("owner-runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let authority = paths
            .acquire_launch_lock()
            .expect("startup authority is acquired");
        let (mut child, child_paths) = spawn_cleanup_child(&temporary.path().join("child-exited"));
        let child_id = child.id();
        child.kill().expect("cleanup child can be terminated");
        child.wait().expect("cleanup child can be reaped");
        let mut startup = CoordinatedStartup {
            authority: Some(authority),
            ownership: StartupOwnership::Detached,
            child: Some(child),
        };

        assert!(
            !startup
                .matches_running(ReadyDaemonIdentity {
                    pid: child_id,
                    instance_nonce: [5; 16],
                })
                .expect("exited child status remains observable")
        );
        assert!(startup.child.is_none());
        child_paths
            .acquire_launch_lock()
            .expect("exited child released its proof lock");
    }

    #[derive(Debug)]
    struct FakeStartupProcess {
        exited: bool,
        terminate_failures: usize,
        status_failures: usize,
        exit_after_terminate: bool,
        terminate_calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl Drop for FakeStartupProcess {
        fn drop(&mut self) {
            self.dropped.store(true, AtomicOrdering::SeqCst);
        }
    }

    impl StartupProcess for FakeStartupProcess {
        fn try_exited(&mut self) -> io::Result<bool> {
            if self.status_failures > 0 {
                self.status_failures = self.status_failures.saturating_sub(1);
                return Err(io::Error::other("injected status failure"));
            }
            Ok(self.exited)
        }

        fn terminate(&mut self) -> io::Result<()> {
            self.terminate_calls.fetch_add(1, AtomicOrdering::SeqCst);
            if self.terminate_failures > 0 {
                self.terminate_failures = self.terminate_failures.saturating_sub(1);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected terminate failure",
                ));
            }
            if self.exit_after_terminate {
                self.exited = true;
            }
            Ok(())
        }
    }

    #[test]
    fn startup_cleanup_surfaces_kill_failure_and_timeout() {
        let mut kill_failure = FakeStartupProcess {
            exited: false,
            terminate_failures: 1,
            status_failures: 0,
            exit_after_terminate: false,
            terminate_calls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
        };
        assert!(matches!(
            terminate_startup_process_until(&mut kill_failure, || false),
            Err(ClientError::LaunchIo(error))
                if error.kind() == io::ErrorKind::PermissionDenied
        ));

        let mut timeout = FakeStartupProcess {
            exited: false,
            terminate_failures: 0,
            status_failures: 0,
            exit_after_terminate: false,
            terminate_calls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
        };
        assert!(matches!(
            terminate_startup_process_until(&mut timeout, || true),
            Err(ClientError::DaemonLaunchCleanupTimedOut)
        ));
    }

    #[test]
    fn bounded_startup_cleanup_retries_transient_failures() {
        let terminate_calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let process = FakeStartupProcess {
            exited: false,
            terminate_failures: 2,
            status_failures: 0,
            exit_after_terminate: true,
            terminate_calls: Arc::clone(&terminate_calls),
            dropped: Arc::clone(&dropped),
        };
        let mut pauses = 0_usize;

        let outcome = terminate_or_retain_startup_process_with(
            process,
            START_CHILD_RETAIN_ATTEMPTS,
            || pauses = pauses.saturating_add(1),
            || panic!("successful cleanup must not retain startup authority"),
        );

        assert_eq!(outcome, StartupCleanup::Reaped);
        assert_eq!(
            terminate_calls.load(AtomicOrdering::SeqCst),
            START_CHILD_RETAIN_ATTEMPTS
        );
        assert_eq!(pauses, START_CHILD_RETAIN_ATTEMPTS.saturating_sub(1));
        assert!(dropped.load(AtomicOrdering::SeqCst));
    }

    #[test]
    fn permanent_cleanup_failure_retains_process_and_startup_authority() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let authority = paths
            .acquire_launch_lock()
            .expect("startup authority is acquired");
        let retained_authority = std::cell::RefCell::new(None);
        let terminate_calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let process = FakeStartupProcess {
            exited: false,
            terminate_failures: usize::MAX,
            status_failures: usize::MAX,
            exit_after_terminate: false,
            terminate_calls: Arc::clone(&terminate_calls),
            dropped: Arc::clone(&dropped),
        };
        let mut pauses = 0_usize;

        let outcome = terminate_or_retain_startup_process_with(
            process,
            START_CHILD_RETAIN_ATTEMPTS,
            || pauses = pauses.saturating_add(1),
            || {
                retained_authority.replace(Some(authority));
            },
        );

        assert_eq!(outcome, StartupCleanup::Retained);
        assert_eq!(
            terminate_calls.load(AtomicOrdering::SeqCst),
            START_CHILD_RETAIN_ATTEMPTS
        );
        assert_eq!(pauses, START_CHILD_RETAIN_ATTEMPTS.saturating_sub(1));
        assert!(!dropped.load(AtomicOrdering::SeqCst));
        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(rootlight_runtime::RuntimeError::LaunchBusy)
        ));
        drop(retained_authority.take());
        paths
            .acquire_launch_lock()
            .expect("test releases retained startup authority explicitly");
    }

    #[test]
    fn coordinated_startup_cleanup_child() {
        let Some(root) = std::env::var_os(STARTUP_CHILD_ROOT_ENV) else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let paths = RuntimePaths::new(root.join("state"), root.join("runtime"))
            .expect("cleanup child runtime paths are valid");
        paths
            .prepare_owner()
            .expect("cleanup child runtime paths are private");
        let _proof = paths
            .acquire_launch_lock()
            .expect("cleanup child proof lock is acquired");
        std::fs::write(root.join("ready"), b"ready").expect("cleanup child marker writes");
        std::thread::sleep(Duration::from_secs(30));
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
    fn client_capabilities_and_request_gate_exclude_lease_renewal() {
        let hello = client_hello([7; 16], [9; 16]);
        assert_eq!(
            hello.capabilities,
            vec![
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
            ]
        );
        assert!(
            !hello
                .capabilities
                .iter()
                .any(|capability| capability == "operation.lease.renew")
        );

        let renewal = daemon::request_envelope::Request::OperationLeaseRenew(
            daemon::OperationLeaseRenewRequest {
                operation: Some(operation_to_wire(OperationId::from_bytes([10; 16]))),
                lease_expires_unix_ms: 1,
            },
        );
        for minor in MINIMUM_PROTOCOL_MINOR..=CURRENT_PROTOCOL_MINOR {
            assert!(matches!(
                ensure_request_supported(&renewal, minor),
                Err(ClientError::ProtocolFeatureUnavailable)
            ));
        }
    }

    #[test]
    fn lease_renewal_client_api_is_a_local_unsupported_stub() {
        let operation = OperationId::from_bytes([11; 16]);
        let client = Client::new(test_endpoint("lease-renewal-stub"), [7; 16], [9; 16]);
        assert!(matches!(
            client.operation_renew_lease(operation, 0),
            Err(ClientError::InvalidOperationLease)
        ));

        let error = client
            .operation_renew_lease(operation, 1)
            .expect_err("nonzero lease renewal remains unsupported");
        let public = error
            .as_public_error()
            .expect("unsupported compatibility stub returns a public error");
        assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
        assert_eq!(public.operation(), Some(operation));
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

        let diagnostics =
            daemon::request_envelope::Request::DiagnosticsQuick(daemon::DiagnosticsQuickRequest {});
        let support =
            daemon::request_envelope::Request::SupportBundle(daemon::SupportBundleRequest {});
        assert!(matches!(
            ensure_request_supported(&diagnostics, 2),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        assert!(matches!(
            ensure_request_supported(&support, 2),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        assert!(ensure_request_supported(&diagnostics, 3).is_ok());
        assert!(ensure_request_supported(&support, 3).is_ok());

        let first_slice =
            daemon::request_envelope::Request::CodeLocate(daemon::CodeLocateRequest::default());
        assert!(matches!(
            ensure_request_supported(&first_slice, 4),
            Err(ClientError::ProtocolFeatureUnavailable)
        ));
        assert!(ensure_request_supported(&first_slice, 5).is_ok());
        for capability in [
            "code.locate.v1",
            "repository.index.v1",
            "source.read.v1",
            "symbol.explain.v1",
        ] {
            assert!(CLIENT_CAPABILITIES.contains(&capability));
        }
    }

    #[test]
    fn first_slice_input_bounds_and_outer_pairing_fail_closed() {
        let client = Client::new(test_endpoint("first-slice-bounds"), [1; 16], [2; 16]);
        let operation = OperationId::from_bytes([8; 16]);
        assert!(matches!(
            client.repository_index("", operation, true),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            client.repository_operation_status(
                operation,
                RepositoryOperationAction::Get,
                Some(30_001),
                None,
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            client.code_locate(
                test_repository(),
                GenerationSelector::Active,
                "",
                LocateMode::Exact,
                1,
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            client.symbol_explain(test_repository(), GenerationSelector::Active, &[]),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        let source = test_source(4, 0, 2);
        assert!(matches!(
            client.source_read(
                test_repository(),
                GenerationSelector::Active,
                &[source.clone(), source],
            ),
            Err(ClientError::InvalidFirstSliceRequest)
        ));
        assert!(matches!(
            SourceReference::new(
                test_repository(),
                test_generation(),
                FileId::from_bytes([4; 20]),
                {
                    let start = 3;
                    let end = 2;
                    start..end
                },
                ContentHash::from_bytes([4; 32]),
                None,
            ),
            Err(ClientError::InvalidSourceReference)
        ));

        let response = daemon::ResponseEnvelope {
            request_id: 9,
            response: Some(daemon::response_envelope::Response::Health(
                daemon::HealthResponse::default(),
            )),
        };
        assert!(matches!(
            correlated_response(response, 8),
            Err(ClientError::MismatchedRequestId)
        ));
    }

    #[test]
    fn first_slice_decoders_validate_schema_identity_and_nested_correlation() {
        let operation = OperationId::from_bytes([8; 16]);
        let foreign_operation = OperationId::from_bytes([9; 16]);
        assert!(matches!(
            parse_expected_operation_status(Some(wire_operation(foreign_operation)), operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut failed_without_error = wire_operation(operation);
        failed_without_error.state = daemon::OperationState::Failed as i32;
        assert!(matches!(
            parse_expected_operation_status(Some(failed_without_error), operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut queued_with_error = wire_operation(operation);
        queued_with_error.state = daemon::OperationState::Queued as i32;
        queued_with_error.error = Some(wire_failure("checked failure"));
        assert!(matches!(
            parse_expected_operation_status(Some(queued_with_error), operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut checked_failure = wire_operation(operation);
        checked_failure.state = daemon::OperationState::Failed as i32;
        checked_failure.error = Some(wire_failure("checked failure"));
        assert!(parse_expected_operation_status(Some(checked_failure.clone()), operation).is_ok());
        let mut foreign_nested_error = checked_failure.clone();
        foreign_nested_error
            .error
            .as_mut()
            .expect("error exists")
            .operation = Some(operation_to_wire(foreign_operation));
        assert!(matches!(
            parse_expected_operation_status(Some(foreign_nested_error), operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut contradictory_retry = checked_failure;
        contradictory_retry
            .error
            .as_mut()
            .expect("error exists")
            .retry_after_ms = Some(1);
        assert!(matches!(
            parse_expected_operation_status(Some(contradictory_retry), operation),
            Err(ClientError::InvalidPublicError)
        ));
        let mut unsafe_failure = wire_operation(operation);
        unsafe_failure.state = daemon::OperationState::Failed as i32;
        unsafe_failure.error = Some(wire_failure(r"C:\secret\src\lib.rs"));
        assert!(matches!(
            parse_expected_operation_status(Some(unsafe_failure), operation),
            Err(ClientError::InvalidPublicError)
        ));

        let index = daemon::RepositoryIndexResponse {
            schema_version: Some(first_slice_schema()),
            repository: Some(repository_to_wire(test_repository())),
            operation: Some(operation_to_wire(operation)),
            state: daemon::OperationState::Succeeded as i32,
            revision: 1,
            parent_generation: None,
            published_generation: Some(generation_to_wire(test_generation())),
            discovered_inputs: 5,
            indexed_files: 3,
            entities: 2,
            elapsed_micros: 10,
        };
        assert!(parse_repository_index(index.clone(), operation).is_ok());
        let mut wrong_schema = index.clone();
        wrong_schema.schema_version = Some(common::ContractVersion { major: 1, minor: 1 });
        assert!(matches!(
            parse_repository_index(wrong_schema, operation),
            Err(ClientError::InvalidResponseSchema)
        ));
        let mut self_parent = index.clone();
        self_parent.parent_generation = self_parent.published_generation.clone();
        assert!(matches!(
            parse_repository_index(self_parent, operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut wrong_operation = index;
        wrong_operation.operation = Some(operation_to_wire(foreign_operation));
        assert!(matches!(
            parse_repository_index(wrong_operation, operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));

        let status = daemon::RepositoryOperationStatusResponse {
            schema_version: Some(first_slice_schema()),
            operation: Some(wire_operation(operation)),
            published_generation: Some(generation_to_wire(test_generation())),
            started_unix_ms: 1,
            peak_rss_bytes: 0,
            written_bytes: 0,
            files_examined: 5,
            retry_after_ms: None,
        };
        assert!(parse_repository_operation_status(status.clone(), operation).is_ok());
        let mut queued_without_retry = status.clone();
        queued_without_retry
            .operation
            .as_mut()
            .expect("operation exists")
            .state = daemon::OperationState::Queued as i32;
        queued_without_retry.published_generation = None;
        assert!(parse_repository_operation_status(queued_without_retry.clone(), operation).is_ok());
        queued_without_retry.retry_after_ms = Some(0);
        assert!(parse_repository_operation_status(queued_without_retry, operation).is_ok());
        let mut foreign_status = status;
        foreign_status
            .operation
            .as_mut()
            .expect("operation exists")
            .operation = Some(operation_to_wire(foreign_operation));
        assert!(matches!(
            parse_repository_operation_status(foreign_status, operation),
            Err(ClientError::InvalidResponseCorrelation)
        ));

        let source = test_source(4, 0, 2);
        let symbol = SymbolId::from_bytes([5; 20]);
        let locate = daemon::CodeLocateResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(2, 0)),
            hits: vec![daemon::FirstSliceLocateHit {
                symbol: Some(symbol_to_wire(symbol)),
                file: Some(file_to_wire(source.file)),
                identifier: "answer".to_owned(),
                qualified_name: "answer".to_owned(),
                path: "src/lib.rs".to_owned(),
                kind: "function".to_owned(),
                language: "rust".to_owned(),
                tier: daemon::FirstSliceAnalysisTier::FirstSliceTierC as i32,
                generated: false,
                score: 1_000,
                source: Some(wire_source(&source)),
            }],
            matched_candidates: 1,
            truncated: false,
        };
        assert!(
            parse_code_locate(
                locate.clone(),
                test_repository(),
                GenerationSelector::Active,
                1,
            )
            .is_ok()
        );
        assert!(
            parse_code_locate(
                locate.clone(),
                test_repository(),
                GenerationSelector::Generation(test_generation()),
                1,
            )
            .is_ok()
        );
        let mut incomplete_without_truncation = locate.clone();
        incomplete_without_truncation.matched_candidates = 2;
        assert!(matches!(
            parse_code_locate(
                incomplete_without_truncation.clone(),
                test_repository(),
                GenerationSelector::Active,
                1,
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        incomplete_without_truncation.truncated = true;
        assert!(
            parse_code_locate(
                incomplete_without_truncation,
                test_repository(),
                GenerationSelector::Active,
                1,
            )
            .is_ok()
        );
        let mut wrong_result_usage = locate.clone();
        wrong_result_usage
            .context
            .as_mut()
            .expect("context exists")
            .usage
            .as_mut()
            .expect("usage exists")
            .results = 0;
        assert!(matches!(
            parse_code_locate(
                wrong_result_usage,
                test_repository(),
                GenerationSelector::Active,
                1,
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut foreign_context = locate.clone();
        foreign_context
            .context
            .as_mut()
            .expect("context exists")
            .repository = Some(repository_to_wire(RepositoryId::from_bytes([9; 16])));
        assert!(matches!(
            parse_code_locate(
                foreign_context,
                test_repository(),
                GenerationSelector::Active,
                1,
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut foreign_source = locate;
        foreign_source.hits[0]
            .source
            .as_mut()
            .expect("source exists")
            .file = Some(file_to_wire(FileId::from_bytes([9; 20])));
        assert!(matches!(
            parse_code_locate(
                foreign_source,
                test_repository(),
                GenerationSelector::Active,
                1,
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));

        let second_symbol = SymbolId::from_bytes([6; 20]);
        let explain = daemon::SymbolExplainResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(2, 0)),
            symbols: vec![daemon::FirstSliceSymbolExplanation {
                symbol: Some(symbol_to_wire(symbol)),
                kind: "function".to_owned(),
                display_name: "answer".to_owned(),
                signature: None,
                definition: Some(wire_source(&source)),
                outbound_exact: 0,
                outbound_candidates: 0,
                inbound_exact: 0,
                inbound_candidates: 0,
                references_exact: 0,
                provider: "tree-sitter".to_owned(),
                evidence: "parser".to_owned(),
                confidence: 1_000,
            }],
            unresolved_symbols: vec![symbol_to_wire(second_symbol)],
            truncated: false,
        };
        assert!(
            parse_symbol_explain(
                explain.clone(),
                test_repository(),
                GenerationSelector::Active,
                &[symbol, second_symbol],
            )
            .is_ok()
        );
        let mut duplicate = explain;
        duplicate.unresolved_symbols[0] = symbol_to_wire(symbol);
        assert!(matches!(
            parse_symbol_explain(
                duplicate,
                test_repository(),
                GenerationSelector::Active,
                &[symbol, second_symbol],
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));

        let first = test_source(4, 1, 2);
        let second = test_source(5, 4, 5);
        let chunk = |reference: &SourceReference, start_byte: u64, end_byte: u64, content: &str| {
            daemon::FirstSliceSourceChunk {
                source: Some(wire_source(reference)),
                path: "src/lib.rs".to_owned(),
                start_byte,
                end_byte,
                start_line: 1,
                end_line: 1,
                content: content.to_owned(),
                content_hash: Some(content_hash_to_wire(reference.content_hash)),
                language: "rust".to_owned(),
                generated: false,
            }
        };
        let source_read = daemon::SourceReadResponse {
            schema_version: Some(first_slice_schema()),
            context: Some(wire_query_context(2, 6)),
            chunks: vec![chunk(&first, 0, 3, "aaa"), chunk(&second, 3, 6, "bbb")],
            total_source_bytes: 6,
            truncated: false,
        };
        assert!(
            parse_source_read(
                source_read.clone(),
                test_repository(),
                GenerationSelector::Active,
                &[first.clone(), second.clone()],
            )
            .is_ok()
        );
        let mut reordered = source_read.clone();
        reordered.chunks.swap(0, 1);
        assert!(matches!(
            parse_source_read(
                reordered,
                test_repository(),
                GenerationSelector::Active,
                &[first.clone(), second.clone()],
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
        let mut wrong_total = source_read;
        wrong_total.total_source_bytes = 3;
        assert!(matches!(
            parse_source_read(
                wrong_total,
                test_repository(),
                GenerationSelector::Active,
                &[first, second],
            ),
            Err(ClientError::InvalidResponseCorrelation)
        ));
    }

    #[test]
    fn health_decoder_requires_minor_three_additive_fields() {
        let response = daemon::HealthResponse {
            ready: true,
            active_operations: 0,
            admitted_operations: 0,
            protocol_version: "1.3".to_owned(),
            lifecycle: daemon::DaemonLifecycle::Ready as i32,
            accepting_operations: true,
            active_connections: 0,
            connection_limit: 128,
            queued_operations: 0,
            running_operations: 0,
            operation_queue_limit: 256,
            journal_healthy: true,
            catalog_status: daemon::HealthStatus::Unspecified as i32,
            catalog_schema_version: 0,
            generation_status: daemon::HealthStatus::Unspecified as i32,
            adapter_status: daemon::HealthStatus::Unspecified as i32,
            watcher_status: daemon::HealthStatus::Unspecified as i32,
            resource_pressure: daemon::ResourcePressure::Unspecified as i32,
            endpoint_status: daemon::HealthStatus::Unspecified as i32,
            endpoint_schema_version: 0,
        };

        let legacy = parse_health(response.clone(), 2).expect("minor two uses legacy defaults");
        assert_eq!(legacy.catalog_status, HealthStatus::Healthy);
        assert_eq!(legacy.generation_status, HealthStatus::NotConfigured);
        assert_eq!(legacy.resource_pressure, ResourcePressure::Unknown);
        assert!(matches!(
            parse_health(response, 3),
            Err(ClientError::InvalidHealthStatus)
        ));
    }

    fn support_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let output = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(output);
        let options =
            zip::write::SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, contents) in entries {
            writer
                .start_file(*name, options)
                .expect("test ZIP entry starts");
            std::io::Write::write_all(&mut writer, contents).expect("test ZIP entry writes");
        }
        writer.finish().expect("test ZIP finishes").into_inner()
    }

    fn valid_support_archive() -> Vec<u8> {
        let input = rootlight_observability::SupportBundleInput {
            protocol_version: rootlight_observability::ProtocolVersion::V1_3,
            operating_system: rootlight_observability::OperatingSystem::Windows,
            architecture: rootlight_observability::Architecture::X86_64,
            health: SupportHealth {
                ready: true,
                lifecycle: rootlight_observability::DaemonLifecycle::Ready,
                accepting_operations: true,
                active_connections: 1,
                connection_limit: 8,
                admitted_operations: 0,
                queued_operations: 0,
                running_operations: 0,
                operation_queue_limit: 8,
                catalog_status: rootlight_observability::HealthStatus::Healthy,
                catalog_schema_version: 2,
                generation_status: rootlight_observability::HealthStatus::NotConfigured,
                adapter_status: rootlight_observability::HealthStatus::NotConfigured,
                watcher_status: rootlight_observability::HealthStatus::NotConfigured,
                endpoint_status: rootlight_observability::HealthStatus::Healthy,
                endpoint_schema_version: 2,
                resource_pressure: rootlight_observability::ResourcePressure::Unknown,
            },
            diagnostics: SupportDiagnosticsQuick {
                schema_version: 1,
                overall_status: rootlight_observability::HealthStatus::Healthy,
                catalog_quick_check: rootlight_observability::DiagnosticOutcome::Passed,
                duration_ms: 1,
                error_code: None,
            },
            operations: SupportOperations {
                queued: 0,
                running: 0,
                cancelling: 0,
            },
            telemetry: None,
        };
        rootlight_observability::build_support_bundle(&input)
            .expect("test support bundle builds")
            .archive()
            .to_vec()
    }

    fn support_response(archive: Vec<u8>) -> daemon::SupportBundleResponse {
        support_response_with_schema(archive, SUPPORT_BUNDLE_SCHEMA_VERSION)
    }

    fn support_response_with_schema(
        archive: Vec<u8>,
        schema_version: u32,
    ) -> daemon::SupportBundleResponse {
        let digest: [u8; 32] = Sha256::digest(&archive).into();
        daemon::SupportBundleResponse {
            schema_version,
            archive_bytes: u64::try_from(archive.len()).expect("test archive fits u64"),
            archive,
            sha256: digest.to_vec(),
            contains_source: false,
        }
    }

    fn valid_support_archive_v2() -> Vec<u8> {
        valid_telemetry_support_archive(
            rootlight_observability::ProtocolVersion::V1_4,
            SupportBundleSchema::V2,
        )
    }

    fn valid_support_archive_v3() -> Vec<u8> {
        valid_telemetry_support_archive(
            rootlight_observability::ProtocolVersion::V1_5,
            SupportBundleSchema::V3,
        )
    }

    fn valid_telemetry_support_archive(
        protocol_version: rootlight_observability::ProtocolVersion,
        schema: SupportBundleSchema,
    ) -> Vec<u8> {
        let telemetry = rootlight_observability::Telemetry::default();
        telemetry.record_lifecycle(rootlight_observability::DaemonLifecycle::Ready);
        let input = rootlight_observability::SupportBundleInput {
            protocol_version,
            operating_system: rootlight_observability::OperatingSystem::Windows,
            architecture: rootlight_observability::Architecture::X86_64,
            health: SupportHealth {
                ready: true,
                lifecycle: rootlight_observability::DaemonLifecycle::Ready,
                accepting_operations: true,
                active_connections: 1,
                connection_limit: 8,
                admitted_operations: 0,
                queued_operations: 0,
                running_operations: 0,
                operation_queue_limit: 8,
                catalog_status: rootlight_observability::HealthStatus::Healthy,
                catalog_schema_version: 2,
                generation_status: rootlight_observability::HealthStatus::NotConfigured,
                adapter_status: rootlight_observability::HealthStatus::NotConfigured,
                watcher_status: rootlight_observability::HealthStatus::NotConfigured,
                endpoint_status: rootlight_observability::HealthStatus::Healthy,
                endpoint_schema_version: 2,
                resource_pressure: rootlight_observability::ResourcePressure::Unknown,
            },
            diagnostics: SupportDiagnosticsQuick {
                schema_version: 1,
                overall_status: rootlight_observability::HealthStatus::Healthy,
                catalog_quick_check: rootlight_observability::DiagnosticOutcome::Passed,
                duration_ms: 1,
                error_code: None,
            },
            operations: SupportOperations {
                queued: 0,
                running: 0,
                cancelling: 0,
            },
            telemetry: Some(telemetry.snapshot()),
        };
        build_support_bundle_for_schema(&input, schema)
            .expect("test telemetry support bundle builds")
            .archive()
            .to_vec()
    }

    fn support_entries(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = zip::ZipArchive::new(Cursor::new(archive)).expect("test ZIP opens");
        (0..archive.len())
            .map(|index| {
                let mut entry = archive.by_index(index).expect("test ZIP entry opens");
                let name = entry.name().to_owned();
                let mut contents = Vec::new();
                entry
                    .read_to_end(&mut contents)
                    .expect("test ZIP entry reads");
                (name, contents)
            })
            .collect()
    }

    const END_OF_CENTRAL_DIRECTORY_SIGNATURE: &[u8; 4] = b"PK\x05\x06";

    fn end_of_central_directory_offset(archive: &[u8]) -> usize {
        archive
            .windows(END_OF_CENTRAL_DIRECTORY_SIGNATURE.len())
            .rposition(|window| window == END_OF_CENTRAL_DIRECTORY_SIGNATURE)
            .expect("test support ZIP has an end-of-central-directory record")
    }

    fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .expect("test ZIP field has two bytes"),
        )
    }

    fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("test ZIP field has four bytes"),
        )
    }

    fn write_u16_le(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn central_directory_offset(archive: &[u8]) -> usize {
        let end = end_of_central_directory_offset(archive);
        usize::try_from(read_u32_le(archive, end + 16))
            .expect("test ZIP central-directory offset fits usize")
    }

    fn increase_central_directory_size(archive: &mut [u8], increase: u32) {
        let end = end_of_central_directory_offset(archive);
        let size = read_u32_le(archive, end + 12);
        write_u32_le(
            archive,
            end + 12,
            size.checked_add(increase)
                .expect("test ZIP central-directory size remains bounded"),
        );
    }

    fn with_archive_comment(mut archive: Vec<u8>) -> Vec<u8> {
        let end = end_of_central_directory_offset(&archive);
        assert_eq!(read_u16_le(&archive, end + 20), 0);
        write_u16_le(&mut archive, end + 20, 1);
        archive.push(b'x');
        archive
    }

    fn with_first_entry_comment(mut archive: Vec<u8>) -> Vec<u8> {
        let central_directory = central_directory_offset(&archive);
        assert_eq!(
            &archive[central_directory..central_directory + 4],
            b"PK\x01\x02"
        );
        let name_length = usize::from(read_u16_le(&archive, central_directory + 28));
        let extra_length = usize::from(read_u16_le(&archive, central_directory + 30));
        assert_eq!(read_u16_le(&archive, central_directory + 32), 0);
        let comment_offset = central_directory + 46 + name_length + extra_length;
        write_u16_le(&mut archive, central_directory + 32, 1);
        archive.insert(comment_offset, b'x');
        increase_central_directory_size(&mut archive, 1);
        archive
    }

    fn with_first_entry_extra_field(mut archive: Vec<u8>) -> Vec<u8> {
        let central_directory = central_directory_offset(&archive);
        assert_eq!(
            &archive[central_directory..central_directory + 4],
            b"PK\x01\x02"
        );
        let name_length = usize::from(read_u16_le(&archive, central_directory + 28));
        let extra_length = read_u16_le(&archive, central_directory + 30);
        let extra_offset = central_directory + 46 + name_length;
        write_u16_le(
            &mut archive,
            central_directory + 30,
            extra_length
                .checked_add(4)
                .expect("test ZIP extra-field length remains bounded"),
        );
        archive.splice(extra_offset..extra_offset, [0xfe, 0xca, 0, 0]);
        increase_central_directory_size(&mut archive, 4);
        archive
    }

    #[test]
    fn support_bundle_decoder_enforces_privacy_size_digest_and_shape() {
        let valid = support_response(valid_support_archive());
        assert!(parse_support_bundle(valid.clone(), 3).is_ok());

        let mut contains_source = valid.clone();
        contains_source.contains_source = true;
        assert!(matches!(
            parse_support_bundle(contains_source, 3),
            Err(ClientError::InvalidSupportBundle)
        ));

        let mut wrong_digest = valid.clone();
        wrong_digest.sha256 = vec![0; 32];
        assert!(matches!(
            parse_support_bundle(wrong_digest, 3),
            Err(ClientError::InvalidSupportBundle)
        ));

        let mut wrong_length = valid;
        wrong_length.archive_bytes = 1;
        assert!(matches!(
            parse_support_bundle(wrong_length, 3),
            Err(ClientError::InvalidSupportBundle)
        ));

        let wrong_shape = support_response(support_archive(&[("source.rs", b"{}\n")]));
        assert!(matches!(
            parse_support_bundle(wrong_shape, 3),
            Err(ClientError::InvalidSupportBundle)
        ));

        let poisoned_json =
            serde_json::to_vec("PRIVATE_SOURCE_BODY").expect("poisoned JSON serializes");
        let poisoned = support_response(support_archive(&[
            ("diagnostics/quick.json", &poisoned_json),
            ("health.json", &poisoned_json),
            ("manifest.json", &poisoned_json),
            ("operations-summary.json", &poisoned_json),
            ("redaction-report.json", &poisoned_json),
        ]));
        assert!(matches!(
            parse_support_bundle(poisoned, 3),
            Err(ClientError::InvalidSupportBundle)
        ));

        let archive = valid_support_archive();
        let mut entries = support_entries(&archive);
        let health_index = entries
            .iter()
            .position(|(name, _)| name == "health.json")
            .expect("health entry exists");
        let mut health: serde_json::Value =
            serde_json::from_slice(&entries[health_index].1).expect("health JSON parses");
        health["source"] = serde_json::Value::String("PRIVATE_SOURCE_BODY".to_owned());
        entries[health_index].1 = serde_json::to_vec_pretty(&health).expect("health JSON writes");
        let manifest_index = entries
            .iter()
            .position(|(name, _)| name == "manifest.json")
            .expect("manifest entry exists");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&entries[manifest_index].1).expect("manifest JSON parses");
        let health_record = manifest["entries"]
            .as_array_mut()
            .expect("manifest records exist")
            .iter_mut()
            .find(|entry| entry["name"] == "health.json")
            .expect("health manifest record exists");
        health_record["bytes"] = serde_json::Value::from(entries[health_index].1.len());
        health_record["sha256"] = serde_json::Value::String(
            hex_sha256(&entries[health_index].1).expect("test digest formatting succeeds"),
        );
        entries[manifest_index].1 =
            serde_json::to_vec_pretty(&manifest).expect("manifest JSON writes");
        let entry_refs = entries
            .iter()
            .map(|(name, contents)| (name.as_str(), contents.as_slice()))
            .collect::<Vec<_>>();
        let unknown_source = support_response(support_archive(&entry_refs));
        assert!(matches!(
            parse_support_bundle(unknown_source, 3),
            Err(ClientError::InvalidSupportBundle)
        ));
    }

    #[test]
    fn support_bundle_decoder_negotiates_telemetry_schema() {
        let v2 = support_response_with_schema(
            valid_support_archive_v2(),
            PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION,
        );
        let parsed = parse_support_bundle(v2.clone(), 4)
            .expect("protocol 1.4 accepts schema v2 support evidence");
        assert_eq!(
            parsed.schema_version,
            PREVIOUS_SUPPORT_BUNDLE_SCHEMA_VERSION
        );
        assert!(parsed.telemetry.is_some());
        assert!(matches!(
            parse_support_bundle(v2.clone(), 3),
            Err(ClientError::InvalidSupportBundle)
        ));
        assert!(matches!(
            parse_support_bundle(v2, 5),
            Err(ClientError::InvalidSupportBundle)
        ));

        let v3 = support_response_with_schema(
            valid_support_archive_v3(),
            CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION,
        );
        let parsed = parse_support_bundle(v3.clone(), 5)
            .expect("protocol 1.5 accepts schema v3 support evidence");
        assert_eq!(parsed.schema_version, CURRENT_SUPPORT_BUNDLE_SCHEMA_VERSION);
        assert!(parsed.telemetry.is_some());
        assert!(matches!(
            parse_support_bundle(v3, 4),
            Err(ClientError::InvalidSupportBundle)
        ));
        assert!(matches!(
            parse_support_bundle(support_response(valid_support_archive()), 4),
            Err(ClientError::InvalidSupportBundle)
        ));
    }

    #[test]
    fn support_bundle_decoder_requires_canonical_zip_bytes() {
        let canonical = valid_support_archive();
        let mut trailing = canonical.clone();
        trailing.extend_from_slice(b"source-bearing trailing bytes");
        for archive in [
            trailing,
            with_archive_comment(canonical.clone()),
            with_first_entry_comment(canonical.clone()),
            with_first_entry_extra_field(canonical),
        ] {
            assert!(matches!(
                parse_support_bundle(support_response(archive), 3),
                Err(ClientError::InvalidSupportBundle)
            ));
        }
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
        let identity = ReadyDaemonIdentity {
            pid: 1,
            instance_nonce: [1; 16],
        };
        assert!(matches!(
            classify_health_probe(client, identity, Err(ClientError::ProtocolMismatch)),
            Err(ClientError::ProtocolMismatch)
        ));
    }

    #[test]
    fn readiness_probe_only_treats_connection_absence_as_unavailable() {
        let endpoint = test_endpoint("absent");
        let client = Client::new(endpoint, [1; 16], [2; 16]);
        let identity = ReadyDaemonIdentity {
            pid: 1,
            instance_nonce: [1; 16],
        };
        let unavailable = classify_health_probe(
            client,
            identity,
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
            classify_health_probe(client, identity, Err(ClientError::NonceMismatch)),
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
