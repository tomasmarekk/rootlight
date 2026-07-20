//! Strict typed schemas for context assembly, batch queries, and advanced queries.
//!
//! These types define the bounded public MCP contract for `context.pack`,
//! `query.batch`, and `query.advanced`, matching the normative agent interface
//! specification. All repository-derived content is classified as untrusted
//! data; server-generated guidance is kept structurally separate and source-free.

use std::collections::BTreeMap;

use rootlight_error::SafeLabel;
use rootlight_ids::{FileId, GenerationId, SymbolId};
use rootlight_ir::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::TrustClassification;
use crate::vertical::{
    ContinuationCursor, EntityKind, GenerationSelector, ReadEnvelope, RepositorySelector,
    RequiredNullable, ResponseBudget, ResponseProfile, ResponseWarning, SourceFreeMessage,
    ToolResponse, UsageSummary,
};

// ---------------------------------------------------------------------------
// context.pack
// ---------------------------------------------------------------------------

/// Seed selector that anchors a context pack to one or more starting points.
///
/// At least one seed kind must be supplied; the router rejects an empty
/// selector. Handles refer to prior bounded results and stay opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextSeedSelector {
    /// Stable symbol identifiers to anchor the pack.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub symbols: Option<Vec<SymbolId>>,
    /// Repository-relative paths to anchor the pack.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32), inner(length(min = 1, max = 4096)))]
    pub paths: Option<Vec<String>>,
    /// Service or route names to anchor the pack.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32), inner(length(min = 1, max = 4096)))]
    pub routes: Option<Vec<String>>,
    /// Stable test symbol identifiers to anchor the pack.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub tests: Option<Vec<SymbolId>>,
    /// Opaque handle to a prior located result set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub located: Option<ContinuationCursor>,
    /// Revision or change-set descriptor.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub change: Option<String>,
    /// Opaque handle to a prior change plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub plan: Option<String>,
}

/// How much source detail the assembled pack may include.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourcePolicy {
    /// Only stable references, no source text.
    ReferencesOnly,
    /// Symbol signatures without bodies.
    Signatures,
    /// Small focused snippets around the evidence.
    FocusedSnippets,
    /// Fuller evidence snippets up to the source budget.
    EvidenceHeavy,
}

/// Evidence sections a pack may assemble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextSection {
    /// Module, layer, or service boundaries.
    Architecture,
    /// Primary symbol definitions.
    Definitions,
    /// Caller evidence.
    Callers,
    /// Callee evidence.
    Callees,
    /// Type and signature evidence.
    Types,
    /// Covering tests.
    Tests,
    /// Recent change history.
    History,
    /// Source snippets.
    Source,
    /// Risk signals.
    Risks,
}

/// Diversity bias applied when ranking evidence under a tight budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Diversity {
    /// Balance across all relevant roles.
    Balanced,
    /// Favor implementation evidence.
    Implementation,
    /// Favor test evidence.
    Tests,
    /// Favor change-impact evidence.
    Impact,
    /// Favor architecture evidence.
    Architecture,
}

/// Strict input for `context.pack`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextPackInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Immutable generation to pin evidence resolution; defaults to active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Specific coding, review, debugging, or refactoring objective.
    #[schemars(length(min = 1, max = 4096))]
    pub task: String,
    /// Starting points that anchor the evidence pack.
    pub seeds: ContextSeedSelector,
    /// Maximum estimated output tokens (minimum 500, hard maximum 20000).
    #[schemars(range(min = 500, max = 20_000))]
    pub token_budget: u16,
    /// How much source detail the pack may include.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_policy: Option<SourcePolicy>,
    /// Evidence sections to assemble.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 9))]
    pub sections: Option<Vec<ContextSection>>,
    /// Diversity bias applied under a tight budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diversity: Option<Diversity>,
    /// Minimum evidence confidence, integer 0 through 1000; defaults to 700.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 1_000))]
    pub min_confidence: Option<u16>,
    /// Progressive detail handle from a prior pack response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationCursor>,
}

/// Role of one evidence item within the assembled context pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    /// Primary definition of a target or closely related symbol.
    Definition,
    /// Implementation body or concrete logic.
    Implementation,
    /// Direct or transitive caller evidence.
    Caller,
    /// Test covering the target symbol or its callers.
    Test,
    /// Risk signal such as complexity, churn, or known fragility.
    Risk,
    /// Architectural context: module boundaries, layers, or dependency direction.
    Architecture,
    /// Recent change history relevant to the target.
    Change,
}

