use std::time::Duration;

use rootlight_cancel::CancellationReason;
use rootlight_ids::{ContentHash, FileId, GenerationId, SymbolId};
use rootlight_ir::{
    CoverageRecord, EntityRecord, OccurrenceRecord, ProvenanceRecord, RelationRecord, SourceRef,
};
use rootlight_search::{SearchBudget, SearchError, SearchMode};
use rootlight_source::{SourceBudget, SourceError, SourceReadOptions};
use serde::Serialize;

const HARD_MAX_QUERY_ROWS: u64 = 1_000_000;
const HARD_MAX_QUERY_EDGES: u64 = 1_000_000;
const HARD_MAX_QUERY_RESULTS: u64 = 10_000;
const HARD_MAX_QUERY_SOURCE_BYTES: u64 = 512 * 1024;
const HARD_MAX_QUERY_JSON_BYTES: u64 = 4 * 1024 * 1024;
const HARD_MAX_QUERY_TOKENS: u64 = 4 * 1024 * 1024;
const HARD_MAX_QUERY_MEMORY_BYTES: u64 = 128 * 1024 * 1024;
const HARD_MAX_QUERY_DURATION: Duration = Duration::from_secs(10);

/// Shared resource admission for one daemon-independent query plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryBudget {
    pub(crate) max_rows: u64,
    pub(crate) max_edges: u64,
    pub(crate) max_results: u64,
    pub(crate) max_source_bytes: u64,
    pub(crate) max_json_bytes: u64,
    pub(crate) max_tokens: u64,
    pub(crate) max_memory_bytes: u64,
    pub(crate) max_duration: Duration,
}

impl QueryBudget {
    /// Creates the default interactive first-slice budget.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_rows: 250_000,
            max_edges: 100_000,
            max_results: 1_000,
            max_source_bytes: 64 * 1024,
            max_json_bytes: 1024 * 1024,
            max_tokens: 1_000_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_duration: Duration::from_secs(2),
        }
    }

    /// Replaces the row ceiling.
    #[must_use]
    pub const fn with_max_rows(mut self, maximum: u64) -> Self {
        self.max_rows = maximum;
        self
    }

    /// Replaces the traversed-edge ceiling.
    #[must_use]
    pub const fn with_max_edges(mut self, maximum: u64) -> Self {
        self.max_edges = maximum;
        self
    }

    /// Replaces the returned-record ceiling.
    #[must_use]
    pub const fn with_max_results(mut self, maximum: u64) -> Self {
        self.max_results = maximum;
        self
    }

    /// Replaces the returned source-byte ceiling.
    #[must_use]
    pub const fn with_max_source_bytes(mut self, maximum: u64) -> Self {
        self.max_source_bytes = maximum;
        self
    }

    /// Replaces the exact serialized-response byte ceiling.
    #[must_use]
    pub const fn with_max_json_bytes(mut self, maximum: u64) -> Self {
        self.max_json_bytes = maximum;
        self
    }

    /// Replaces the conservative output-token ceiling.
    #[must_use]
    pub const fn with_max_tokens(mut self, maximum: u64) -> Self {
        self.max_tokens = maximum;
        self
    }

    /// Replaces the owned response-memory ceiling.
    #[must_use]
    pub const fn with_max_memory_bytes(mut self, maximum: u64) -> Self {
        self.max_memory_bytes = maximum;
        self
    }

    /// Replaces the cooperative monotonic duration ceiling.
    #[must_use]
    pub const fn with_max_duration(mut self, maximum: Duration) -> Self {
        self.max_duration = maximum;
        self
    }

    pub(crate) fn validate(self) -> Result<(), QueryError> {
        for (resource, value, maximum) in [
            (QueryResource::Rows, self.max_rows, HARD_MAX_QUERY_ROWS),
            (QueryResource::Edges, self.max_edges, HARD_MAX_QUERY_EDGES),
            (
                QueryResource::Results,
                self.max_results,
                HARD_MAX_QUERY_RESULTS,
            ),
            (
                QueryResource::SourceBytes,
                self.max_source_bytes,
                HARD_MAX_QUERY_SOURCE_BYTES,
            ),
            (
                QueryResource::JsonBytes,
                self.max_json_bytes,
                HARD_MAX_QUERY_JSON_BYTES,
            ),
            (
                QueryResource::Tokens,
                self.max_tokens,
                HARD_MAX_QUERY_TOKENS,
            ),
            (
                QueryResource::MemoryBytes,
                self.max_memory_bytes,
                HARD_MAX_QUERY_MEMORY_BYTES,
            ),
        ] {
            if value == 0 || value > maximum {
                return Err(QueryError::InvalidBudget { resource, maximum });
            }
        }
        if self.max_duration.is_zero() || self.max_duration > HARD_MAX_QUERY_DURATION {
            return Err(QueryError::InvalidDurationBudget {
                maximum: HARD_MAX_QUERY_DURATION,
            });
        }
        Ok(())
    }
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Resource families admitted and measured by the query layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum QueryResource {
    /// Logical rows inspected or returned.
    Rows,
    /// Relationship edges traversed.
    Edges,
    /// Records returned to the caller.
    Results,
    /// Raw source bytes returned.
    SourceBytes,
    /// Exact JSON bytes for the complete versioned response.
    JsonBytes,
    /// Conservative output-token estimate.
    Tokens,
    /// Variable-sized response memory.
    MemoryBytes,
}

