use std::collections::BTreeSet;
use std::time::Duration;

use rootlight_cancel::CancellationReason;
use rootlight_ids::{ContentHash, FileId, GenerationId, SymbolId};
use rootlight_ir::{
    CoverageRecord, CoverageStatus, EntityKind as IrEntityKind, EntityRecord, OccurrenceRecord,
    ProvenanceRecord, RelationPredicate, RelationRecord, SourceRef,
};
use rootlight_search::{SearchBudget, SearchError, SearchMode};
use rootlight_source::{SourceBudget, SourceError, SourceReadOptions};
use serde::{Deserialize, Serialize};

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
    /// Expand typed relation neighborhoods for stable symbols.
    SymbolRelationships,
    /// Trace bounded directed paths between stable symbols.
    FlowTrace,
    /// Detect bounded architecture cycles among stable symbols.
    ArchitectureCycles,
    /// Detect bounded dead-code candidates among stable symbols.
    CodeDead,
    /// Aggregate a bounded file-granularity architecture overview.
    ArchitectureOverview,
    /// Select bounded relevant tests for a seed set.
    TestsSelect,
    /// Map bounded change impact for an explicit change set.
    ChangeImpact,
    /// Build a bounded ordered change plan for explicit targets.
    PlanChange,
    /// Compare two immutable generations for bounded semantic changes.
    HistoryCompare,
    /// Read generation-bound source.
    SourceRead,
    /// Execute a bounded advanced query over a safe typed AST.
    QueryAdvanced,
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

/// Traversal direction for a relationship expansion relative to the seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RelationDirection {
    /// Follow outbound edges from the seed (seed is the subject).
    Outbound,
    /// Follow inbound edges toward the seed (seed is the object).
    Inbound,
    /// Follow edges in both directions.
    Both,
}

impl RelationDirection {
    /// Returns the stable wire label shared with the MCP direction contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
            Self::Both => "both",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "outbound" => Some(Self::Outbound),
            "inbound" => Some(Self::Inbound),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

/// Typed relation family expanded by a `symbol.relationships` query.
///
/// Each family maps to a closed set of normalized IR predicates plus a natural
/// traversal direction relative to the seed. Families without first-slice
/// oracle data map to an empty predicate set and therefore expand to no groups
/// rather than fabricated edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RelationFamily {
    /// Direct outbound call.
    Calls,
    /// Inbound caller.
    CalledBy,
    /// Symbol reference or usage.
    References,
    /// Type dependency or type-of relation.
    Types,
    /// Trait or interface implementation.
    Implements,
    /// Import or module dependency.
    Imports,
    /// Test coverage relation.
    Tests,
    /// Code ownership or authorship.
    Ownership,
    /// Service or RPC call.
    ServiceCall,
    /// HTTP route invocation.
    CallsRoute,
    /// Message publish or consume.
    Messaging,
    /// Database table read.
    ReadsTable,
    /// Database table write.
    WritesTable,
    /// Build or compilation dependency.
    BuildDependency,
    /// Data flow propagation.
    DataFlow,
    /// Co-change history signal.
    History,
}

impl RelationFamily {
    /// Returns the stable wire label shared with the MCP relation contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::CalledBy => "called_by",
            Self::References => "references",
            Self::Types => "types",
            Self::Implements => "implements",
            Self::Imports => "imports",
            Self::Tests => "tests",
            Self::Ownership => "ownership",
            Self::ServiceCall => "service_call",
            Self::CallsRoute => "calls_route",
            Self::Messaging => "messaging",
            Self::ReadsTable => "reads_table",
            Self::WritesTable => "writes_table",
            Self::BuildDependency => "build_dependency",
            Self::DataFlow => "data_flow",
            Self::History => "history",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "calls" => Some(Self::Calls),
            "called_by" => Some(Self::CalledBy),
            "references" => Some(Self::References),
            "types" => Some(Self::Types),
            "implements" => Some(Self::Implements),
            "imports" => Some(Self::Imports),
            "tests" => Some(Self::Tests),
            "ownership" => Some(Self::Ownership),
            "service_call" => Some(Self::ServiceCall),
            "calls_route" => Some(Self::CallsRoute),
            "messaging" => Some(Self::Messaging),
            "reads_table" => Some(Self::ReadsTable),
            "writes_table" => Some(Self::WritesTable),
            "build_dependency" => Some(Self::BuildDependency),
            "data_flow" => Some(Self::DataFlow),
            "history" => Some(Self::History),
            _ => None,
        }
    }

    /// Returns the closed IR predicate set backing this family.
    ///
    /// Families the first-slice oracle cannot serve return an empty set, so they
    /// expand to no groups instead of fabricated edges.
    #[must_use]
    pub fn predicates(self) -> &'static [RelationPredicate] {
        match self {
            Self::Calls => &[RelationPredicate::Calls],
            Self::CalledBy => &[RelationPredicate::Calls],
            Self::References => &[RelationPredicate::RefersTo],
            Self::Types => &[
                RelationPredicate::UsesType,
                RelationPredicate::ReturnsType,
                RelationPredicate::ParameterType,
            ],
            Self::Implements => &[
                RelationPredicate::Implements,
                RelationPredicate::Satisfies,
                RelationPredicate::Extends,
                RelationPredicate::Embeds,
                RelationPredicate::MixesIn,
                RelationPredicate::Overrides,
            ],
            Self::Imports => &[RelationPredicate::Imports],
            Self::Tests
            | Self::Ownership
            | Self::ServiceCall
            | Self::CallsRoute
            | Self::Messaging
            | Self::ReadsTable
            | Self::WritesTable
            | Self::BuildDependency
            | Self::DataFlow
            | Self::History => &[],
        }
    }

    /// Returns the natural traversal direction relative to the seed.
    #[must_use]
    pub const fn natural_direction(self) -> RelationDirection {
        match self {
            Self::CalledBy => RelationDirection::Inbound,
            _ => RelationDirection::Outbound,
        }
    }
}