/// A bounded source snippet wrapped as untrusted repository data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositorySnippet {
    /// Generation-pinned source reference for the snippet.
    pub source_ref: SourceRef,
    /// Raw source text, treated strictly as data.
    #[schemars(length(min = 1, max = 524_288))]
    pub content: String,
    /// Trust classification for this repository-derived content.
    pub trust: TrustClassification,
}

/// Stable identifier for a context pack.
///
/// Deterministic for the exact generation, normalized request, and planner
/// version so a repeated request yields the same pack identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ContextPackId(#[schemars(length(min = 1, max = 128))] String);

impl ContextPackId {
    /// Wraps an already-validated pack identifier.
    #[must_use]
    pub fn new(id: String) -> Self {
        Self(id)
    }

    /// Borrows the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One ordered evidence item in a context pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextItem {
    /// Evidence role used for ranking and reading-order decisions.
    pub role: EvidenceRole,
    /// Stable symbol this item describes, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<SymbolId>,
    /// Generation-pinned source reference for the evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<SourceRef>,
    /// Relevance score, integer 0 through 1000.
    #[schemars(range(max = 1_000))]
    pub score: u16,
    /// Estimated token cost of this item.
    #[schemars(range(max = 32_000))]
    pub tokens: u32,
    /// Trust classification for repository-derived content in this item.
    pub trust: TrustClassification,
    /// Bounded source snippet, present only when source inclusion is requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<RepositorySnippet>,
}

/// Rootlight-generated structure guidance that never contains repository content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextStructure {
    /// Suggested reading or prompt order for the evidence items.
    #[schemars(length(max = 64))]
    pub reading_order: Vec<SourceFreeMessage>,
    /// Source-free notes on dependencies between evidence items.
    #[schemars(length(max = 64))]
    pub dependencies: Vec<SourceFreeMessage>,
}

/// One category of omitted evidence with an optional continuation handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OmissionSummary {
    /// Source-free reason code for the omission.
    pub reason: SafeLabel,
    /// Number of evidence items excluded for this reason.
    #[schemars(range(max = 100_000))]
    pub count: u32,
    /// Continuation handle to retrieve omitted items, when pageable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationCursor>,
}

/// A precise suggested next step that never contains repository content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolSuggestion {
    /// Dotted tool name recommended for the next step.
    #[schemars(length(min = 1, max = 64))]
    pub tool: String,
    /// Source-free rationale for the suggestion.
    pub reason: SourceFreeMessage,
    /// Continuation handle for the suggested call, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationCursor>,
}

/// Estimated token accounting for the assembled pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenAccounting {
    /// Total estimated tokens across all included items.
    #[schemars(range(max = 32_000))]
    pub estimated_total: u32,
    /// Estimated tokens broken down by section.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(length(max = 16))]
    pub by_section: BTreeMap<String, u32>,
}

/// `context.pack` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextPackData {
    /// Stable pack identifier for the exact generation, request, and planner.
    pub pack_id: ContextPackId,
    /// Ordered, deduplicated evidence items in deterministic rank order.
    #[schemars(length(max = 200))]
    pub items: Vec<ContextItem>,
    /// Rootlight-generated reading order and dependency notes.
    pub structure: ContextStructure,
    /// Summarized evidence excluded by budget, confidence, or diversity.
    #[schemars(length(max = 32))]
    pub omitted: Vec<OmissionSummary>,
    /// Precise continuation or source-read suggestions.
    #[schemars(length(max = 32))]
    pub followups: Vec<ToolSuggestion>,
    /// Estimated token usage by section and total.
    pub token_accounting: TokenAccounting,
}

/// Checked success-or-error output for `context.pack`.
pub type ContextPackOutput = ToolResponse<ReadEnvelope<ContextPackData>>;

// ---------------------------------------------------------------------------
// query.batch
// ---------------------------------------------------------------------------

/// How a batch treats independent operations after a runtime failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    /// Continue scheduling operations that do not depend on a failed one.
    ///
    /// This is the default: successful independent results are preserved.
    ContinueIndependent,
    /// Stop scheduling new operations after the first runtime failure.
    FailFast,
}

