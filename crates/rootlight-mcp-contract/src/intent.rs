//! Strict typed input and output schemas for the intent-oriented MCP tools.
//!
//! These types define the bounded wire contract for `symbol.relationships`,
//! `flow.trace`, `architecture.overview`, `architecture.cycles`, and
//! `code.dead`. The schema generator derives checked public artifacts from
//! these types; transport routing consumes only those generated artifacts.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::SymbolId;
use rootlight_ir::SourceRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TrustClassification;
use crate::vertical::{
    AnalysisReadEnvelope, AnalysisToolResponse, ContinuationCursor, GenerationSelector,
    ProvenanceSummary, ProvenanceSummaryV1_0, ReadEnvelope, RepositorySelector, ResponseBudget,
    ResponseProfile, ScopeSelector, ToolResponse,
};

// ---------------------------------------------------------------------------
// Shared relation and direction types
// ---------------------------------------------------------------------------

/// Typed relation families recognized across intent tools.
///
/// Each tool advertises and enforces its own supported subset.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
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

/// Traversal direction for relation queries.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow outbound edges from the seed.
    Outbound,
    /// Follow inbound edges toward the seed.
    Inbound,
    /// Follow edges in both directions.
    Both,
}

// ---------------------------------------------------------------------------
// symbol.relationships
// ---------------------------------------------------------------------------

/// Strict input for `symbol.relationships`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolRelationshipsInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Seed symbols for neighborhood expansion.
    #[schemars(length(min = 1, max = 64))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Requested relation families.
    ///
    /// The served set is `calls`, `called_by`, `references`, `types`,
    /// `implements`, and `imports`.
    #[schemars(length(min = 1, max = 16))]
    pub relations: BTreeSet<RelationKind>,
    /// Traversal direction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Optional structural scope constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Minimum edge confidence threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Whether to include ambiguous candidate sets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_candidates: Option<bool>,
    /// Maximum returned relationship edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub max_results: Option<u16>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Opaque continuation cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One typed relationship target within a group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipTarget {
    /// Stable symbol identity of the related entity.
    pub symbol_id: SymbolId,
    /// Edge confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source evidence references.
    #[schemars(length(max = 16))]
    pub source_refs: Vec<SourceRef>,
    /// Bounded provenance evidence.
    #[schemars(length(max = 16))]
    pub provenance: Vec<ProvenanceSummary>,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// Relationship target retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipTargetV1_0 {
    /// Stable symbol identity of the related entity.
    pub symbol_id: SymbolId,
    /// Edge confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source evidence references.
    #[schemars(length(max = 16))]
    pub source_refs: Vec<SourceRef>,
    /// Bounded provenance evidence.
    #[schemars(length(max = 16))]
    pub provenance: Vec<ProvenanceSummaryV1_0>,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// One seed-relation group in the relationship response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipGroup {
    /// Seed symbol that was expanded.
    pub seed: SymbolId,
    /// Relation family for this group.
    pub relation: RelationKind,
    /// Direction of the returned edges.
    pub direction: Direction,
    /// Bounded relationship targets.
    #[schemars(length(max = 500))]
    pub items: Vec<RelationshipTarget>,
    /// Total known edges before truncation.
    pub total_count: u32,
}

/// Relationship group retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipGroupV1_0 {
    /// Seed symbol that was expanded.
    pub seed: SymbolId,
    /// Relation family for this group.
    pub relation: RelationKind,
    /// Direction of the returned edges.
    pub direction: Direction,
    /// Bounded relationship targets.
    #[schemars(length(max = 500))]
    pub items: Vec<RelationshipTargetV1_0>,
    /// Total known edges before truncation.
    pub total_count: u32,
}

/// Summary of unresolved or ambiguous relationship sites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedSiteSummary {
    /// Seed symbol with unresolved sites.
    pub seed: SymbolId,
    /// Relation family.
    pub relation: RelationKind,
    /// Number of ambiguous candidate sets.
    pub candidate_count: u32,
    /// Source-free reason code.
    pub reason: crate::vertical::SourceFreeMessage,
}