/// Prevalidated `symbol.relationships` plan.
#[derive(Debug, Clone)]
pub struct SymbolRelationshipsPlan {
    pub(crate) seeds: BTreeSet<SymbolId>,
    pub(crate) families: Vec<RelationFamily>,
    pub(crate) direction: Option<RelationDirection>,
    pub(crate) min_confidence: u16,
    pub(crate) max_results: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl SymbolRelationshipsPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Prevalidated `flow.trace` plan.
#[derive(Debug, Clone)]
pub struct FlowTracePlan {
    pub(crate) from: SymbolId,
    pub(crate) to: Option<SymbolId>,
    pub(crate) direction: RelationDirection,
    pub(crate) families: Vec<RelationFamily>,
    pub(crate) min_confidence: u16,
    pub(crate) max_depth: u8,
    pub(crate) max_paths: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl FlowTracePlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Prevalidated `architecture.cycles` plan.
#[derive(Debug, Clone)]
pub struct ArchitectureCyclesPlan {
    pub(crate) families: Vec<RelationFamily>,
    pub(crate) min_confidence: u16,
    pub(crate) min_size: u8,
    pub(crate) max_cycles: usize,
    pub(crate) include_self_cycles: bool,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl ArchitectureCyclesPlan {
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

/// One typed relationship target expanded from a seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationshipEdgeTarget {
    /// Stable identity of the related entity.
    pub symbol: SymbolId,
    /// Fixed-point edge confidence from 0 through 1000.
    pub confidence: u16,
    /// Direct immutable source evidence for the edge.
    pub source_refs: Vec<SourceRef>,
}

/// One seed-relation group expanded by a relationships query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelationshipGroup {
    /// Seed symbol that was expanded.
    pub seed: SymbolId,
    /// Relation family for this group.
    pub family: RelationFamily,
    /// Effective direction of the returned edges.
    pub direction: RelationDirection,
    /// Bounded relationship targets in deterministic order.
    pub items: Vec<RelationshipEdgeTarget>,
    /// Qualifying edges known for this group before truncation.
    pub total_count: u32,
}

/// Data returned by a `symbol.relationships` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolRelationshipsResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Seed-relation groups in deterministic order.
    pub groups: Vec<RelationshipGroup>,
    /// Total edges returned across all groups.
    pub returned_edges: u32,
    /// Qualifying edges known before budget limits.
    pub total_edges: u32,
    /// Whether the counts are exact or lower bounds.
    pub exact: bool,
    /// Whether a resource limit stopped complete materialization.
    pub truncated: bool,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// One evidence-bearing edge within a traced path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowTraceEdge {
    /// Relation family that admitted this hop.
    pub family: RelationFamily,
    /// Fixed-point edge confidence from 0 through 1000.
    pub confidence: u16,
    /// Direct immutable source evidence for the edge.
    pub source_refs: Vec<SourceRef>,
}

/// One complete traced path from the source toward a reached node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowTracePath {
    /// Aggregate weakest-link edge confidence from 0 through 1000.
    pub confidence: u16,
    /// Ordered node identifiers along the path.
    pub nodes: Vec<SymbolId>,
    /// Evidence-bearing edges between consecutive nodes.
    pub edges: Vec<FlowTraceEdge>,
    /// Whether this path revisits a node.
    pub cyclic: bool,
}

/// Traversal boundary summary for a `flow.trace` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FlowTraceFrontier {
    /// Distinct nodes entered during traversal.
    pub reached_nodes: u32,
    /// Adjacency edges examined during traversal.
    pub examined_edges: u32,
    /// Whether budget, path cap, or depth stopped complete exploration.
    pub truncated: bool,
    /// Reached nodes with an admissible edge leaving the reached set.
    pub unresolved_boundaries: u32,
}

/// Relation projection actually used by a `flow.trace` query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowTraceProjection {
    /// Relation families included in the traversal, in deterministic order.
    pub families: Vec<RelationFamily>,
    /// Minimum confidence threshold applied.
    pub min_confidence: u16,
}

/// Data returned by a `flow.trace` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowTraceResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Bounded traced paths in deterministic order.
    pub paths: Vec<FlowTracePath>,
    /// Traversal frontier and boundary summary.
    pub frontier: FlowTraceFrontier,
    /// Actual relation projection used.
    pub projection: FlowTraceProjection,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// One strongly connected component detected in the served relation graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleComponent {
    /// Number of member symbols in the component.
    pub size: u32,
    /// Member symbol identifiers in deterministic order.
    pub members: Vec<SymbolId>,
    /// Count of served edges whose endpoints both lie in the component.
    pub internal_edges: u32,
}

/// One bounded representative minimal cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CyclePath {
    /// Ordered node identifiers forming the cycle, first repeated at the end.
    pub nodes: Vec<SymbolId>,
    /// Aggregate weakest-edge confidence from 0 through 1000.
    pub confidence: u16,
    /// Direct immutable source evidence for the cycle edges.
    pub edge_evidence: Vec<SourceRef>,
}

/// One candidate edge for breaking a reported cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CycleBreak {
    /// Source symbol of the break edge.
    pub from: SymbolId,
    /// Target symbol of the break edge.
    pub to: SymbolId,
    /// Relation family that admitted the break edge.
    pub family: RelationFamily,
    /// Heuristic break cost from 0 through 1000; lower confidence is cheaper.
    pub break_cost: u16,
    /// Direct immutable source evidence for the break edge.
    pub source_refs: Vec<SourceRef>,
}

/// Relation projection actually used by an `architecture.cycles` query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureCyclesProjection {
    /// Relation families included in the detection, in deterministic order.
    pub families: Vec<RelationFamily>,
    /// Minimum confidence threshold applied.
    pub min_confidence: u16,
}

/// Data returned by an `architecture.cycles` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureCyclesResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Strongly connected components containing cycles, in deterministic order.
    pub components: Vec<CycleComponent>,
    /// Bounded representative minimal cycles, in deterministic order.
    pub cycles: Vec<CyclePath>,
    /// Ranked candidate break points, in deterministic order.
    pub break_candidates: Vec<CycleBreak>,
    /// Actual relation projection used.
    pub projection: ArchitectureCyclesProjection,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Architecture derived-view categories served by an `architecture.overview`.
///
/// The first slice serves only the structural hotspot ranking as a derived
/// view; the base component and connection model is always file-granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArchitectureOverviewView {
    /// Structural hotspot ranking derived view.
    Hotspots,
}

impl ArchitectureOverviewView {
    /// Returns the stable wire label shared with the MCP view contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hotspots => "hotspots",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "hotspots" => Some(Self::Hotspots),
            _ => None,
        }
    }
}