/// Closed allowlist of tools composable inside a public `query.batch`.
///
/// Serialized with dotted public tool names. Mutation tools, polling, nested
/// batches, `history.compare`, `query.advanced`, cross-generation operations,
/// and unbounded fanout are forbidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BatchTool {
    /// Bounded structural or lexical code search.
    #[serde(rename = "code.locate")]
    CodeLocate,
    /// Semantic evidence for stable symbol identifiers.
    #[serde(rename = "symbol.explain")]
    SymbolExplain,
    /// Typed relationship traversal for one or more symbols.
    #[serde(rename = "symbol.relationships")]
    SymbolRelationships,
    /// Cross-service or cross-module flow tracing.
    #[serde(rename = "flow.trace")]
    FlowTrace,
    /// Bounded change-impact analysis.
    #[serde(rename = "change.impact")]
    ChangeImpact,
    /// Test selection for given symbols or paths.
    #[serde(rename = "tests.select")]
    TestsSelect,
    /// High-level architecture overview.
    #[serde(rename = "architecture.overview")]
    ArchitectureOverview,
    /// Dependency-cycle detection.
    #[serde(rename = "architecture.cycles")]
    ArchitectureCycles,
    /// Dead-code detection.
    #[serde(rename = "code.dead")]
    CodeDead,
    /// Ordered change planning.
    #[serde(rename = "plan.change")]
    PlanChange,
    /// Context pack assembly.
    #[serde(rename = "context.pack")]
    ContextPack,
    /// Generation-pinned source range reads.
    #[serde(rename = "source.read")]
    SourceRead,
}

/// A restricted typed binding that copies one declared output field from a
/// completed dependency operation into a schema-compatible input field.
///
/// Wildcards, filters, expressions, templates, array expansion, and references
/// to warnings or untrusted text are forbidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchBinding {
    /// Operation identifier of the declared dependency to read from.
    #[serde(rename = "$from")]
    #[schemars(length(min = 1, max = 32))]
    pub from: String,
    /// Bounded RFC 6901 JSON Pointer into the completed operation response.
    #[schemars(length(min = 1, max = 1024))]
    pub pointer: String,
}

/// One operation inside a `query.batch` request.
///
/// The `arguments` object is validated against the selected tool's strict input
/// schema after all bindings are resolved. The `repository`, `generation`,
/// `budget`, `cursor`, and `response_profile` fields are omitted from arguments
/// because they are inherited from the batch envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchOperation {
    /// Unique operation identifier within this batch.
    #[schemars(length(min = 1, max = 32), regex(pattern = r"^[A-Za-z0-9_]+$"))]
    pub id: String,
    /// Tool selected from the closed batch allowlist.
    pub tool: BatchTool,
    /// Zero to eight earlier or later operation identifiers forming a bounded
    /// acyclic dependency graph with maximum depth eight.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 8), inner(length(min = 1, max = 32)))]
    pub depends_on: Option<Vec<String>>,
    /// Strict tool arguments as an object with batch-inherited fields omitted.
    ///
    /// Leaf values may be [`BatchBinding`] references that are resolved from
    /// completed dependency responses before schema validation.
    pub arguments: Map<String, Value>,
    /// Optional per-operation budget cap that may only reduce the allocation
    /// derived from the shared batch budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_budget: Option<ResponseBudget>,
}

/// Strict input for `query.batch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryBatchInput {
    /// One repository for the entire batch.
    pub repository: RepositorySelector,
    /// One generation selector applied to every operation.
    ///
    /// Defaults to the active generation and is resolved once before execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// One to sixteen operations in request order with unique identifiers.
    #[schemars(length(min = 1, max = 16))]
    pub operations: Vec<BatchOperation>,
    /// How to treat independent operations after a runtime failure.
    ///
    /// Defaults to continue-independent, preserving successful results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_policy: Option<FailurePolicy>,
    /// One shared result, traversal, source-byte, time, and token budget.
    ///
    /// The aggregate output budget defaults to 3000 tokens with a hard maximum
    /// of 16000 tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested response representation for the aggregate batch response.
    ///
    /// Individual operations cannot widen this profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
}

/// Aggregate batch outcome derived from individual operation results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    /// Every operation completed successfully.
    Ok,
    /// At least one operation succeeded and at least one failed or was skipped.
    Partial,
    /// No operation produced a successful result.
    Error,
}

/// Terminal status of one operation inside a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchOperationStatus {
    /// The operation completed successfully.
    Ok,
    /// The operation failed at runtime with a structured error.
    Error,
    /// The operation was skipped because a declared dependency failed.
    SkippedDependency,
    /// The operation was not scheduled because fail-fast stopped the batch.
    NotRunFailFast,
}

/// Result of one operation inside a `query.batch` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchOperationResult {
    /// Operation identifier matching the request.
    #[schemars(length(min = 1, max = 32))]
    pub id: String,
    /// Tool that was executed or scheduled.
    pub tool: BatchTool,
    /// Terminal status for this operation.
    pub status: BatchOperationStatus,
    /// Tool-specific successful result data, present when status is ok.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Structured error, present when status is error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<rootlight_error::PublicError>,
    /// Whether a hard or requested limit stopped this operation.
    pub truncated: bool,
    /// Safe continuation cursor when the operation result is pageable.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting for this operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageSummary>,
    /// Source-free warnings local to this operation.
    #[schemars(length(max = 32))]
    pub warnings: Vec<ResponseWarning>,
}