/// Aggregate relationship counts for the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationshipTotals {
    /// Total edges returned.
    pub returned_edges: u32,
    /// Total edges known before budget limits.
    pub total_edges: u32,
    /// Whether counts are exact or lower bounds.
    pub exact: bool,
}

/// `symbol.relationships` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolRelationshipsData {
    /// Seed-relation groups with typed targets.
    #[schemars(length(max = 1024))]
    pub groups: Vec<RelationshipGroup>,
    /// Unresolved or ambiguous sites when candidates requested.
    #[schemars(length(max = 64))]
    pub unresolved: Vec<UnresolvedSiteSummary>,
    /// Aggregate edge counts.
    pub totals: RelationshipTotals,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `symbol.relationships` result data retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolRelationshipsDataV1_0 {
    /// Seed-relation groups with typed targets.
    #[schemars(length(max = 1024))]
    pub groups: Vec<RelationshipGroupV1_0>,
    /// Unresolved or ambiguous sites when candidates requested.
    #[schemars(length(max = 64))]
    pub unresolved: Vec<UnresolvedSiteSummary>,
    /// Aggregate edge counts.
    pub totals: RelationshipTotals,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Retained 1.0 output for `symbol.relationships`.
pub type SymbolRelationshipsOutputV1_0 = ToolResponse<ReadEnvelope<SymbolRelationshipsDataV1_0>>;

/// Current additive output for `symbol.relationships`.
pub type SymbolRelationshipsOutputV1_1 =
    AnalysisToolResponse<AnalysisReadEnvelope<SymbolRelationshipsData>>;

/// Checked success-or-error output for `symbol.relationships`.
pub type SymbolRelationshipsOutput = SymbolRelationshipsOutputV1_1;

// ---------------------------------------------------------------------------
// flow.trace
// ---------------------------------------------------------------------------

/// Node selector for trace endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeSelector {
    /// Stable symbol identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<SymbolId>,
    /// Route identifier for service traces.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub route_id: Option<String>,
    /// Service identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub service_id: Option<String>,
    /// Database object identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 512))]
    pub database_object_id: Option<String>,
}

/// Path selection policy for trace results.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PathPolicy {
    /// Prefer shortest paths.
    Shortest,
    /// Prefer diverse paths through different intermediaries.
    Diverse,
    /// Prefer paths with highest aggregate confidence.
    HighestConfidence,
}

/// Strict input for `flow.trace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowTraceInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Source node for the trace.
    pub from: NodeSelector,
    /// Optional target node. Without a target, returns bounded outward traces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<NodeSelector>,
    /// Traversal direction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Explicit relation allow-list.
    ///
    /// Every declared relation family is admitted; missing adapter coverage is
    /// reported through response coverage rather than fabricated edges.
    #[schemars(length(min = 1, max = 16))]
    pub relations: BTreeSet<RelationKind>,
    /// Maximum traversal depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 8))]
    pub max_depth: Option<u8>,
    /// Maximum independently returned paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100))]
    pub max_paths: Option<u16>,
    /// Minimum edge confidence threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Path selection strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_policy: Option<PathPolicy>,
    /// Whether cross-repository traversal is permitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_repository: Option<bool>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One edge within a traced path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TraceEdge {
    /// Relation kind for this hop.
    pub kind: RelationKind,
    /// Edge confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source evidence for this edge.
    #[schemars(length(max = 8))]
    pub source_refs: Vec<SourceRef>,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// One complete traced path from source toward target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TracePath {
    /// Aggregate path confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Ordered node identifiers along the path.
    #[schemars(length(min = 2, max = 9))]
    pub nodes: Vec<SymbolId>,
    /// Evidence-bearing edges between consecutive nodes.
    #[schemars(length(min = 1, max = 8))]
    pub edges: Vec<TraceEdge>,
    /// Whether this path contains a cycle.
    pub cyclic: bool,
}

/// Frontier summary describing traversal boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrontierSummary {
    /// Number of nodes reached during traversal.
    pub reached_nodes: u32,
    /// Number of edges examined during traversal.
    pub examined_edges: u32,
    /// Whether the traversal was truncated by budget or depth.
    pub truncated: bool,
    /// Number of unresolved boundary nodes.
    pub unresolved_boundaries: u32,
}

