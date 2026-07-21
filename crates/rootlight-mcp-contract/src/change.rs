//! Strict typed input and output schemas for the change-intelligence MCP tools.
//!
//! These types define the wire contract for `change.impact`, `tests.select`,
//! `history.compare`, and `plan.change`. The schema generator derives checked
//! public artifacts from these bounded types; transport routing consumes only
//! those generated artifacts.

use rootlight_error::SafeLabel;
use rootlight_ids::{FileId, GenerationId, SymbolId};
use rootlight_ir::{CoverageStatus, EntityKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vertical::{
    GenerationSelector, ReadEnvelope, RepositorySelector, RequiredNullable, ResponseBudget,
    ResponseProfile, ToolResponse,
};

// ---------------------------------------------------------------------------
// Shared change-intelligence enums
// ---------------------------------------------------------------------------

/// Relation propagation policy for impact analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationPolicy {
    /// Default balanced propagation.
    Standard,
    /// Over-approximate to avoid missing dependents.
    Conservative,
    /// Only direct dependents, no transitive closure.
    DirectOnly,
}

/// Classification of a resolved change span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClassification {
    /// Public API surface was modified.
    Surface,
    /// Internal implementation body was modified.
    Body,
    /// A new entity was added.
    Added,
    /// An entity was removed.
    Removed,
    /// An entity was renamed or moved.
    Renamed,
}

/// Aggregate risk level for an impact result.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// No measurable risk.
    None,
    /// Low risk, local changes only.
    Low,
    /// Medium risk, some cross-module effects.
    Medium,
    /// High risk, public surface or cross-service effects.
    High,
    /// Critical risk, breaking changes with wide fanout.
    Critical,
}

/// Kind of test relevant to a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
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

/// Kind of semantic change detected between revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticChangeKind {
    /// A new entity was added.
    Added,
    /// An entity was removed.
    Removed,
    /// An entity body was modified without signature change.
    Modified,
    /// An entity was moved to a different location.
    Moved,
    /// An entity was renamed.
    Renamed,
    /// An entity was split into multiple entities.
    Split,
    /// Multiple entities were merged.
    Merged,
    /// A public signature was modified.
    SignatureModified,
    /// A relation was added or removed.
    RelationChanged,
    /// An architectural boundary changed.
    ArchitectureChanged,
}

/// Objective class for a change plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanObjective {
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

// ---------------------------------------------------------------------------
// change.impact
// ---------------------------------------------------------------------------

/// Selector for the change set to analyze.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeSelector {
    /// Working-tree state to diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_tree: Option<WorkingTreeState>,
    /// Explicit Git revision range.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub revision_range: Option<String>,
    /// Explicit symbol identifiers known to be changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub symbol_ids: Option<Vec<SymbolId>>,
    /// Explicit file paths known to be changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 1000), inner(length(min = 1, max = 8192)))]
    pub paths: Option<Vec<String>>,
}

/// Working-tree diff state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkingTreeState {
    /// Unstaged changes only.
    Unstaged,
    /// Staged changes only.
    Staged,
    /// All working-tree changes including staged.
    All,
}

/// Scope bounding for impact analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactScope {
    /// Restrict to these repository-relative paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 8192)))]
    pub paths: Option<Vec<String>>,
    /// Restrict to these package identities.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128), inner(length(min = 1, max = 512)))]
    pub packages: Option<Vec<String>>,
    /// Restrict to these service identities.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 64), inner(length(min = 1, max = 512)))]
    pub services: Option<Vec<String>>,
}

/// Strict input for `change.impact`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeImpactInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Generation to analyze against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// The change set to map.
    pub change: ChangeSelector,
    /// Optional scope bounding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ImpactScope>,
    /// Relation propagation policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation_policy: Option<RelationPolicy>,
    /// Maximum transitive depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 8))]
    pub max_depth: Option<u8>,
    /// Whether to include test candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_tests: Option<bool>,
    /// Whether to include bounded history signals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_history: Option<bool>,
    /// Minimum confidence for propagation inclusion.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Response budget overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested response profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One resolved change from the input change set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedChange {
    /// Symbol identity, when the change maps to a known symbol.
    pub symbol_id: RequiredNullable<SymbolId>,
    /// File identity for the changed span.
    pub file_id: RequiredNullable<FileId>,
    /// Classification of the change.
    pub classification: ChangeClassification,
    /// Entity kind of the affected symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntityKind>,
}

/// One impacted dependent with path rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactEntry {
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind of the affected symbol.
    pub kind: EntityKind,
    /// Transitive distance from the change.
    #[schemars(range(min = 1, max = 8))]
    pub distance: u8,
    /// Confidence in the impact path, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub confidence: u16,
    /// Relation predicates forming the impact path.
    #[schemars(length(min = 1, max = 16), inner(length(min = 1, max = 128)))]
    pub via: Vec<String>,
    /// Whether this dependent is a public surface.
    pub is_public: bool,
}