/// Prevalidated `architecture.overview` plan.
#[derive(Debug, Clone)]
pub struct ArchitectureOverviewPlan {
    pub(crate) views: Vec<ArchitectureOverviewView>,
    pub(crate) min_confidence: u16,
    pub(crate) max_components: usize,
    pub(crate) include_edges: bool,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl ArchitectureOverviewPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One aggregated architecture component keyed by its containing file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureComponent {
    /// Stable component identity derived from the containing file identity.
    pub id: String,
    /// Component kind label; always `file` for this slice.
    pub kind: String,
    /// Repository-controlled display path; always untrusted data.
    pub name: String,
    /// Number of contained symbols.
    pub symbol_count: u32,
    /// Source-free evidence categories supporting the responsibility claim.
    pub responsibility_evidence: Vec<String>,
    /// Aggregate containment confidence from 0 through 1000.
    pub confidence: u16,
}

/// One aggregated typed connection between two architecture components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureConnection {
    /// Source component identity.
    pub from: String,
    /// Target component identity.
    pub to: String,
    /// Relation family admitting the aggregated edges.
    pub kind: RelationFamily,
    /// Aggregated edge count.
    pub weight: u32,
    /// Strongest aggregated edge confidence from 0 through 1000.
    pub confidence: u16,
}

/// One structural hotspot ranking entry for a component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureHotspot {
    /// Component identity.
    pub component_id: String,
    /// Number of incoming connections from distinct components.
    pub fan_in: u32,
    /// Number of outgoing connections to distinct components.
    pub fan_out: u32,
    /// Change-frequency signal; always absent in this slice.
    pub change_frequency: Option<u32>,
    /// Complexity signal; always absent in this slice.
    pub complexity: Option<u32>,
    /// Aggregate hotspot score from 0 through 1000.
    pub score: u16,
}

/// Derived-view algorithm metadata reported by an `architecture.overview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureOverviewDerivedView {
    /// Derived-view category.
    pub view: ArchitectureOverviewView,
    /// Algorithm version identifier.
    pub algorithm_version: String,
}

/// Data returned by an `architecture.overview` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchitectureOverviewResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Aggregated file-granularity components in deterministic order.
    pub components: Vec<ArchitectureComponent>,
    /// Aggregated typed connections between distinct components.
    pub connections: Vec<ArchitectureConnection>,
    /// Structural hotspot rankings in deterministic order.
    pub hotspots: Vec<ArchitectureHotspot>,
    /// Derived-view algorithm metadata in deterministic order.
    pub views: Vec<ArchitectureOverviewDerivedView>,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Test granularity category served by a `tests.select` query.
///
/// The first-slice lexical oracle records a test as an entity kind or flag but
/// cannot distinguish integration, end-to-end, or contract tests, so every
/// detected test entity is honestly reported as a unit-level test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TestsSelectKind {
    /// Unit-level test.
    Unit,
    /// Integration test spanning components.
    Integration,
    /// End-to-end test.
    E2e,
    /// Contract or schema compatibility test.
    Contract,
    /// Static analysis or lint check.
    Static,
    /// Build or compilation verification.
    Build,
}

impl TestsSelectKind {
    /// Returns the stable wire label shared with the MCP test-kind contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Integration => "integration",
            Self::E2e => "e2e",
            Self::Contract => "contract",
            Self::Static => "static",
            Self::Build => "build",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "unit" => Some(Self::Unit),
            "integration" => Some(Self::Integration),
            "e2e" => Some(Self::E2e),
            "contract" => Some(Self::Contract),
            "static" => Some(Self::Static),
            "build" => Some(Self::Build),
            _ => None,
        }
    }
}

/// Prevalidated `tests.select` plan.
#[derive(Debug, Clone)]
pub struct TestsSelectPlan {
    pub(crate) seeds: BTreeSet<SymbolId>,
    pub(crate) test_kinds: Vec<TestsSelectKind>,
    pub(crate) max_tests: usize,
    pub(crate) include_commands: bool,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl TestsSelectPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One ranked test selected for relevance to the seed set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RankedTestSelection {
    /// Stable symbol identity of the test.
    pub test_id: SymbolId,
    /// Test granularity category.
    pub kind: TestsSelectKind,
    /// Repository-controlled display path to the test, when served.
    pub path: Option<String>,
    /// Relevance score from 0 through 1000.
    pub score: u16,
    /// Source-free rationale codes, in deterministic order.
    pub why: Vec<String>,
    /// Estimated execution cost in milliseconds; always absent in this slice.
    pub estimated_cost_ms: Option<u32>,
    /// Inert declarative command hint; only present when requested.
    pub command_hint: Option<String>,
}

/// Coverage signals actually used by a `tests.select` ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TestsSelectCoverage {
    /// Whether direct test-to-seed edges were used.
    pub direct_edges: bool,
    /// Whether transitive dependency signals were used.
    pub transitive_signals: bool,
    /// Whether historical co-change signals were used; always false here.
    pub history_signals: bool,
    /// Whether file co-location with a seed was used.
    pub build_target_signals: bool,
}

/// One honest gap where a seed scope has no related test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestsSelectGap {
    /// Seed scope identifier with no related test.
    pub scope: String,
    /// Source-free reason code.
    pub reason: String,
}

/// Data returned by a `tests.select` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestsSelectResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Ranked tests in deterministic order.
    pub tests: Vec<RankedTestSelection>,
    /// Coverage signals actually used by the ranking.
    pub coverage_strategy: TestsSelectCoverage,
    /// Honest coverage gaps in deterministic order.
    pub gaps: Vec<TestsSelectGap>,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Classification of one resolved change span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeImpactClassification {
    /// A public API surface was modified.
    Surface,
    /// An internal implementation body was modified.
    Body,
    /// A new entity was added.
    Added,
    /// An entity was removed.
    Removed,
    /// An entity was renamed or moved.
    Renamed,
}

impl ChangeImpactClassification {
    /// Returns the stable wire label shared with the MCP classification contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Body => "body",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Renamed => "renamed",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "surface" => Some(Self::Surface),
            "body" => Some(Self::Body),
            "added" => Some(Self::Added),
            "removed" => Some(Self::Removed),
            "renamed" => Some(Self::Renamed),
            _ => None,
        }
    }
}

/// Aggregate risk level for a `change.impact` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeImpactRiskLevel {
    /// No measurable risk.
    None,
    /// Low risk, local changes only.
    Low,
    /// Medium risk, some cross-module effects.
    Medium,
    /// High risk, public surface affected.
    High,
    /// Critical risk, public surface with wide fanout.
    Critical,
}