/// Relation projection actually used by the trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationProjection {
    /// Relations included in the traversal.
    #[schemars(length(min = 1, max = 16))]
    pub relations: BTreeSet<RelationKind>,
    /// Minimum confidence threshold applied.
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: u16,
}

/// `flow.trace` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowTraceData {
    /// Bounded traced paths ordered by policy.
    #[schemars(length(max = 100))]
    pub paths: Vec<TracePath>,
    /// Traversal frontier and boundary summary.
    pub frontier: FrontierSummary,
    /// Actual relation projection used.
    pub projection: RelationProjection,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `flow.trace`.
pub type FlowTraceOutput = ToolResponse<ReadEnvelope<FlowTraceData>>;

// ---------------------------------------------------------------------------
// architecture.overview
// ---------------------------------------------------------------------------

/// Architecture view categories.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArchitectureView {
    /// Module-level decomposition.
    Modules,
    /// Package-level decomposition.
    Packages,
    /// Service boundaries.
    Services,
    /// Data stores and schemas.
    Data,
    /// Build targets and compilation units.
    Build,
    /// Ownership and authorship.
    Ownership,
    /// Community detection clusters.
    Communities,
    /// Structural hotspot ranking.
    Hotspots,
}

/// Architecture detail level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArchitectureDetail {
    /// Minimal summary with counts.
    Summary,
    /// Standard component and connection detail.
    Standard,
    /// Maximum bounded evidence and metrics.
    Detailed,
}

/// Strict input for `architecture.overview`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureOverviewInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Optional structural scope constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Requested architecture views.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 8))]
    pub views: Option<BTreeSet<ArchitectureView>>,
    /// Requested detail level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ArchitectureDetail>,
    /// Maximum returned components.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 250))]
    pub max_components: Option<u16>,
    /// Whether to include aggregated connection edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_edges: Option<bool>,
    /// Minimum confidence for heuristic links.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One aggregated architecture component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureComponent {
    /// Stable component identity.
    #[schemars(length(min = 1, max = 512))]
    pub id: String,
    /// Component kind label.
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
    /// Repository-controlled display name; always untrusted data.
    #[schemars(length(min = 1, max = 1024))]
    pub name: String,
    /// Number of contained symbols.
    pub symbol_count: u32,
    /// Number of distinct files represented by the component.
    pub file_count: u32,
    /// Evidence categories supporting the responsibility assignment.
    #[schemars(length(max = 16))]
    pub responsibility_evidence: Vec<String>,
    /// Direct immutable evidence, bounded by the requested detail level.
    #[schemars(length(max = 16))]
    pub source_refs: Vec<SourceRef>,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// One architecture component retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureComponentV1_0 {
    /// Stable component identity.
    #[schemars(length(min = 1, max = 512))]
    pub id: String,
    /// Component kind label.
    #[schemars(length(min = 1, max = 64))]
    pub kind: String,
    /// Repository-controlled display name; always untrusted data.
    #[schemars(length(min = 1, max = 1024))]
    pub name: String,
    /// Number of contained symbols.
    pub symbol_count: u32,
    /// Evidence categories supporting the responsibility assignment.
    #[schemars(length(max = 16))]
    pub responsibility_evidence: Vec<String>,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// One aggregated connection between architecture components.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureConnection {
    /// Source component identity.
    #[schemars(length(min = 1, max = 512))]
    pub from: String,
    /// Target component identity.
    #[schemars(length(min = 1, max = 512))]
    pub to: String,
    /// Relation kind for this connection.
    pub kind: RelationKind,
    /// Aggregated edge weight or count.
    pub weight: u32,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
}

/// One structural hotspot ranking entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hotspot {
    /// Component identity.
    #[schemars(length(min = 1, max = 512))]
    pub component_id: String,
    /// Fan-in metric.
    pub fan_in: u32,
    /// Fan-out metric.
    pub fan_out: u32,
    /// Change frequency signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_frequency: Option<u32>,
    /// Complexity signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<u32>,
    /// Ownership-edge signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership_signal: Option<u32>,
    /// Test-edge signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_signal: Option<u32>,
    /// Aggregate hotspot score from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub score: u16,
}