/// `query.batch` result data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryBatchData {
    /// Aggregate outcome derived from all operation results.
    pub batch_status: BatchStatus,
    /// The single generation pinned for every operation in the batch.
    pub generation_id: GenerationId,
    /// One result per requested operation in original request order.
    #[schemars(length(min = 1, max = 16))]
    pub operation_results: Vec<BatchOperationResult>,
}

/// Checked success-or-error output for `query.batch`.
pub type QueryBatchOutput = ToolResponse<ReadEnvelope<QueryBatchData>>;

// ---------------------------------------------------------------------------
// query.advanced
// ---------------------------------------------------------------------------

/// Typed scalar or identifier value bound as a query parameter or predicate operand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryValue {
    /// UTF-8 text literal.
    Text(#[schemars(length(min = 1, max = 4096))] String),
    /// Signed 64-bit integer literal.
    Integer(i64),
    /// Boolean literal.
    Boolean(bool),
    /// Stable symbol identifier.
    Symbol(SymbolId),
    /// Stable file identifier.
    File(FileId),
}

/// Allow-listed predicate operators for filter expressions.
///
/// Arbitrary regex, shell fragments, SQL, and Cypher strings are forbidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "pred", rename_all = "snake_case")]
pub enum QueryPredicate {
    /// Field equals a bound value.
    Equals {
        /// Column or field name to test.
        #[schemars(length(min = 1, max = 256))]
        field: String,
        /// Expected value.
        value: QueryValue,
    },
    /// Field does not equal a bound value.
    NotEquals {
        /// Column or field name to test.
        #[schemars(length(min = 1, max = 256))]
        field: String,
        /// Value to exclude.
        value: QueryValue,
    },
    /// Field value is contained in a bounded set.
    In {
        /// Column or field name to test.
        #[schemars(length(min = 1, max = 256))]
        field: String,
        /// Bounded set of allowed values.
        #[schemars(length(min = 1, max = 256))]
        values: Vec<QueryValue>,
    },
    /// Logical conjunction of bounded predicates.
    And {
        /// Predicates that must all hold.
        #[schemars(length(min = 1, max = 16))]
        predicates: Vec<QueryPredicate>,
    },
    /// Logical disjunction of bounded predicates.
    Or {
        /// Predicates of which at least one must hold.
        #[schemars(length(min = 1, max = 16))]
        predicates: Vec<QueryPredicate>,
    },
}

/// Traversal relation kinds permitted by the safe query AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
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

/// Traversal direction for graph navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraverseDirection {
    /// Follow edges toward callers or importers.
    Inbound,
    /// Follow edges toward callees or dependencies.
    Outbound,
    /// Follow edges in both directions.
    Both,
}

/// Allow-listed aggregate functions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "fn", rename_all = "snake_case")]
pub enum AggregateFunction {
    /// Count rows in each group.
    Count,
    /// Sum a numeric field per group.
    Sum {
        /// Numeric field to sum.
        #[schemars(length(min = 1, max = 256))]
        field: String,
    },
    /// Minimum of a comparable field per group.
    Min {
        /// Field to minimize.
        #[schemars(length(min = 1, max = 256))]
        field: String,
    },
    /// Maximum of a comparable field per group.
    Max {
        /// Field to maximize.
        #[schemars(length(min = 1, max = 256))]
        field: String,
    },
}

/// One sort directive for the sort operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SortKey {
    /// Column or field name to sort by.
    #[schemars(length(min = 1, max = 256))]
    pub field: String,
    /// Whether to sort ascending or descending.
    pub descending: bool,
}