impl ChangeImpactRiskLevel {
    /// Returns the stable wire label shared with the MCP risk-level contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// Prevalidated `change.impact` plan.
#[derive(Debug, Clone)]
pub struct ChangeImpactPlan {
    pub(crate) changed_symbols: BTreeSet<SymbolId>,
    pub(crate) changed_paths: Vec<String>,
    pub(crate) max_depth: u8,
    pub(crate) min_confidence: u16,
    pub(crate) include_tests: bool,
    pub(crate) max_dependents: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl ChangeImpactPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One resolved change from the input change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedChangeRecord {
    /// Stable symbol identity, when the change maps to a known symbol.
    pub symbol_id: Option<SymbolId>,
    /// File identity for the changed span, when resolved.
    pub file_id: Option<FileId>,
    /// Classification of the change.
    pub classification: ChangeImpactClassification,
    /// Entity kind label of the affected symbol, when resolved.
    pub kind: Option<String>,
}

/// One impacted dependent with path rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactEntryRecord {
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind label of the affected symbol.
    pub kind: String,
    /// Transitive distance from the change, 1 through 8.
    pub distance: u8,
    /// Confidence in the impact path, 0 through 1000.
    pub confidence: u16,
    /// Relation predicate labels forming the impact path, in deterministic order.
    pub via: Vec<String>,
    /// Whether this dependent is a public surface.
    pub is_public: bool,
}

/// One grouped set of impacted dependents for a resolved change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImpactGroupRecord {
    /// Index of the originating resolved change.
    pub source_index: u16,
    /// Ranked dependents in deterministic order.
    pub dependents: Vec<ImpactEntryRecord>,
}

/// One test candidate relevant to the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeImpactTestCandidate {
    /// Stable test identity label.
    pub test_id: String,
    /// Relevance score, 0 through 1000.
    pub relevance: u16,
    /// Source-free rationale codes, in deterministic order.
    pub why: Vec<String>,
    /// Estimated execution cost in milliseconds; always absent in this slice.
    pub estimated_cost_ms: Option<u32>,
}

/// Aggregate risk summary for a `change.impact` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeImpactRiskSummary {
    /// Aggregate risk level.
    pub level: ChangeImpactRiskLevel,
    /// Source-free reason codes, in deterministic order.
    pub reasons: Vec<String>,
    /// Coverage status of the impact analysis.
    pub coverage: CoverageStatus,
    /// Whether public surface was changed.
    pub breaking_surface: bool,
    /// Total transitive fanout count.
    pub fanout: u32,
    /// Whether dynamic or reflection-based relations create blind spots.
    pub dynamic_blind_spots: bool,
}

/// Data returned by a `change.impact` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeImpactResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Resolved changes from the input change set, in deterministic order.
    pub resolved_changes: Vec<ResolvedChangeRecord>,
    /// Ranked impact groups, one per resolved change.
    pub impacted: Vec<ImpactGroupRecord>,
    /// Test candidates when requested, in deterministic order.
    pub tests: Vec<ChangeImpactTestCandidate>,
    /// Aggregate risk summary.
    pub risk_summary: ChangeImpactRiskSummary,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Objective class for a `plan.change` plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanChangeObjective {
    /// Fix a defect.
    BugFix,
    /// Restructure without behavior change.
    Refactor,
    /// Explain or document existing behavior.
    Explanation,
    /// Migrate to a new API, platform, or dependency.
    Migration,
    /// Review and assess existing code.
    Review,
}

impl PlanChangeObjective {
    /// Returns the stable wire label shared with the MCP plan-objective contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BugFix => "bug_fix",
            Self::Refactor => "refactor",
            Self::Explanation => "explanation",
            Self::Migration => "migration",
            Self::Review => "review",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "bug_fix" => Some(Self::BugFix),
            "refactor" => Some(Self::Refactor),
            "explanation" => Some(Self::Explanation),
            "migration" => Some(Self::Migration),
            "review" => Some(Self::Review),
            _ => None,
        }
    }
}

/// Prevalidated `plan.change` plan.
#[derive(Debug, Clone)]
pub struct PlanChangePlan {
    pub(crate) objective: PlanChangeObjective,
    pub(crate) target_symbols: BTreeSet<SymbolId>,
    pub(crate) target_files: BTreeSet<FileId>,
    pub(crate) max_steps: usize,
    pub(crate) max_depth: u8,
    pub(crate) max_dependents: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl PlanChangePlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One ordered step in a `plan.change` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChangeStepRecord {
    /// One-based step ordinal.
    pub step: u8,
    /// Source-free action description.
    pub action: String,
    /// Target symbol identities for this step, in deterministic order.
    pub targets: Vec<SymbolId>,
    /// Step ordinals this step depends on, in deterministic order.
    pub depends_on: Vec<u8>,
    /// Source-free risk codes for this step, in deterministic order.
    pub risks: Vec<String>,
    /// Source-free verification hint, when one applies.
    pub verification: Option<String>,
}

/// Compact impact and ownership summary for a `plan.change` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChangeImpactSummary {
    /// Total affected symbol count.
    pub affected_symbols: u32,
    /// Total affected file count.
    pub affected_files: u32,
    /// Aggregate risk level.
    pub risk_level: ChangeImpactRiskLevel,
    /// Whether public surface is affected.
    pub touches_public_surface: bool,
}

/// One open decision that cannot be safely inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChangeDecision {
    /// Source-free question identifier.
    pub question: String,
    /// Recommended default choice.
    pub recommended_default: String,
}

/// Ready follow-up context-pack arguments for a `plan.change` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChangeContextPack {
    /// Symbol identities to include in the context pack, in deterministic order.
    pub symbols: Vec<SymbolId>,
    /// File identities to include in the context pack, in deterministic order.
    pub files: Vec<FileId>,
}

/// Data returned by a `plan.change` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanChangeResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Ordered plan steps in deterministic order.
    pub plan: Vec<PlanChangeStepRecord>,
    /// Compact impact and ownership summary.
    pub affected_scope: PlanChangeImpactSummary,
    /// Ranked verification test plan, in deterministic order.
    pub test_plan: Vec<ChangeImpactTestCandidate>,
    /// Open decisions requiring user input, in deterministic order.
    pub open_decisions: Vec<PlanChangeDecision>,
    /// Ready follow-up context-pack arguments.
    pub context_pack_request: PlanChangeContextPack,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Change category filter for a `history.compare` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HistoryChangeKind {
    /// Entity additions and removals.
    Entities,
    /// Signature modifications.
    Signatures,
    /// Relation additions and removals.
    Relations,
    /// Architectural boundary changes.
    Architecture,
    /// Ownership changes.
    Ownership,
    /// Test additions, removals, or modifications.
    Tests,
    /// Route or endpoint changes.
    Routes,
    /// Data schema changes.
    Data,
}