/// One structural hotspot retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HotspotV1_0 {
    /// Component identity.
    #[schemars(length(min = 1, max = 512))]
    pub component_id: String,
    /// Fan-in metric.
    pub fan_in: u32,
    /// Fan-out metric.
    pub fan_out: u32,
    /// Change frequency signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_frequency: Option<u32>,
    /// Complexity signal when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub complexity: Option<u32>,
    /// Aggregate hotspot score from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub score: u16,
}

/// One deterministic structural-affinity community.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureCommunity {
    /// Stable derived community identity.
    #[schemars(length(min = 1, max = 512))]
    pub id: String,
    /// Component identities assigned to this derived community.
    #[schemars(length(min = 1, max = 250))]
    pub members: Vec<String>,
    /// Sum of weights for connections whose endpoints are both members.
    pub internal_connection_weight: u64,
    /// Always false; communities do not establish canonical ownership.
    pub ownership_truth: bool,
}

/// Derived view metadata for community or ownership algorithms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DerivedViewInfo {
    /// View category.
    pub view: ArchitectureView,
    /// Algorithm version identifier.
    #[schemars(length(min = 1, max = 128))]
    pub algorithm_version: String,
    /// Canonically ordered source-free algorithm parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(length(max = 16))]
    pub parameters: BTreeMap<String, String>,
}

/// `architecture.overview` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureOverviewData {
    /// Aggregated architecture components.
    #[schemars(length(max = 250))]
    pub components: Vec<ArchitectureComponent>,
    /// Aggregated typed connections between components.
    #[schemars(length(max = 1000))]
    pub connections: Vec<ArchitectureConnection>,
    /// Structural hotspot rankings.
    #[schemars(length(max = 250))]
    pub hotspots: Vec<Hotspot>,
    /// Deterministic non-canonical architectural communities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 250))]
    pub communities: Vec<ArchitectureCommunity>,
    /// Derived view algorithm metadata.
    #[schemars(length(max = 8))]
    pub views: Vec<DerivedViewInfo>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `architecture.overview` result data retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureOverviewDataV1_0 {
    /// Aggregated architecture components.
    #[schemars(length(max = 250))]
    pub components: Vec<ArchitectureComponentV1_0>,
    /// Aggregated typed connections between components.
    #[schemars(length(max = 1000))]
    pub connections: Vec<ArchitectureConnection>,
    /// Structural hotspot rankings.
    #[schemars(length(max = 250))]
    pub hotspots: Vec<HotspotV1_0>,
    /// Deterministic non-canonical architectural communities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 250))]
    pub communities: Vec<ArchitectureCommunity>,
    /// Derived view algorithm metadata.
    #[schemars(length(max = 8))]
    pub views: Vec<DerivedViewInfo>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked `architecture.overview` output retained for explicit 1.0 callers.
pub type ArchitectureOverviewOutputV1_0 = ToolResponse<ReadEnvelope<ArchitectureOverviewDataV1_0>>;

/// Current checked `architecture.overview` output.
pub type ArchitectureOverviewOutputV1_1 =
    AnalysisToolResponse<AnalysisReadEnvelope<ArchitectureOverviewData>>;

/// Current checked success-or-error output for `architecture.overview`.
pub type ArchitectureOverviewOutput = ArchitectureOverviewOutputV1_1;

// ---------------------------------------------------------------------------
// architecture.cycles
// ---------------------------------------------------------------------------

/// Cycle ranking strategy.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CycleRankBy {
    /// Rank by cycle size.
    Size,
    /// Rank by aggregate edge weight.
    EdgeWeight,
    /// Rank by change risk signal.
    ChangeRisk,
    /// Rank by estimated break cost.
    BreakCost,
}

/// Aggregation level for a cycle projection.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CycleLevel {
    /// Preserve symbol-level nodes.
    Symbol,
    /// Aggregate through the nearest module or namespace.
    Module,
    /// Aggregate through the nearest package.
    Package,
    /// Aggregate through the nearest build target.
    BuildTarget,
    /// Aggregate through the nearest service.
    Service,
}