/// One grouped set of impacted dependents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactGroup {
    /// The originating resolved change.
    pub source_index: u16,
    /// Ranked dependents.
    #[schemars(length(max = 500))]
    pub dependents: Vec<ImpactEntry>,
}

/// One cross-service or cross-repository impact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceImpact {
    /// Service or repository identity affected.
    #[schemars(length(min = 1, max = 512))]
    pub target: String,
    /// Kind of cross-boundary effect.
    pub kind: ServiceImpactKind,
    /// Confidence in the cross-boundary effect, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub confidence: u16,
    /// Source-free reason.
    pub reason: SafeLabel,
}

/// Kind of cross-service impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceImpactKind {
    /// An HTTP route consumer is affected.
    RouteConsumer,
    /// An async message consumer is affected.
    MessageConsumer,
    /// A shared database schema is affected.
    DatabaseSchema,
    /// A cross-repository symbol consumer is affected.
    CrossRepoSymbol,
}

/// One test candidate relevant to the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestCandidate {
    /// Test identity.
    #[schemars(length(min = 1, max = 512))]
    pub test_id: String,
    /// Relevance score, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub relevance: u16,
    /// Source-free rationale codes.
    #[schemars(length(min = 1, max = 8), inner(length(min = 1, max = 128)))]
    pub why: Vec<String>,
    /// Estimated execution cost in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 3_600_000))]
    pub estimated_cost_ms: Option<u32>,
}

/// Aggregate risk summary for the impact result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImpactRiskSummary {
    /// Aggregate risk level.
    pub level: RiskLevel,
    /// Source-free reason codes.
    #[schemars(length(max = 16), inner(length(min = 1, max = 128)))]
    pub reasons: Vec<String>,
    /// Coverage status of the impact analysis.
    pub coverage: CoverageStatus,
    /// Whether public surface was changed.
    pub breaking_surface: bool,
    /// Total transitive fanout count.
    #[schemars(range(max = 100_000))]
    pub fanout: u32,
    /// Whether dynamic or reflection-based relations create blind spots.
    pub dynamic_blind_spots: bool,
}

/// `change.impact` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeImpactData {
    /// Resolved changes from the input change set.
    #[schemars(length(min = 1, max = 1256))]
    pub resolved_changes: Vec<ResolvedChange>,
    /// Ranked impact groups.
    #[schemars(length(max = 1256))]
    pub impacted: Vec<ImpactGroup>,
    /// Cross-service and cross-repository effects.
    #[schemars(length(max = 128))]
    pub service_impacts: Vec<ServiceImpact>,
    /// Test candidates when requested.
    #[schemars(length(max = 500))]
    pub tests: Vec<TestCandidate>,
    /// Aggregate risk summary.
    pub risk_summary: ImpactRiskSummary,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `change.impact`.
pub type ChangeImpactOutput = ToolResponse<ReadEnvelope<ChangeImpactData>>;

// ---------------------------------------------------------------------------
// tests.select
// ---------------------------------------------------------------------------

/// Seed selector for test relevance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestSeedSelector {
    /// Symbol identifiers to seed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 64))]
    pub symbols: Option<Vec<SymbolId>>,
    /// File paths to seed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 8192)))]
    pub paths: Option<Vec<String>>,
    /// A change selector to derive seeds from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<ChangeSelector>,
    /// Build target identities to seed from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128), inner(length(min = 1, max = 512)))]
    pub build_targets: Option<Vec<String>>,
}

/// Execution budget hint for test selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionBudget {
    /// Maximum estimated total execution time in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 3_600_000))]
    pub max_total_ms: Option<u32>,
    /// Maximum number of slow tests to include.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 500))]
    pub max_slow_tests: Option<u16>,
}

/// Strict input for `tests.select`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestsSelectInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Generation to analyze against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Seed selector for relevance.
    pub seeds: TestSeedSelector,
    /// Filter to specific test kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 6))]
    pub test_kinds: Option<Vec<TestKind>>,
    /// Filter to specific test frameworks.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 32), inner(length(min = 1, max = 256)))]
    pub frameworks: Option<Vec<String>>,
    /// Maximum tests to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub max_tests: Option<u16>,
    /// Execution budget hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_budget: Option<ExecutionBudget>,
    /// Whether to include declarative command metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_commands: Option<bool>,
    /// Response budget overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested response profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One ranked test in the selection result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RankedTest {
    /// Test identity.
    #[schemars(length(min = 1, max = 512))]
    pub test_id: String,
    /// Test kind.
    pub kind: TestKind,
    /// Repository-relative path to the test.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 8192))]
    pub path: Option<String>,
    /// Relevance score, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub score: u16,
    /// Source-free rationale codes.
    #[schemars(length(min = 1, max = 8), inner(length(min = 1, max = 128)))]
    pub why: Vec<String>,
    /// Estimated execution cost in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(max = 3_600_000))]
    pub estimated_cost_ms: Option<u32>,
    /// Declarative test command metadata, inert and untrusted.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 1024))]
    pub command_hint: Option<String>,
}