/// Intent represented by a deterministic query plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanKind {
    /// Lexically locate code entities.
    CodeLocate,
    /// Explain one stable symbol.
    SymbolExplain,
    /// Read generation-bound source.
    SourceRead,
}

/// Closed first-slice operator catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum QueryOperator {
    /// Assert the immutable generation before other work.
    GenerationPin,
    /// Execute bounded lexical retrieval.
    LexicalSearch,
    /// Resolve lexical identities against normalized IR.
    EntityHydration,
    /// Resolve one entity by stable identity.
    EntityLookup,
    /// Scan typed relations with an edge cap.
    RelationScan,
    /// Scan source occurrences with a row cap.
    OccurrenceScan,
    /// Resolve deduplicated provenance.
    ProvenanceLookup,
    /// Project relevant completeness records.
    CoverageProjection,
    /// Resolve indexed source references.
    SourceResolve,
    /// Snapshot source through the VFS capability.
    VfsSnapshotRead,
    /// Verify immutable source identity.
    ContentHashVerify,
    /// Measure and enforce the response envelope.
    OutputBudget,
}

/// Conservative deterministic cost bound produced before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlanEstimate {
    /// Maximum logical rows the plan may inspect.
    pub rows: u64,
    /// Maximum typed edges the plan may traverse.
    pub edges: u64,
    /// Maximum records the plan may return.
    pub results: u64,
    /// Maximum raw source bytes the plan may return.
    pub source_bytes: u64,
    /// Maximum variable-sized response memory.
    pub memory_bytes: u64,
    /// Maximum exact JSON bytes admitted for the complete response.
    pub json_bytes: u64,
    /// Maximum conservative output-token estimate.
    pub estimated_tokens: u64,
    /// Maximum monotonic execution duration, rounded up to microseconds.
    pub duration_micros: u64,
}

/// Stable explain form for one typed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanExplanation {
    /// Immutable generation selected by the plan.
    pub generation: GenerationId,
    /// Public intent represented by the plan.
    pub kind: PlanKind,
    /// Deterministic operator sequence.
    pub operators: Vec<QueryOperator>,
    /// Conservative pre-execution resource estimate.
    pub estimate: PlanEstimate,
}

/// Runtime counters observed for a completed plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QueryUsage {
    /// Logical rows inspected or materialized.
    pub rows: u64,
    /// Typed relation edges inspected or returned.
    pub edges: u64,
    /// Records returned in the typed data payload.
    pub results: u64,
    /// Raw source bytes returned before JSON escaping.
    pub source_bytes: u64,
    /// Exact JSON bytes of the complete response, including plan and usage.
    pub json_bytes: u64,
    /// Conservative output-token upper bound under `token_accounting`.
    pub estimated_tokens: u64,
    /// Versioned profile used to derive `estimated_tokens`.
    pub token_accounting: TokenAccountingProfile,
    /// Variable-sized bytes owned by the typed data payload.
    pub memory_bytes: u64,
    /// Monotonic execution duration rounded up to microseconds.
    pub elapsed_micros: u64,
}

/// Versioned conservative token-accounting profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TokenAccountingProfile {
    /// Counts each serialized UTF-8 byte as one possible tokenizer token.
    Utf8ByteUpperBoundV1,
}

/// Typed plan result with explain and measured resource evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryResponse<T> {
    /// Deterministic plan selected before execution.
    pub plan: PlanExplanation,
    /// Intent-specific typed data.
    pub data: T,
    /// Observed runtime counters.
    pub usage: QueryUsage,
}

/// Locate matching mode independent of a lexical backend implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LocateMode {
    /// Case-insensitive whole identifier.
    Exact,
    /// Case-insensitive identifier prefix.
    Prefix,
    /// Code-aware lexical text search.
    Text,
    /// Restricted finite-automaton regular expression.
    SafeRegex,
    /// Restricted finite-automaton glob.
    Glob,
}