/// Relation projection for cycle detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CycleProjection {
    /// Relation families included in the projection.
    ///
    /// The served set is `calls`, `references`, `types`, `implements`, and
    /// `imports`.
    #[schemars(length(min = 1, max = 8))]
    pub relations: BTreeSet<RelationKind>,
    /// Aggregation level for the projection.
    pub level: CycleLevel,
}

/// Strict input for `architecture.cycles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureCyclesInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Optional structural scope constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Relation projection for cycle detection.
    pub projection: CycleProjection,
    /// Minimum cycle size to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 2, max = 64))]
    pub min_size: Option<u8>,
    /// Maximum returned cycles.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 200))]
    pub max_cycles: Option<u16>,
    /// Whether to include self-referential cycles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_self_cycles: Option<bool>,
    /// Cycle ranking strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank_by: Option<CycleRankBy>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// One strongly connected component containing cycles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StronglyConnectedComponent {
    /// Number of nodes in the component.
    pub size: u32,
    /// Member node identifiers.
    #[schemars(length(min = 1, max = 1_000_000))]
    pub members: Vec<String>,
    /// Internal edge count.
    pub internal_edges: u32,
    /// Sum of admitted confidence across internal edges.
    pub edge_weight: u64,
    /// Bounded internal history-edge count.
    pub change_risk: u32,
    /// Cost of the least expensive candidate break edge.
    #[schemars(range(min = 0, max = 1000))]
    pub break_cost: u16,
}

/// One strongly connected component retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StronglyConnectedComponentV1_0 {
    /// Number of nodes in the component.
    pub size: u32,
    /// Member node identifiers.
    #[schemars(length(min = 1, max = 1_000_000))]
    pub members: Vec<String>,
    /// Internal edge count.
    pub internal_edges: u32,
}

/// One minimal representative cycle with evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MinimalCycle {
    /// Ordered node identifiers forming the cycle, first repeated at end.
    #[schemars(length(min = 2, max = 1_000_001))]
    pub nodes: Vec<String>,
    /// Source evidence for each edge in the cycle.
    #[schemars(length(max = 64))]
    pub edge_evidence: Vec<SourceRef>,
    /// Aggregate cycle confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
}

/// One candidate edge or interface for breaking a cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CycleBreakCandidate {
    /// Source node of the break edge.
    #[schemars(length(min = 1, max = 512))]
    pub from: String,
    /// Target node of the break edge.
    #[schemars(length(min = 1, max = 512))]
    pub to: String,
    /// Relation kind of the break edge.
    pub kind: RelationKind,
    /// Estimated break cost from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub break_cost: u16,
    /// Source evidence for the candidate edge.
    #[schemars(length(max = 8))]
    pub source_refs: Vec<SourceRef>,
}

/// Relation projection actually served by cycle detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServedCycleProjection {
    /// Relation families included in the projection.
    #[schemars(length(min = 1, max = 8))]
    pub relations: BTreeSet<RelationKind>,
    /// Minimum admitted edge confidence.
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: u16,
    /// Aggregation level applied before SCC detection.
    pub level: CycleLevel,
    /// Ranking strategy applied to cyclic components.
    pub rank_by: CycleRankBy,
    /// Scoped endpoints omitted because their requested-level container was unavailable.
    pub omitted_nodes: u32,
}