/// Typed declarative query AST node.
///
/// This is a bounded, allow-listed operator tree. SQL strings, Cypher text,
/// shell fragments, arbitrary regex, arbitrary code, and unbounded recursion
/// are forbidden. Every node is type-checked and cost-estimated before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum QueryAstNode {
    /// Base scan over entities of a given kind.
    Scan {
        /// Entity kind to scan.
        entity: EntityKind,
        /// Optional filter applied during the scan.
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<Box<QueryPredicate>>,
    },
    /// Filter rows from an input node by a bounded predicate.
    Filter {
        /// Input node producing rows to filter.
        input: Box<QueryAstNode>,
        /// Predicate that rows must satisfy.
        predicate: QueryPredicate,
    },
    /// Project a bounded set of columns from an input node.
    Project {
        /// Input node producing rows to project.
        input: Box<QueryAstNode>,
        /// Column names to retain.
        #[schemars(length(min = 1, max = 64), inner(length(min = 1, max = 256)))]
        columns: Vec<String>,
    },
    /// Join two input nodes on a shared key column.
    Join {
        /// Left input node.
        left: Box<QueryAstNode>,
        /// Right input node.
        right: Box<QueryAstNode>,
        /// Column name to join on.
        #[schemars(length(min = 1, max = 256))]
        on: String,
    },
    /// Aggregate rows from an input node by grouping keys.
    Aggregate {
        /// Input node producing rows to aggregate.
        input: Box<QueryAstNode>,
        /// Column names to group by.
        #[schemars(length(max = 16), inner(length(min = 1, max = 256)))]
        group_by: Vec<String>,
        /// Aggregate functions to compute per group.
        #[schemars(length(min = 1, max = 16))]
        aggregations: Vec<AggregateFunction>,
    },
    /// Traverse graph edges from a seed symbol or bound column.
    Traverse {
        /// Seed symbol identifier for the traversal origin.
        #[serde(skip_serializing_if = "Option::is_none")]
        seed: Option<SymbolId>,
        /// Column name providing seed identifiers from the input node.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schemars(length(min = 1, max = 256))]
        seed_from: Option<String>,
        /// Relation kind to traverse.
        relation: RelationKind,
        /// Traversal direction.
        direction: TraverseDirection,
        /// Maximum traversal depth, hard ceiling five.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schemars(range(min = 1, max = 5))]
        max_depth: Option<u8>,
    },
    /// Sort rows from an input node by bounded keys.
    Sort {
        /// Input node producing rows to sort.
        input: Box<QueryAstNode>,
        /// Sort directives applied in order.
        #[schemars(length(min = 1, max = 8))]
        by: Vec<SortKey>,
    },
    /// Limit the number of rows from an input node.
    Limit {
        /// Input node producing rows to limit.
        input: Box<QueryAstNode>,
        /// Maximum rows to return.
        #[schemars(range(min = 1, max = 1000))]
        max_rows: u16,
    },
}

/// Typed column definition in a query result schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColumnSchema {
    /// Stable column name.
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    /// Column type descriptor.
    pub column_type: ColumnType,
}

/// Supported column types in advanced query results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
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

/// Explainable cost and plan for an advanced query execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanExplanation {
    /// Estimated total cost units for the query plan.
    #[schemars(range(max = 10_000_000))]
    pub estimated_cost: u64,
    /// Ordered operator names in the physical plan.
    #[schemars(length(max = 64), inner(length(min = 1, max = 128)))]
    pub operators: Vec<String>,
    /// Applied limit descriptions.
    #[schemars(length(max = 16), inner(length(min = 1, max = 256)))]
    pub applied_limits: Vec<String>,
}

/// Completeness classification for an advanced query result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryCompleteness {
    /// All matching rows were returned.
    Complete,
    /// Result is safely pageable with a continuation cursor.
    Paged,
    /// Result was truncated by a hard limit.
    Truncated,
    /// Query pattern is not supported with a source-free reason.
    Unsupported,
}

/// Strict input for `query.advanced`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryAdvancedInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Immutable generation to pin query execution; defaults to active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Typed declarative AST, never text SQL, Cypher, or shell.
    pub query: QueryAstNode,
    /// Bound typed values referenced by the AST.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 64))]
    pub parameters: Option<BTreeMap<String, QueryValue>>,
    /// Return the logical and physical plan without executing the query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
    /// Maximum returned rows, default one hundred.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: Option<u16>,
    /// Maximum traversal or plan depth, default three, hard ceiling five.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: Option<u8>,
    /// Maximum estimated plan cost the client is willing to pay.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 10_000_000))]
    pub cost_limit: Option<u64>,
    /// Continuation cursor when the plan is safely pageable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
}

/// `query.advanced` result data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryAdvancedData {
    /// Stable typed column definitions for the result rows.
    #[schemars(length(min = 1, max = 64))]
    pub columns: Vec<ColumnSchema>,
    /// Typed result rows: identifiers, scalars, compact entity views, or paths.
    #[schemars(length(max = 1000))]
    pub rows: Vec<Value>,
    /// Operators, estimates, and applied limits when explain was requested.
    pub plan: RequiredNullable<PlanExplanation>,
    /// Whether the result is complete, paged, truncated, or unsupported.
    pub completeness: QueryCompleteness,
}

/// Checked success-or-error output for `query.advanced`.
pub type QueryAdvancedOutput = ToolResponse<ReadEnvelope<QueryAdvancedData>>;