impl HistoryChangeKind {
    /// Returns the stable wire label shared with the MCP change-kind contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Entities => "entities",
            Self::Signatures => "signatures",
            Self::Relations => "relations",
            Self::Architecture => "architecture",
            Self::Ownership => "ownership",
            Self::Tests => "tests",
            Self::Routes => "routes",
            Self::Data => "data",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "entities" => Some(Self::Entities),
            "signatures" => Some(Self::Signatures),
            "relations" => Some(Self::Relations),
            "architecture" => Some(Self::Architecture),
            "ownership" => Some(Self::Ownership),
            "tests" => Some(Self::Tests),
            "routes" => Some(Self::Routes),
            "data" => Some(Self::Data),
            _ => None,
        }
    }
}

/// Kind of semantic change detected between two generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HistorySemanticChangeKind {
    /// A new entity was added.
    Added,
    /// An entity was removed.
    Removed,
    /// An entity body or location was modified without a kind change.
    Modified,
    /// An entity kind or signature span was modified.
    SignatureModified,
    /// A relation was added or removed.
    RelationChanged,
}

impl HistorySemanticChangeKind {
    /// Returns the stable wire label shared with the MCP semantic-change contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
            Self::SignatureModified => "signature_modified",
            Self::RelationChanged => "relation_changed",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "added" => Some(Self::Added),
            "removed" => Some(Self::Removed),
            "modified" => Some(Self::Modified),
            "signature_modified" => Some(Self::SignatureModified),
            "relation_changed" => Some(Self::RelationChanged),
            _ => None,
        }
    }
}

/// Prevalidated `history.compare` plan.
///
/// The head generation is the plan's pinned generation; the base generation is
/// carried explicitly so the result can name the resolved state pair.
#[derive(Debug, Clone)]
pub struct HistoryComparePlan {
    pub(crate) base_generation: GenerationId,
    pub(crate) change_kinds: BTreeSet<HistoryChangeKind>,
    pub(crate) max_results: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl HistoryComparePlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One semantic change between two generations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticChangeRecord {
    /// Kind of semantic change.
    pub kind: HistorySemanticChangeKind,
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind label of the affected symbol.
    pub entity_kind: String,
    /// Whether this change is a breaking candidate.
    pub breaking_candidate: bool,
    /// Significance rank, 0 through 1000.
    pub significance: u16,
}

/// Aggregate architecture delta between two generations.
///
/// This slice models no service or component-boundary graph, so every field is
/// an honest zero rather than a fabricated delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HistoryArchitectureDelta {
    /// Number of new cross-service edges; always zero in this slice.
    pub new_cross_service_edges: u32,
    /// Number of removed cross-service edges; always zero in this slice.
    pub removed_cross_service_edges: u32,
    /// Number of new component boundaries; always zero in this slice.
    pub new_boundaries: u32,
    /// Number of removed component boundaries; always zero in this slice.
    pub removed_boundaries: u32,
}

/// One breaking-change candidate with consumer evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BreakingCandidateRecord {
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Number of known consumers in the base generation.
    pub consumer_count: u32,
    /// Whether the symbol is part of a public API surface.
    pub is_public_surface: bool,
    /// Source-free reason code.
    pub reason: String,
}

/// One lineage match between base and head entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageMatchRecord {
    /// Base symbol identity.
    pub base_symbol_id: SymbolId,
    /// Head symbol identity.
    pub head_symbol_id: SymbolId,
    /// Match confidence, 0 through 1000.
    pub confidence: u16,
    /// Whether this is a rename rather than identity preservation.
    pub is_rename: bool,
}

/// Data returned by a `history.compare` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryCompareResult {
    /// Resolved base generation.
    pub base_generation: GenerationId,
    /// Resolved head generation that served the query.
    pub head_generation: GenerationId,
    /// Coverage of the comparison.
    pub coverage: CoverageStatus,
    /// Semantic changes in significance order.
    pub changes: Vec<SemanticChangeRecord>,
    /// Aggregate architecture delta.
    pub architecture_delta: HistoryArchitectureDelta,
    /// Breaking-change candidates in significance order.
    pub breaking_candidates: Vec<BreakingCandidateRecord>,
    /// Entity lineage matches in deterministic identity order.
    pub lineage: Vec<LineageMatchRecord>,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
}

/// Entry-point model policy for a `code.dead` reachability query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodeDeadEntryPointPolicy {
    /// Standard mixed entry-point model.
    Standard,
    /// Library export surface as entry points.
    Library,
    /// Application main and registered handlers as entry points.
    Application,
}

impl CodeDeadEntryPointPolicy {
    /// Returns the stable wire label shared with the MCP policy contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Library => "library",
            Self::Application => "application",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "standard" => Some(Self::Standard),
            "library" => Some(Self::Library),
            "application" => Some(Self::Application),
            _ => None,
        }
    }
}

/// Reachability classification for one dead-code candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeadCodeClassification {
    /// Proven unreachable under the entry-point model: no incoming edges.
    ProvenDead,
    /// Probable dead code: unreached with confident incoming edges.
    ProbableDead,
    /// Suspected dead code: unreached with weak incoming edges.
    SuspectedDead,
}

impl DeadCodeClassification {
    /// Returns the stable wire label shared with the MCP classification contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvenDead => "proven_dead",
            Self::ProbableDead => "probable_dead",
            Self::SuspectedDead => "suspected_dead",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "proven_dead" => Some(Self::ProvenDead),
            "probable_dead" => Some(Self::ProbableDead),
            "suspected_dead" => Some(Self::SuspectedDead),
            _ => None,
        }
    }
}

/// Prevalidated `code.dead` plan.
#[derive(Debug, Clone)]
pub struct CodeDeadPlan {
    pub(crate) entry_point_policy: CodeDeadEntryPointPolicy,
    pub(crate) include_exported: bool,
    pub(crate) include_tests: bool,
    pub(crate) min_confidence: u16,
    pub(crate) max_candidates: usize,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl CodeDeadPlan {
    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// One dead-code candidate detected by reachability analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeadCodeCandidate {
    /// Stable symbol identity of the candidate.
    pub symbol_id: SymbolId,
    /// Reachability classification.
    pub classification: DeadCodeClassification,
    /// Classification confidence from 0 through 1000.
    pub confidence: u16,
    /// Source-free reasons supporting the classification, in deterministic order.
    pub why: Vec<String>,
    /// Suppression rules checked for this candidate, in deterministic order.
    pub suppressions_checked: Vec<String>,
    /// Direct immutable source evidence for the candidate definition.
    pub source_refs: Vec<SourceRef>,
}

/// Summary of the entry-point model used for reachability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CodeDeadEntryPointSummary {
    /// Policy used for entry-point resolution.
    pub policy: CodeDeadEntryPointPolicy,
    /// Number of resolved entry points.
    pub entry_point_count: u32,
    /// Whether the model is complete for the scope.
    pub complete: bool,
}