/// `architecture.cycles` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureCyclesData {
    /// Strongly connected components containing cycles.
    #[schemars(length(max = 200))]
    pub components: Vec<StronglyConnectedComponent>,
    /// Bounded representative minimal cycles.
    #[schemars(length(max = 200))]
    pub cycles: Vec<MinimalCycle>,
    /// Ranked candidate break points.
    #[schemars(length(max = 200))]
    pub break_candidates: Vec<CycleBreakCandidate>,
    /// Relation projection and ranking actually served.
    pub projection: ServedCycleProjection,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `architecture.cycles` result data retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureCyclesDataV1_0 {
    /// Strongly connected components containing cycles.
    #[schemars(length(max = 200))]
    pub components: Vec<StronglyConnectedComponentV1_0>,
    /// Bounded representative minimal cycles.
    #[schemars(length(max = 200))]
    pub cycles: Vec<MinimalCycle>,
    /// Ranked candidate break points.
    #[schemars(length(max = 200))]
    pub break_candidates: Vec<CycleBreakCandidate>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked `architecture.cycles` output retained for explicit 1.0 callers.
pub type ArchitectureCyclesOutputV1_0 = ToolResponse<ReadEnvelope<ArchitectureCyclesDataV1_0>>;

/// Current checked `architecture.cycles` output.
pub type ArchitectureCyclesOutputV1_1 =
    AnalysisToolResponse<AnalysisReadEnvelope<ArchitectureCyclesData>>;

/// Current checked success-or-error output for `architecture.cycles`.
pub type ArchitectureCyclesOutput = ArchitectureCyclesOutputV1_1;

// ---------------------------------------------------------------------------
// code.dead
// ---------------------------------------------------------------------------

/// Named entry-point model policy for bounded static reachability observations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NamedEntryPointPolicy {
    /// Partial mixed entry-point model.
    Standard,
    /// Library export surface.
    Library,
    /// Application mains and registered handlers.
    Application,
    /// Framework routes, services, and registrations.
    FrameworkSpecific,
}

/// Caller-supplied complete static entry roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplicitEntryPointPolicy {
    /// Distinct stable entry symbols.
    #[schemars(length(min = 1, max = 64))]
    pub entry_symbols: BTreeSet<SymbolId>,
}

/// Entry-point policy accepted by `code.dead`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EntryPointPolicy {
    /// One predefined bounded entry model.
    Named(NamedEntryPointPolicy),
    /// A caller-supplied stable root set.
    Explicit(ExplicitEntryPointPolicy),
}

/// Entry-point policy kind reported by `code.dead`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryPointPolicyKind {
    /// Partial mixed entry-point model.
    Standard,
    /// Library export surface.
    Library,
    /// Application mains and registered handlers.
    Application,
    /// Framework routes, services, and registrations.
    FrameworkSpecific,
    /// Caller-supplied stable root set.
    Explicit,
}

/// Entry-point policy retained in explicit 1.0 responses.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntryPointPolicyV1_0 {
    /// Partial mixed entry-point model.
    Standard,
    /// Library entry-point model.
    Library,
    /// Application entry-point model.
    Application,
}

/// Strict input for `code.dead`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeDeadInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Optional structural scope constraint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Entry-point model policy or caller-supplied stable entry symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_point_policy: Option<EntryPointPolicy>,
    /// Whether to include exported symbols as candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_exported: Option<bool>,
    /// Whether to include test symbols as candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_tests: Option<bool>,
    /// Minimum reachability edge confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Maximum returned dead-code candidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub max_candidates: Option<u16>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Classification of one bounded static reachability observation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeadClassification {
    /// No incoming reference was observed in the served static relation graph.
    NoObservedIncomingReferences,
    /// The partial entry-point scan did not reach a strongly referenced symbol.
    NotObservedFromEntryPointsStrongReferences,
    /// The partial entry-point scan did not reach a weakly referenced symbol.
    NotObservedFromEntryPointsWeakReferences,
}

/// One bounded static reachability observation with evidence and caveats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeadCandidate {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
    /// Static reachability observation; never a runtime-liveness proof.
    pub classification: DeadClassification,
    /// Confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source-free reasons supporting the classification.
    #[schemars(length(min = 1, max = 16))]
    pub why: Vec<String>,
    /// Suppression rules checked for this candidate.
    #[schemars(length(max = 16))]
    pub suppressions_checked: Vec<String>,
    /// Static reachability measurements supporting the classification.
    pub reachability: DeadReachabilitySummary,
    /// Source-free uncertainty conditions.
    #[schemars(length(max = 16), inner(length(min = 1, max = 256)))]
    pub uncertainty: Vec<String>,
    /// Source evidence for the candidate.
    #[schemars(length(max = 8))]
    pub source_refs: Vec<SourceRef>,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// Static reachability measurements for one dead-code candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeadReachabilitySummary {
    /// Whether the selected entry-point closure reached the symbol.
    pub reached_from_entry_points: bool,
    /// Number of admitted incoming edges.
    pub incoming_edges: u32,
    /// Strongest admitted incoming edge confidence.
    #[schemars(range(min = 0, max = 1000))]
    pub strongest_incoming_confidence: u16,
}