/// Strategy summary for test coverage signals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestCoverageStrategy {
    /// Whether direct test edges were used.
    pub direct_edges: bool,
    /// Whether transitive dependency signals were used.
    pub transitive_signals: bool,
    /// Whether historical co-change signals were used.
    pub history_signals: bool,
    /// Whether build-target co-location was used.
    pub build_target_signals: bool,
}

/// One gap in test coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestGap {
    /// Scope identifier for the untested area.
    #[schemars(length(min = 1, max = 512))]
    pub scope: String,
    /// Source-free reason code.
    pub reason: SafeLabel,
}

/// `tests.select` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TestsSelectData {
    /// Ranked tests.
    #[schemars(length(max = 500))]
    pub tests: Vec<RankedTest>,
    /// Coverage strategy summary.
    pub coverage_strategy: TestCoverageStrategy,
    /// Identified coverage gaps.
    #[schemars(length(max = 128))]
    pub gaps: Vec<TestGap>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `tests.select`.
pub type TestsSelectOutput = ToolResponse<ReadEnvelope<TestsSelectData>>;

// ---------------------------------------------------------------------------
// history.compare
// ---------------------------------------------------------------------------

/// Selector for a revision or generation to compare.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RevisionSelector {
    /// Select by Git ref expression.
    Git(GitRevisionSelector),
    /// Select by generation identity.
    Generation(GenerationId),
}

/// Git revision selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GitRevisionSelector {
    /// Git ref expression such as a tag, branch, or commit SHA.
    #[schemars(length(min = 1, max = 512))]
    pub git: String,
}

/// Scope bounding for history comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareScope {
    /// Restrict to these repository-relative paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 8192)))]
    pub paths: Option<Vec<String>>,
    /// Restrict to these package identities.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128), inner(length(min = 1, max = 512)))]
    pub packages: Option<Vec<String>>,
    /// Restrict to these symbol identifiers.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub symbols: Option<Vec<SymbolId>>,
}

/// Kind of change to include in comparison results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareChangeKind {
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

/// Strict input for `history.compare`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryCompareInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Base revision or generation.
    pub base: RevisionSelector,
    /// Head revision or generation.
    pub head: RevisionSelector,
    /// Optional scope bounding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<CompareScope>,
    /// Filter to specific change kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 8))]
    pub change_kinds: Option<Vec<CompareChangeKind>>,
    /// Maximum results per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: Option<u16>,
    /// Whether to include unchanged context entities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_unchanged_context: Option<bool>,
    /// Response budget overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested response profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ResponseProfile>,
}

/// Resolved state pair for the comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatchedStates {
    /// Resolved base generation.
    pub base_generation: GenerationId,
    /// Resolved head generation.
    pub head_generation: GenerationId,
    /// Coverage of the comparison.
    pub coverage: CoverageStatus,
}

/// One semantic change between revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SemanticChange {
    /// Kind of semantic change.
    pub kind: SemanticChangeKind,
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind.
    pub entity_kind: EntityKind,
    /// Whether this change is a breaking candidate.
    pub breaking_candidate: bool,
    /// Significance rank, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub significance: u16,
}

/// Aggregate architecture delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureDelta {
    /// Number of new cross-service edges.
    #[schemars(range(max = 10_000))]
    pub new_cross_service_edges: u32,
    /// Number of removed cross-service edges.
    #[schemars(range(max = 10_000))]
    pub removed_cross_service_edges: u32,
    /// Number of new component boundaries.
    #[schemars(range(max = 10_000))]
    pub new_boundaries: u32,
    /// Number of removed component boundaries.
    #[schemars(range(max = 10_000))]
    pub removed_boundaries: u32,
}

/// One breaking-change candidate with consumer evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BreakingCandidate {
    /// Affected symbol identity.
    pub symbol_id: SymbolId,
    /// Number of known consumers.
    #[schemars(range(max = 100_000))]
    pub consumer_count: u32,
    /// Whether the symbol is part of a public API surface.
    pub is_public_surface: bool,
    /// Source-free reason code.
    pub reason: SafeLabel,
}

/// One lineage match between base and head entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineageMatch {
    /// Base symbol identity.
    pub base_symbol_id: SymbolId,
    /// Head symbol identity.
    pub head_symbol_id: SymbolId,
    /// Match confidence, 0 through 1000.
    #[schemars(range(max = 1000))]
    pub confidence: u16,
    /// Whether this is a rename rather than identity preservation.
    pub is_rename: bool,
}