impl From<LocateMode> for SearchMode {
    fn from(mode: LocateMode) -> Self {
        match mode {
            LocateMode::Exact => Self::Exact,
            LocateMode::Prefix => Self::Prefix,
            LocateMode::Text => Self::Text,
            LocateMode::SafeRegex => Self::SafeRegex,
            LocateMode::Glob => Self::Glob,
        }
    }
}

/// Prevalidated lexical locate plan.
#[derive(Debug, Clone)]
pub struct CodeLocatePlan {
    pub(crate) query: String,
    pub(crate) mode: LocateMode,
    pub(crate) max_results: usize,
    pub(crate) search_budget: SearchBudget,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl CodeLocatePlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Prevalidated symbol explanation plan.
#[derive(Debug, Clone)]
pub struct SymbolExplainPlan {
    pub(crate) symbol: SymbolId,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl SymbolExplainPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Prevalidated source-read plan.
#[derive(Debug, Clone)]
pub struct SourceReadPlan {
    pub(crate) references: Vec<SourceRef>,
    pub(crate) options: SourceReadOptions,
    pub(crate) source_budget: SourceBudget,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl SourceReadPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Mandatory trust label for repository-controlled query values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RepositoryDataTrust {
    /// Repository text is untrusted data and never instructions.
    UntrustedRepositoryData,
}

/// One hydrated deterministic lexical result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocateHit {
    /// Stable semantic entity identity.
    pub symbol: SymbolId,
    /// Stable declaring file identity.
    pub file: FileId,
    /// Declared source spelling.
    pub identifier: String,
    /// Qualified presentation name.
    pub qualified_name: String,
    /// Repository-relative display path.
    pub path: String,
    /// Closed semantic kind label.
    pub kind: String,
    /// Language identity.
    pub language: String,
    /// Truthfulness tier.
    pub tier: String,
    /// Whether the declaring file is generated.
    pub generated: bool,
    /// Backend relevance score before stable identity tie-breaking.
    pub relevance_score: f32,
    /// Exact source evidence when supplied by the producer.
    pub source: Option<SourceRef>,
    /// Mandatory trust marker for presentation text.
    pub trust: RepositoryDataTrust,
}

/// Data returned by a `code.locate` plan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeLocateResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Hydrated hits in deterministic relevance and identity order.
    pub hits: Vec<LocateHit>,
    /// Matching lexical candidates before the public result cap.
    pub matched_candidates: u64,
    /// Deduplicated coverage evidence relevant to returned entities.
    pub coverage: Vec<CoverageRecord>,
    /// Whether a result or resource limit stopped complete materialization.
    pub truncated: bool,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
}

/// Data returned by a `symbol.explain` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolExplainResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Exact normalized entity.
    pub entity: EntityRecord,
    /// Typed relations touching the entity.
    pub relations: Vec<RelationRecord>,
    /// Source occurrences enclosing or targeting the entity.
    pub occurrences: Vec<OccurrenceRecord>,
    /// Deduplicated producer record for the entity.
    pub provenance: ProvenanceRecord,
    /// Coverage evidence relevant to the entity.
    pub coverage: Vec<CoverageRecord>,
    /// Whether a resource limit stopped any optional scan.
    pub truncated: bool,
    /// Resource limits that stopped optional scans, in deterministic scan order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled names and labels.
    pub trust: RepositoryDataTrust,
}

/// One verified UTF-8 source chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceChunkResult {
    /// Exact indexed selector.
    pub reference: SourceRef,
    /// Repository-relative display path.
    pub path: String,
    /// Expanded byte start.
    pub start_byte: u64,
    /// Expanded byte end.
    pub end_byte: u64,
    /// One-based first included line.
    pub start_line: u64,
    /// One-based last included line.
    pub end_line: u64,
    /// Exact verified UTF-8 text without normalization.
    pub text: String,
    /// Immutable content identity.
    pub content_hash: ContentHash,
    /// Indexed language identity.
    pub language: String,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Mandatory trust marker for every repository-controlled field.
    pub trust: RepositoryDataTrust,
}

/// Data returned by a `source.read` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReadQueryResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Verified chunks in selector order.
    pub chunks: Vec<SourceChunkResult>,
}