/// One dead-code candidate retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeadCandidateV1_0 {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
    /// Static reachability observation; never a runtime-liveness proof.
    pub classification: DeadClassification,
    /// Confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source-free reasons supporting the classification.
    #[schemars(length(min = 1, max = 16))]
    pub why: Vec<String>,
    /// Suppression rules checked for this candidate.
    #[schemars(length(max = 16))]
    pub suppressions_checked: Vec<String>,
    /// Source evidence for the candidate.
    #[schemars(length(max = 8))]
    pub source_refs: Vec<SourceRef>,
    /// Trust classification for repository-derived content.
    pub trust: TrustClassification,
}

/// Summary of the partial entry-point model used for static reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryPointSummary {
    /// Policy used for entry-point resolution.
    pub policy: EntryPointPolicyKind,
    /// Number of resolved entry points.
    pub entry_point_count: u32,
    /// Bounded stable entry symbols used as roots.
    #[schemars(length(max = 64))]
    pub entry_symbols: Vec<SymbolId>,
    /// Whether the model covers every possible runtime entry point.
    pub complete: bool,
}

/// Entry-point summary retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntryPointSummaryV1_0 {
    /// Policy used for entry-point resolution.
    pub policy: EntryPointPolicyV1_0,
    /// Number of resolved entry points.
    pub entry_point_count: u32,
    /// Whether the model covers every possible runtime entry point.
    pub complete: bool,
}

/// One known blind spot in the reachability analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlindSpot {
    /// Source-free blind-spot category label.
    #[schemars(length(min = 1, max = 256))]
    pub category: String,
    /// Number of symbols potentially affected.
    pub affected_count: u32,
}

/// One applied false-positive suppression rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSummary {
    /// Rule identifier or annotation pattern.
    #[schemars(length(min = 1, max = 256))]
    pub rule: String,
    /// Number of symbols suppressed by this rule.
    pub suppressed_count: u32,
}

/// `code.dead` static reachability result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeDeadData {
    /// Ranked static reachability observations that require human review.
    #[schemars(length(max = 500))]
    pub candidates: Vec<DeadCandidate>,
    /// Entry-point model summary.
    pub entry_points: EntryPointSummary,
    /// Known analysis blind spots.
    #[schemars(length(max = 32))]
    pub blind_spots: Vec<BlindSpot>,
    /// Applied false-positive suppression rules.
    #[schemars(length(max = 32))]
    pub false_positive_controls: Vec<RuleSummary>,
    /// Source-free coverage caveats governing negative conclusions.
    #[schemars(length(max = 32), inner(length(min = 1, max = 256)))]
    pub coverage_caveats: Vec<String>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `code.dead` result data retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeDeadDataV1_0 {
    /// Ranked static reachability observations that require human review.
    #[schemars(length(max = 500))]
    pub candidates: Vec<DeadCandidateV1_0>,
    /// Entry-point model summary.
    pub entry_points: EntryPointSummaryV1_0,
    /// Known analysis blind spots.
    #[schemars(length(max = 32))]
    pub blind_spots: Vec<BlindSpot>,
    /// Applied false-positive suppression rules.
    #[schemars(length(max = 32))]
    pub false_positive_controls: Vec<RuleSummary>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked `code.dead` output retained for explicit 1.0 callers.
pub type CodeDeadOutputV1_0 = ToolResponse<ReadEnvelope<CodeDeadDataV1_0>>;

/// Current checked `code.dead` output.
pub type CodeDeadOutputV1_1 = AnalysisToolResponse<AnalysisReadEnvelope<CodeDeadData>>;

/// Current checked success-or-error output for `code.dead`.
pub type CodeDeadOutput = CodeDeadOutputV1_1;