/// One known blind spot in the reachability analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeDeadBlindSpot {
    /// Source-free blind-spot category label.
    pub category: String,
    /// Number of symbols potentially affected.
    pub affected_count: u32,
}

/// One applied false-positive suppression rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeDeadSuppressionRule {
    /// Rule identifier or annotation pattern.
    pub rule: String,
    /// Number of symbols suppressed by this rule.
    pub suppressed_count: u32,
}

/// Data returned by a `code.dead` plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeDeadResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Ranked dead-code candidates in deterministic order.
    pub candidates: Vec<DeadCodeCandidate>,
    /// Entry-point model summary.
    pub entry_points: CodeDeadEntryPointSummary,
    /// Known analysis blind spots in deterministic order.
    pub blind_spots: Vec<CodeDeadBlindSpot>,
    /// Applied false-positive suppression rules in deterministic order.
    pub suppression_rules: Vec<CodeDeadSuppressionRule>,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
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

/// Default maximum rows returned by an advanced query.
pub const ADVANCED_DEFAULT_MAX_RESULTS: usize = 100;
/// Hard ceiling on rows a single advanced query may return.
pub const ADVANCED_MAX_RESULTS: usize = 1_000;
/// Hard ceiling on advanced query AST nesting depth.
pub const ADVANCED_MAX_DEPTH: usize = 5;
/// Default maximum traversal or plan depth.
pub const ADVANCED_DEFAULT_MAX_DEPTH: usize = 3;
/// Hard ceiling on traversal facts an advanced query may examine.
pub const ADVANCED_MAX_TRAVERSAL: usize = 100_000;
/// Hard ceiling on the static advanced query cost estimate.
pub const ADVANCED_MAX_ESTIMATED_COST: u64 = 1_000_000;

/// Allow-listed advanced query operators.
///
/// Only these operators can appear in a valid advanced query AST. The grammar
/// structurally excludes SQL, Cypher, shell, arbitrary regex, arbitrary code,
/// and unbounded recursion. This is the query-layer mirror of the public
/// contract operator catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedOperator {
    /// Full entity scan with an optional kind filter.
    Scan,
    /// Predicate-based row filtering.
    Filter,
    /// Column selection.
    Project,
    /// Inner join on typed equality.
    Join,
    /// Count, sum, min, max aggregation.
    Aggregate,
    /// Bounded graph traversal along typed edges.
    Traverse,
    /// Deterministic ordering by typed keys.
    Sort,
    /// Row count limitation.
    Limit,
}

impl AdvancedOperator {
    /// Base cost weight for static estimation.
    #[must_use]
    pub const fn base_cost(self) -> u64 {
        match self {
            Self::Scan => 100,
            Self::Filter => 10,
            Self::Project => 5,
            Self::Join => 500,
            Self::Aggregate => 50,
            Self::Traverse => 200,
            Self::Sort => 20,
            Self::Limit => 1,
        }
    }

    /// Stable display name used in plan explanations.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "Scan",
            Self::Filter => "Filter",
            Self::Project => "Project",
            Self::Join => "Join",
            Self::Aggregate => "Aggregate",
            Self::Traverse => "Traverse",
            Self::Sort => "Sort",
            Self::Limit => "Limit",
        }
    }

    /// Parses a stable operator label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "scan" => Some(Self::Scan),
            "filter" => Some(Self::Filter),
            "project" => Some(Self::Project),
            "join" => Some(Self::Join),
            "aggregate" => Some(Self::Aggregate),
            "traverse" => Some(Self::Traverse),
            "sort" => Some(Self::Sort),
            "limit" => Some(Self::Limit),
            _ => None,
        }
    }
}

/// Closed entity kind understood by the advanced query scan operator.
///
/// This is the query-layer mirror of the public contract entity kind. Each
/// variant maps to a closed set of normalized IR entity kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedEntityKind {
    /// Source file.
    File,
    /// Namespace or module.
    Module,
    /// Type declaration.
    Type,
    /// Function declaration.
    Function,
    /// Method declaration.
    Method,
    /// Field declaration.
    Field,
    /// Constant declaration.
    Constant,
    /// Variable declaration.
    Variable,
    /// Configuration record.
    Configuration,
}

impl AdvancedEntityKind {
    /// Stable wire label shared with the MCP entity-kind contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Type => "type",
            Self::Function => "function",
            Self::Method => "method",
            Self::Field => "field",
            Self::Constant => "constant",
            Self::Variable => "variable",
            Self::Configuration => "configuration",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "type" => Some(Self::Type),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "field" => Some(Self::Field),
            "constant" => Some(Self::Constant),
            "variable" => Some(Self::Variable),
            "configuration" => Some(Self::Configuration),
            _ => None,
        }
    }

    /// Whether a normalized IR entity kind belongs to this advanced kind.
    #[must_use]
    pub fn matches_ir(self, kind: IrEntityKind) -> bool {
        match self {
            Self::File => matches!(kind, IrEntityKind::File),
            Self::Module => matches!(
                kind,
                IrEntityKind::Module | IrEntityKind::Namespace | IrEntityKind::Package
            ),
            Self::Type => matches!(
                kind,
                IrEntityKind::Class
                    | IrEntityKind::Struct
                    | IrEntityKind::Enum
                    | IrEntityKind::Union
                    | IrEntityKind::TypeAlias
                    | IrEntityKind::Trait
                    | IrEntityKind::Interface
                    | IrEntityKind::Protocol
                    | IrEntityKind::TypeParameter
            ),
            Self::Function => matches!(
                kind,
                IrEntityKind::Function | IrEntityKind::Constructor | IrEntityKind::Closure
            ),
            Self::Method => matches!(kind, IrEntityKind::Method),
            Self::Field => matches!(kind, IrEntityKind::Field | IrEntityKind::Property),
            Self::Constant => matches!(kind, IrEntityKind::Constant),
            Self::Variable => matches!(kind, IrEntityKind::Variable | IrEntityKind::Parameter),
            Self::Configuration => matches!(kind, IrEntityKind::ConfigurationKey),
        }
    }
}

/// Typed scalar or identifier value bound as an advanced query predicate operand.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvancedValue {
    /// UTF-8 text literal.
    Text(String),
    /// Signed 64-bit integer literal.
    Integer(i64),
    /// Boolean literal.
    Boolean(bool),
    /// Stable symbol identifier.
    Symbol(SymbolId),
    /// Stable file identifier.
    File(FileId),
}