/// Failure from the bounded daemon-independent query layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueryError {
    /// A caller budget was zero or exceeded the hard ceiling.
    #[error("query budget is invalid for {resource:?}")]
    InvalidBudget {
        /// Invalid resource family.
        resource: QueryResource,
        /// Contract hard ceiling.
        maximum: u64,
    },
    /// A duration was zero or exceeded the hard ceiling.
    #[error("query duration budget is invalid")]
    InvalidDurationBudget {
        /// Contract hard ceiling.
        maximum: Duration,
    },
    /// A deterministic plan estimate exceeded admission.
    #[error("query plan exceeds its admitted {resource:?} budget")]
    PlanRejected {
        /// Resource that cannot be admitted.
        resource: QueryResource,
    },
    /// Runtime work exceeded an admitted resource.
    #[error("query execution exceeded its {resource:?} budget")]
    BudgetExceeded {
        /// Exhausted resource.
        resource: QueryResource,
        /// Admitted maximum.
        limit: u64,
    },
    /// Cooperative cancellation or deadline expiry stopped work.
    #[error("query execution was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// A plan or backend belonged to another immutable generation.
    #[error("query generation does not match its pinned inputs")]
    GenerationMismatch,
    /// No entity matched the stable identity.
    #[error("query symbol was not found in the pinned generation")]
    SymbolNotFound,
    /// Search metadata did not resolve to canonical normalized IR.
    #[error("lexical index does not match the pinned generation")]
    IndexDrift,
    /// Entity provenance was absent from normalized IR.
    #[error("normalized entity provenance is incomplete")]
    ProvenanceMissing,
    /// Bounded lexical execution failed.
    #[error("lexical query failed")]
    Search(#[source] SearchError),
    /// Bounded source execution failed.
    #[error("source query failed")]
    Source(#[source] SourceError),
    /// A verified source chunk was not representable as UTF-8.
    #[error("source query returned an invalid encoding")]
    InvalidSourceEncoding,
    /// Exact output measurement could not encode the typed result.
    #[error("query result encoding failed")]
    ResultEncoding,
    /// The in-memory first-slice generation set was invalid.
    #[error("query generation set is invalid")]
    InvalidGenerationSet,
    /// The requested retained generation was unavailable.
    #[error("query generation is not retained")]
    GenerationNotFound,
    /// The retained generation capacity was exhausted.
    #[error("query generation retention limit was reached")]
    RetentionLimit,
    /// A generation identity was already retained.
    #[error("query generation is already retained")]
    DuplicateGeneration,
    /// A response allocation could not be admitted.
    #[error("query response memory is unavailable")]
    MemoryUnavailable,
}

impl From<SearchError> for QueryError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::Cancelled(reason) => Self::Cancelled(reason),
            error => Self::Search(error),
        }
    }
}

impl From<SourceError> for QueryError {
    fn from(error: SourceError) -> Self {
        match error {
            SourceError::Cancelled(reason) => Self::Cancelled(reason),
            error => Self::Source(error),
        }
    }
}

pub(crate) fn checked_usize_to_u64(value: usize) -> Result<u64, QueryError> {
    u64::try_from(value).map_err(|_| QueryError::BudgetExceeded {
        resource: QueryResource::MemoryBytes,
        limit: HARD_MAX_QUERY_MEMORY_BYTES,
    })
}

pub(crate) fn checked_u128_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub(crate) fn checked_add(
    left: u64,
    right: u64,
    resource: QueryResource,
    limit: u64,
) -> Result<u64, QueryError> {
    let value = left
        .checked_add(right)
        .ok_or(QueryError::BudgetExceeded { resource, limit })?;
    if value > limit {
        Err(QueryError::BudgetExceeded { resource, limit })
    } else {
        Ok(value)
    }
}

pub(crate) fn ensure_estimate(
    estimate: PlanEstimate,
    budget: QueryBudget,
) -> Result<(), QueryError> {
    for (resource, estimate, maximum) in [
        (QueryResource::Rows, estimate.rows, budget.max_rows),
        (QueryResource::Edges, estimate.edges, budget.max_edges),
        (QueryResource::Results, estimate.results, budget.max_results),
        (
            QueryResource::SourceBytes,
            estimate.source_bytes,
            budget.max_source_bytes,
        ),
        (
            QueryResource::MemoryBytes,
            estimate.memory_bytes,
            budget.max_memory_bytes,
        ),
        (
            QueryResource::JsonBytes,
            estimate.json_bytes,
            budget.max_json_bytes,
        ),
        (
            QueryResource::Tokens,
            estimate.estimated_tokens,
            budget.max_tokens,
        ),
    ] {
        if estimate > maximum {
            return Err(QueryError::PlanRejected { resource });
        }
    }
    Ok(())
}

pub(crate) fn search_mode(mode: LocateMode) -> SearchMode {
    mode.into()
}