/// `history.compare` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryCompareData {
    /// Resolved state pair.
    pub matched_states: MatchedStates,
    /// Semantic changes in significance order.
    #[schemars(length(max = 1000))]
    pub changes: Vec<SemanticChange>,
    /// Aggregate architecture delta.
    pub architecture_delta: ArchitectureDelta,
    /// Breaking-change candidates.
    #[schemars(length(max = 256))]
    pub breaking_candidates: Vec<BreakingCandidate>,
    /// Entity lineage matches.
    #[schemars(length(max = 1000))]
    pub lineage: Vec<LineageMatch>,
}

/// Checked success-or-error output for `history.compare`.
pub type HistoryCompareOutput = ToolResponse<ReadEnvelope<HistoryCompareData>>;

// ---------------------------------------------------------------------------
// plan.change
// ---------------------------------------------------------------------------

/// Target selector for a change plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PlanTargetSelector {
    /// Select by symbol identity.
    Symbol(PlanSymbolTarget),
    /// Select by file identity.
    File(PlanFileTarget),
}

/// Symbol-based plan target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanSymbolTarget {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
}

/// File-based plan target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanFileTarget {
    /// Stable file identity.
    pub file_id: FileId,
}

/// Strict input for `plan.change`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanChangeInput {
    /// Repository or workspace selector.
    pub repository: RepositorySelector,
    /// Generation to plan against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Typed objective class.
    pub objective: PlanObjective,
    /// Concrete objective description, treated as user instruction.
    #[schemars(length(min = 1, max = 4096))]
    pub objective_text: String,
    /// Target symbols or files.
    #[schemars(length(min = 1, max = 64))]
    pub targets: Vec<PlanTargetSelector>,
    /// User-provided constraints.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 32), inner(length(min = 1, max = 1024)))]
    pub constraints: Option<Vec<String>>,
    /// Existing working-tree or hypothetical change context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_context: Option<ChangeSelector>,
    /// Maximum plan steps.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100))]
    pub max_steps: Option<u8>,
    /// Response budget overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested response profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<ResponseProfile>,
}

/// One ordered step in a change plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePlanStep {
    /// One-based step ordinal.
    #[schemars(range(min = 1, max = 100))]
    pub step: u8,
    /// Source-free action description.
    #[schemars(length(min = 1, max = 1024))]
    pub action: String,
    /// Target symbol identities for this step.
    #[schemars(length(max = 32))]
    pub targets: Vec<SymbolId>,
    /// Step ordinals this step depends on.
    #[schemars(length(max = 32))]
    pub depends_on: Vec<u8>,
    /// Source-free risk codes for this step.
    #[schemars(length(max = 8), inner(length(min = 1, max = 128)))]
    pub risks: Vec<String>,
    /// Source-free verification hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 1024))]
    pub verification: Option<String>,
}

/// Compact impact and ownership summary for the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanImpactSummary {
    /// Total affected symbol count.
    #[schemars(range(max = 100_000))]
    pub affected_symbols: u32,
    /// Total affected file count.
    #[schemars(range(max = 100_000))]
    pub affected_files: u32,
    /// Aggregate risk level.
    pub risk_level: RiskLevel,
    /// Whether public surface is affected.
    pub touches_public_surface: bool,
}

/// One open decision that cannot be safely inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanDecision {
    /// Source-free question identifier.
    #[schemars(length(min = 1, max = 512))]
    pub question: String,
    /// Recommended default choice.
    #[schemars(length(min = 1, max = 512))]
    pub recommended_default: String,
}

/// Ready follow-up arguments for implementation context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextPackRequest {
    /// Symbol identities to include in the context pack.
    #[schemars(length(max = 64))]
    pub symbols: Vec<SymbolId>,
    /// File identities to include in the context pack.
    #[schemars(length(max = 64))]
    pub files: Vec<FileId>,
}

/// `plan.change` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanChangeData {
    /// Ordered plan steps.
    #[schemars(length(min = 1, max = 100))]
    pub plan: Vec<ChangePlanStep>,
    /// Compact impact and ownership summary.
    pub affected_scope: PlanImpactSummary,
    /// Ranked verification test plan.
    #[schemars(length(max = 500))]
    pub test_plan: Vec<TestCandidate>,
    /// Open decisions requiring user input.
    #[schemars(length(max = 16))]
    pub open_decisions: Vec<PlanDecision>,
    /// Ready follow-up context pack arguments.
    pub context_pack_request: ContextPackRequest,
}

/// Checked success-or-error output for `plan.change`.
pub type PlanChangeOutput = ToolResponse<ReadEnvelope<PlanChangeData>>;