/// Allow-listed predicate operators for advanced query filter expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "pred", rename_all = "snake_case")]
pub enum AdvancedPredicate {
    /// Field equals a bound value.
    Equals {
        /// Column or field name to test.
        field: String,
        /// Expected value.
        value: AdvancedValue,
    },
    /// Field does not equal a bound value.
    NotEquals {
        /// Column or field name to test.
        field: String,
        /// Value to exclude.
        value: AdvancedValue,
    },
    /// Field value is contained in a bounded set.
    In {
        /// Column or field name to test.
        field: String,
        /// Bounded set of allowed values.
        values: Vec<AdvancedValue>,
    },
    /// Logical conjunction of bounded predicates.
    And {
        /// Predicates that must all hold.
        predicates: Vec<AdvancedPredicate>,
    },
    /// Logical disjunction of bounded predicates.
    Or {
        /// Predicates of which at least one must hold.
        predicates: Vec<AdvancedPredicate>,
    },
}

/// Traversal relation kinds permitted by the advanced query AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedRelationKind {
    /// Direct call edges.
    Calls,
    /// Reverse call edges.
    CalledBy,
    /// Import or use edges.
    Imports,
    /// Reverse import edges.
    ImportedBy,
    /// Test-to-subject edges.
    Tests,
    /// Subject-to-test edges.
    TestedBy,
    /// Containment or module membership.
    Contains,
    /// Reverse containment.
    ContainedBy,
    /// Trait or interface implementation edges.
    Implements,
    /// Reverse implementation edges.
    ImplementedBy,
    /// General reference edges.
    References,
    /// Reverse reference edges.
    ReferencedBy,
}

impl AdvancedRelationKind {
    /// Stable wire label shared with the MCP relation-kind contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::CalledBy => "called_by",
            Self::Imports => "imports",
            Self::ImportedBy => "imported_by",
            Self::Tests => "tests",
            Self::TestedBy => "tested_by",
            Self::Contains => "contains",
            Self::ContainedBy => "contained_by",
            Self::Implements => "implements",
            Self::ImplementedBy => "implemented_by",
            Self::References => "references",
            Self::ReferencedBy => "referenced_by",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "calls" => Some(Self::Calls),
            "called_by" => Some(Self::CalledBy),
            "imports" => Some(Self::Imports),
            "imported_by" => Some(Self::ImportedBy),
            "tests" => Some(Self::Tests),
            "tested_by" => Some(Self::TestedBy),
            "contains" => Some(Self::Contains),
            "contained_by" => Some(Self::ContainedBy),
            "implements" => Some(Self::Implements),
            "implemented_by" => Some(Self::ImplementedBy),
            "references" => Some(Self::References),
            "referenced_by" => Some(Self::ReferencedBy),
            _ => None,
        }
    }
}

/// Traversal direction for advanced query graph navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedTraverseDirection {
    /// Follow edges toward callers or importers.
    Inbound,
    /// Follow edges toward callees or dependencies.
    Outbound,
    /// Follow edges in both directions.
    Both,
}

/// Allow-listed aggregate functions for the advanced query AST.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fn", rename_all = "snake_case")]
pub enum AdvancedAggregateFunction {
    /// Count rows in each group.
    Count,
    /// Sum a numeric field per group.
    Sum {
        /// Numeric field to sum.
        field: String,
    },
    /// Minimum of a comparable field per group.
    Min {
        /// Field to minimize.
        field: String,
    },
    /// Maximum of a comparable field per group.
    Max {
        /// Field to maximize.
        field: String,
    },
}

/// One sort directive for the advanced query sort operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedSortKey {
    /// Column or field name to sort by.
    pub field: String,
    /// Whether to sort ascending or descending.
    pub descending: bool,
}

/// Typed declarative advanced query AST node.
///
/// This is the query-layer mirror of the public contract AST. It is bounded and
/// allow-listed; SQL strings, Cypher text, shell fragments, arbitrary regex,
/// arbitrary code, and unbounded recursion are forbidden. The serde
/// representation is wire-compatible with the contract AST so a normalized
/// request can cross the daemon boundary unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AdvancedAstNode {
    /// Base scan over entities of a given kind.
    Scan {
        /// Entity kind to scan.
        entity: AdvancedEntityKind,
        /// Optional filter applied during the scan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<Box<AdvancedPredicate>>,
    },
    /// Filter rows from an input node by a bounded predicate.
    Filter {
        /// Input node producing rows to filter.
        input: Box<AdvancedAstNode>,
        /// Predicate that rows must satisfy.
        predicate: AdvancedPredicate,
    },
    /// Project a bounded set of columns from an input node.
    Project {
        /// Input node producing rows to project.
        input: Box<AdvancedAstNode>,
        /// Column names to retain.
        columns: Vec<String>,
    },
    /// Join two input nodes on a shared key column.
    Join {
        /// Left input node.
        left: Box<AdvancedAstNode>,
        /// Right input node.
        right: Box<AdvancedAstNode>,
        /// Column name to join on.
        on: String,
    },
    /// Aggregate rows from an input node by grouping keys.
    Aggregate {
        /// Input node producing rows to aggregate.
        input: Box<AdvancedAstNode>,
        /// Column names to group by.
        group_by: Vec<String>,
        /// Aggregate functions to compute per group.
        aggregations: Vec<AdvancedAggregateFunction>,
    },
    /// Traverse graph edges from a seed symbol or bound column.
    Traverse {
        /// Seed symbol identifier for the traversal origin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<SymbolId>,
        /// Column name providing seed identifiers from the input node.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed_from: Option<String>,
        /// Relation kind to traverse.
        relation: AdvancedRelationKind,
        /// Traversal direction.
        direction: AdvancedTraverseDirection,
        /// Maximum traversal depth, hard ceiling five.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_depth: Option<u8>,
    },
    /// Sort rows from an input node by bounded keys.
    Sort {
        /// Input node producing rows to sort.
        input: Box<AdvancedAstNode>,
        /// Sort directives applied in order.
        by: Vec<AdvancedSortKey>,
    },
    /// Limit the number of rows from an input node.
    Limit {
        /// Input node producing rows to limit.
        input: Box<AdvancedAstNode>,
        /// Maximum rows to return.
        max_rows: u16,
    },
}

impl AdvancedAstNode {
    /// Derives the operator sequence (innermost first) and the nesting depth.
    #[must_use]
    pub fn derive_plan_shape(&self) -> (Vec<AdvancedOperator>, usize) {
        let mut operators = Vec::new();
        let depth = self.collect_operators(&mut operators);
        (operators, depth)
    }

    fn collect_operators(&self, operators: &mut Vec<AdvancedOperator>) -> usize {
        match self {
            Self::Scan { .. } => {
                operators.push(AdvancedOperator::Scan);
                1
            }
            Self::Filter { input, .. } => {
                let depth = input.collect_operators(operators);
                operators.push(AdvancedOperator::Filter);
                depth + 1
            }
            Self::Project { input, .. } => {
                let depth = input.collect_operators(operators);
                operators.push(AdvancedOperator::Project);
                depth + 1
            }
            Self::Join { left, right, .. } => {
                let left_depth = left.collect_operators(operators);
                let right_depth = right.collect_operators(operators);
                operators.push(AdvancedOperator::Join);
                left_depth.max(right_depth) + 1
            }
            Self::Aggregate { input, .. } => {
                let depth = input.collect_operators(operators);
                operators.push(AdvancedOperator::Aggregate);
                depth + 1
            }
            Self::Traverse { .. } => {
                operators.push(AdvancedOperator::Traverse);
                1
            }
            Self::Sort { input, .. } => {
                let depth = input.collect_operators(operators);
                operators.push(AdvancedOperator::Sort);
                depth + 1
            }
            Self::Limit { input, .. } => {
                let depth = input.collect_operators(operators);
                operators.push(AdvancedOperator::Limit);
                depth + 1
            }
        }
    }
}

/// Prevalidated `query.advanced` plan.
///
/// The plan captures the safe AST, the derived operator sequence, the static
/// cost estimate, and the resolved limits. Execution serves only the supported
/// operator subset; everything else yields an honest unsupported result.
#[derive(Debug, Clone)]
pub struct AdvancedQueryPlan {
    pub(crate) ast: AdvancedAstNode,
    pub(crate) operators: Vec<AdvancedOperator>,
    pub(crate) max_rows: usize,
    pub(crate) max_traversal: usize,
    pub(crate) depth: usize,
    pub(crate) estimated_cost: u64,
    pub(crate) explain: bool,
    pub(crate) budget: QueryBudget,
    pub(crate) explanation: PlanExplanation,
}

impl AdvancedQueryPlan {
    /// Validates an operator sequence and limits, returning the static cost.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::PlanRejected`] when the operator list is empty, the
    /// depth exceeds the hard ceiling, the row limit is out of range, the
    /// traversal bound is exceeded, or the static cost estimate is too large.
    pub fn validate(
        operators: &[AdvancedOperator],
        max_rows: usize,
        max_traversal: usize,
        depth: usize,
    ) -> Result<u64, QueryError> {
        if operators.is_empty() {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if depth > ADVANCED_MAX_DEPTH {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_rows == 0 || max_rows > ADVANCED_MAX_RESULTS {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        if max_traversal > ADVANCED_MAX_TRAVERSAL {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Edges,
            });
        }
        let estimated_cost = operators
            .iter()
            .fold(0u64, |acc, op| acc.saturating_add(op.base_cost()))
            .saturating_mul(u64::try_from(max_rows).unwrap_or(u64::MAX) / 100 + 1);
        if estimated_cost > ADVANCED_MAX_ESTIMATED_COST {
            return Err(QueryError::PlanRejected {
                resource: QueryResource::Results,
            });
        }
        Ok(estimated_cost)
    }

    /// Whether a static cost estimate fits an optional client cost limit.
    ///
    /// An absent limit always admits the estimate.
    #[must_use]
    pub fn admits_cost(estimated_cost: u64, cost_limit: Option<u64>) -> bool {
        match cost_limit {
            Some(limit) => estimated_cost <= limit,
            None => true,
        }
    }

    /// Returns the deterministic plan explanation.
    #[must_use]
    pub const fn explanation(&self) -> &PlanExplanation {
        &self.explanation
    }
}

/// Supported column types in advanced query results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedColumnType {
    /// Stable symbol identifier.
    SymbolId,
    /// Stable file identifier.
    FileId,
    /// UTF-8 text.
    Text,
    /// Signed 64-bit integer.
    Integer,
    /// Boolean.
    Boolean,
    /// Repository-relative path.
    Path,
}

impl AdvancedColumnType {
    /// Stable wire label shared with the MCP column-type contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SymbolId => "symbol_id",
            Self::FileId => "file_id",
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::Path => "path",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "symbol_id" => Some(Self::SymbolId),
            "file_id" => Some(Self::FileId),
            "text" => Some(Self::Text),
            "integer" => Some(Self::Integer),
            "boolean" => Some(Self::Boolean),
            "path" => Some(Self::Path),
            _ => None,
        }
    }
}

/// Typed column definition in an advanced query result schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvancedColumnSchema {
    /// Stable column name.
    pub name: String,
    /// Column type descriptor.
    pub column_type: AdvancedColumnType,
}

/// Explainable cost and plan for an advanced query execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvancedPlanExplanation {
    /// Estimated total cost units for the query plan.
    pub estimated_cost: u64,
    /// Ordered operator names in the physical plan.
    pub operators: Vec<String>,
    /// Applied limit descriptions.
    pub applied_limits: Vec<String>,
}

/// Completeness classification for an advanced query result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AdvancedCompleteness {
    /// All matching rows were returned.
    Complete,
    /// Result is safely pageable with a continuation cursor.
    Paged,
    /// Result was truncated by a hard limit.
    Truncated,
    /// Query pattern is not supported in this slice.
    Unsupported,
}

impl AdvancedCompleteness {
    /// Stable wire label shared with the MCP completeness contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Paged => "paged",
            Self::Truncated => "truncated",
            Self::Unsupported => "unsupported",
        }
    }

    /// Parses a stable wire label.
    #[must_use]
    pub fn from_label(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "paged" => Some(Self::Paged),
            "truncated" => Some(Self::Truncated),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }
}

/// Data returned by a `query.advanced` plan.
///
/// Rows are repository-controlled JSON objects keyed by column name. The
/// `plan` is present only when an explanation was requested. Completeness is
/// honest: unsupported patterns return non-empty columns and empty rows rather
/// than fabricated data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AdvancedQueryResult {
    /// Immutable generation that served the query.
    pub generation: GenerationId,
    /// Stable typed column definitions for the result rows.
    pub columns: Vec<AdvancedColumnSchema>,
    /// Typed result rows as JSON objects keyed by column name.
    pub rows: Vec<serde_json::Value>,
    /// Operators, estimates, and applied limits when explain was requested.
    pub plan: Option<AdvancedPlanExplanation>,
    /// Whether the result is complete, paged, truncated, or unsupported.
    pub completeness: AdvancedCompleteness,
    /// Resource limits that stopped work, in deterministic execution order.
    pub limiting_resources: Vec<QueryResource>,
    /// Mandatory trust marker for repository-controlled values.
    pub trust: RepositoryDataTrust,
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
