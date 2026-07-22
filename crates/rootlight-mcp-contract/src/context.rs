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

use crate::vertical::{
    ContinuationCursor, EntityKind, GenerationSelector, ReadEnvelope, RepositorySelector,
    RequiredNullable, ResponseBudget, ResponseProfile, ResponseWarning, SourceFreeMessage,
    ToolResponse, UsageSummary,
};
use crate::{TrustClassification, completeness::LimitingResource};

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
    /// Representation profile; defaults to compact and never changes evidence truth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Progressive detail handle from a prior pack response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationCursor>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Role of one evidence item within the assembled context pack.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
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

/// Version of the objective-to-role policy enforced by `context.pack`.
///
/// A policy change alters pack completeness and is therefore bound into the
/// canonical request digest and pack identity.
pub const OBJECTIVE_ROLE_POLICY_VERSION: u32 = 1;

/// Task class inferred from the normalized `context.pack` objective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextPackObjective {
    /// Fix a defect in target behavior.
    BugFix,
    /// Restructure existing behavior without changing its contract.
    Refactor,
    /// Explain existing behavior.
    Explanation,
    /// Move behavior to a new API, platform, or representation.
    Migration,
    /// Review behavior, risk, or security.
    Review,
}

impl ContextPackObjective {
    /// Returns the roles that must be selected for a complete pack.
    #[must_use]
    pub const fn required_roles(self) -> &'static [EvidenceRole] {
        match self {
            Self::BugFix => &[
                EvidenceRole::Definition,
                EvidenceRole::Implementation,
                EvidenceRole::Test,
            ],
            Self::Refactor => &[
                EvidenceRole::Definition,
                EvidenceRole::Caller,
                EvidenceRole::Test,
            ],
            Self::Explanation => &[EvidenceRole::Definition, EvidenceRole::Architecture],
            Self::Migration => &[
                EvidenceRole::Definition,
                EvidenceRole::Caller,
                EvidenceRole::Change,
            ],
            Self::Review => &[
                EvidenceRole::Change,
                EvidenceRole::Definition,
                EvidenceRole::Risk,
            ],
        }
    }

    /// Returns the roles accepted but not required for a complete pack.
    #[must_use]
    pub const fn optional_roles(self) -> &'static [EvidenceRole] {
        match self {
            Self::BugFix => &[
                EvidenceRole::Caller,
                EvidenceRole::Risk,
                EvidenceRole::Architecture,
                EvidenceRole::Change,
            ],
            Self::Refactor => &[
                EvidenceRole::Implementation,
                EvidenceRole::Risk,
                EvidenceRole::Architecture,
                EvidenceRole::Change,
            ],
            Self::Explanation => &[
                EvidenceRole::Implementation,
                EvidenceRole::Caller,
                EvidenceRole::Test,
                EvidenceRole::Risk,
                EvidenceRole::Change,
            ],
            Self::Migration => &[
                EvidenceRole::Implementation,
                EvidenceRole::Test,
                EvidenceRole::Risk,
                EvidenceRole::Architecture,
            ],
            Self::Review => &[
                EvidenceRole::Implementation,
                EvidenceRole::Caller,
                EvidenceRole::Test,
                EvidenceRole::Architecture,
            ],
        }
    }
}

/// Whether one evidence role is mandatory for the inferred objective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleRequirement {
    /// The pack is incomplete unless at least one item is selected.
    Required,
    /// The role may improve the pack but does not determine completeness.
    Optional,
}

/// Selection outcome for one objective role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleCoverageStatus {
    /// At least one candidate with this role was selected.
    Satisfied,
    /// A required role has no selected candidate.
    MissingRequired,
    /// An optional role has no selected candidate.
    OptionalAbsent,
}

/// Stable observed reason that a required role is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissingRequiredRoleReason {
    /// No provider invocation or omission observation exists for the role.
    NotSearched,
    /// The provider completed an exhaustive search without matching evidence.
    NoEvidence,
    /// The provider does not support the requested evidence domain.
    Unsupported,
    /// The provider was expected to support the domain but was unavailable.
    Unavailable,
    /// A provider resource limit stopped an otherwise supported search.
    Truncated,
    /// Matching evidence was filtered below the admitted confidence threshold.
    LowConfidence,
    /// Provider admission or final pack selection exhausted the shared budget.
    Budget,
}

/// Profile-independent coverage facts for one accepted evidence role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleCoverageEntry {
    /// Evidence role evaluated under the objective policy.
    pub role: EvidenceRole,
    /// Whether the objective requires this role.
    pub requirement: RoleRequirement,
    /// Selected, missing-required, or optional-absent state.
    pub status: RoleCoverageStatus,
    /// Number of typed provider candidates observed before selection.
    #[schemars(range(max = 100_000))]
    pub observed_candidates: u32,
    /// Number of candidates retained in the final pack.
    #[schemars(range(max = 200))]
    pub selected_items: u16,
    /// Stable reason for a missing required role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_reason: Option<MissingRequiredRoleReason>,
}

/// Objective-policy and required-role truth retained under every profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "UncheckedRoleCoverageSummary")]
pub struct RoleCoverageSummary {
    /// Inferred task objective whose policy was evaluated.
    objective: ContextPackObjective,
    /// Version of the objective-to-role rules.
    #[schemars(range(min = 1, max = 1000))]
    objective_rule_version: u32,
    /// Derived truth: every required entry is satisfied.
    complete: bool,
    /// Exactly one deterministic entry for each accepted role.
    #[schemars(length(min = 7, max = 7))]
    roles: Vec<RoleCoverageEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UncheckedRoleCoverageSummary {
    objective: ContextPackObjective,
    #[schemars(range(min = 1, max = 1000))]
    objective_rule_version: u32,
    complete: bool,
    #[schemars(length(min = 7, max = 7))]
    roles: Vec<RoleCoverageEntry>,
}

/// Semantic validation failure for role-coverage output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RoleCoverageError {
    /// Entries do not exactly match the accepted objective policy.
    #[error("role coverage does not match the objective role policy")]
    PolicyMismatch,
    /// Serialized completeness differs from required-role entries.
    #[error("role coverage complete flag is not derived from required entries")]
    InconsistentCompleteness,
}

impl RoleCoverageSummary {
    /// Creates a summary and derives completeness from required entries.
    ///
    /// # Errors
    ///
    /// Returns [`RoleCoverageError`] when roles are missing, duplicated, or
    /// inconsistent with the selected objective policy.
    pub fn new(
        objective: ContextPackObjective,
        roles: Vec<RoleCoverageEntry>,
    ) -> Result<Self, RoleCoverageError> {
        validate_role_entries(objective, &roles)?;
        let complete = roles.iter().all(|entry| {
            entry.requirement != RoleRequirement::Required
                || entry.status == RoleCoverageStatus::Satisfied
        });
        Ok(Self {
            objective,
            objective_rule_version: OBJECTIVE_ROLE_POLICY_VERSION,
            complete,
            roles,
        })
    }

    /// Returns the inferred task objective.
    #[must_use]
    pub const fn objective(&self) -> ContextPackObjective {
        self.objective
    }

    /// Returns the role-policy version bound into the pack identity.
    #[must_use]
    pub const fn objective_rule_version(&self) -> u32 {
        self.objective_rule_version
    }

    /// Returns whether every required role has a selected item.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Returns deterministic per-role observations.
    #[must_use]
    pub fn roles(&self) -> &[RoleCoverageEntry] {
        &self.roles
    }
}

impl TryFrom<UncheckedRoleCoverageSummary> for RoleCoverageSummary {
    type Error = RoleCoverageError;

    fn try_from(value: UncheckedRoleCoverageSummary) -> Result<Self, Self::Error> {
        validate_role_entries(value.objective, &value.roles)?;
        let derived = value.roles.iter().all(|entry| {
            entry.requirement != RoleRequirement::Required
                || entry.status == RoleCoverageStatus::Satisfied
        });
        if value.complete != derived
            || value.objective_rule_version != OBJECTIVE_ROLE_POLICY_VERSION
        {
            return Err(RoleCoverageError::InconsistentCompleteness);
        }
        Ok(Self {
            objective: value.objective,
            objective_rule_version: value.objective_rule_version,
            complete: derived,
            roles: value.roles,
        })
    }
}

fn validate_role_entries(
    objective: ContextPackObjective,
    entries: &[RoleCoverageEntry],
) -> Result<(), RoleCoverageError> {
    const ALL_ROLES: [EvidenceRole; 7] = [
        EvidenceRole::Definition,
        EvidenceRole::Implementation,
        EvidenceRole::Caller,
        EvidenceRole::Test,
        EvidenceRole::Risk,
        EvidenceRole::Architecture,
        EvidenceRole::Change,
    ];
    if entries.len() != ALL_ROLES.len() {
        return Err(RoleCoverageError::PolicyMismatch);
    }
    for (entry, expected_role) in entries.iter().zip(ALL_ROLES) {
        let requirement = if objective.required_roles().contains(&expected_role) {
            RoleRequirement::Required
        } else {
            RoleRequirement::Optional
        };
        let valid_state = match (requirement, entry.status) {
            (RoleRequirement::Required, RoleCoverageStatus::Satisfied) => {
                entry.selected_items > 0 && entry.missing_reason.is_none()
            }
            (RoleRequirement::Required, RoleCoverageStatus::MissingRequired) => {
                entry.selected_items == 0 && entry.missing_reason.is_some()
            }
            (RoleRequirement::Optional, RoleCoverageStatus::Satisfied) => {
                entry.selected_items > 0 && entry.missing_reason.is_none()
            }
            (RoleRequirement::Optional, RoleCoverageStatus::OptionalAbsent) => {
                entry.selected_items == 0 && entry.missing_reason.is_none()
            }
            _ => false,
        };
        if entry.role != expected_role
            || entry.requirement != requirement
            || u32::from(entry.selected_items) > entry.observed_candidates
            || !valid_state
        {
            return Err(RoleCoverageError::PolicyMismatch);
        }
    }
    Ok(())
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
    /// Bounded language label reported by the source provider.
    #[schemars(length(min = 1, max = 64))]
    pub language: String,
    /// Checked mechanism that produced the exact source bytes.
    pub provenance: SnippetProvenance,
    /// Whether the requested source range was reduced by a byte or token cap.
    pub truncated: bool,
    /// Trust classification for this repository-derived content.
    pub trust: TrustClassification,
}

/// Provenance of raw source bytes included in a context pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnippetProvenance {
    /// Bytes returned by generation-pinned `source.read`.
    SourceRead,
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
    /// Bounded repository-derived signature when the source policy permits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub signature: Option<String>,
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
    /// Evidence role affected by the omission, when role-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<EvidenceRole>,
    /// Source-free reason code for the omission.
    pub reason: SafeLabel,
    /// Provider domain that observed the omission, when provider-specific.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<SafeLabel>,
    /// Number of evidence items excluded for this reason.
    #[schemars(range(max = 100_000))]
    pub count: u32,
    /// Exact resources that prevented inclusion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 16))]
    pub limiting_resources: Vec<LimitingResource>,
    /// Whether another page of this exact request can retrieve the omission.
    #[serde(default)]
    pub resumable: bool,
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(length(max = 16))]
    pub by_section: BTreeMap<String, u32>,
}

/// `context.pack` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextPackData {
    /// Stable pack identifier for the exact generation, request, and planner.
    pub pack_id: ContextPackId,
    /// Full domain-separated digest of the canonical source-free request.
    #[schemars(length(min = 72, max = 72))]
    pub request_digest: String,
    /// Planner version bound into the request digest and pack identity.
    #[schemars(range(max = 1000))]
    pub planner_version: u32,
    /// Ordered, deduplicated evidence items in deterministic rank order.
    #[schemars(length(max = 200))]
    pub items: Vec<ContextItem>,
    /// Objective-specific required and optional role coverage.
    pub role_coverage: RoleCoverageSummary,
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
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[repr(u8)]
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
    /// Bounded change planning.
    #[serde(rename = "plan.change")]
    PlanChange,
    /// Context pack assembly.
    #[serde(rename = "context.pack")]
    ContextPack,
    /// Generation-pinned source range reads.
    #[serde(rename = "source.read")]
    SourceRead,
}

impl BatchTool {
    /// Complete public batch-tool catalog in stable wire-schema order.
    pub const ALL: [Self; 12] = [
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SymbolRelationships,
        Self::FlowTrace,
        Self::ChangeImpact,
        Self::TestsSelect,
        Self::ArchitectureOverview,
        Self::ArchitectureCycles,
        Self::CodeDead,
        Self::PlanChange,
        Self::ContextPack,
        Self::SourceRead,
    ];

    /// Stable dotted tool name used by the public batch wire contract.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CodeLocate => "code.locate",
            Self::SymbolExplain => "symbol.explain",
            Self::SymbolRelationships => "symbol.relationships",
            Self::FlowTrace => "flow.trace",
            Self::ChangeImpact => "change.impact",
            Self::TestsSelect => "tests.select",
            Self::ArchitectureOverview => "architecture.overview",
            Self::ArchitectureCycles => "architecture.cycles",
            Self::CodeDead => "code.dead",
            Self::PlanChange => "plan.change",
            Self::ContextPack => "context.pack",
            Self::SourceRead => "source.read",
        }
    }
}

/// A restricted typed binding from one declared dependency operation.
///
/// The legacy-compatible `pointer` spelling is retained for the `1.0` wire
/// contract, but only paths translated by the versioned typed binding registry
/// are accepted. It is not a general JSON Pointer: wildcards, filters,
/// expressions, templates, array expansion, envelope metadata, warnings, and
/// repository-controlled free text are forbidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchBinding {
    /// Operation identifier of the declared dependency to read from.
    #[serde(rename = "$from")]
    #[schemars(length(min = 1, max = 32))]
    pub from: String,
    /// Registry-reviewed compatibility path naming one typed output slot.
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
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
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
    /// The batch plan was validated in explain mode without executing operations.
    Planned,
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
    /// The operation was not scheduled because the shared budget was exhausted.
    NotRunBudget,
    /// The operation was cancelled after the batch plan had been accepted.
    Cancelled,
    /// The operation was planned in explain mode and not executed.
    NotRun,
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
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
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
    /// Reference to a typed scalar in `query.advanced.parameters`.
    ///
    /// Parameter references are accepted only in value positions and are
    /// replaced with their typed value before the query reaches the daemon.
    Parameter {
        /// Parameter name using the portable identifier grammar.
        #[schemars(length(min = 1, max = 64))]
        name: String,
    },
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
    /// Traverse graph edges from a seed symbol.
    Traverse {
        /// Seed symbol identifier for the traversal origin.
        seed: SymbolId,
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
    /// Planner version that produced the plan; part of the fingerprint input.
    #[schemars(range(max = 1000))]
    pub planner_version: u32,
    /// Stable physical-plan fingerprint bound to the plan and pinned generation.
    #[schemars(length(max = 128))]
    pub fingerprint: String,
}

/// Version of the source-free planner that produces explain plans.
///
/// Bumped whenever plan construction changes meaningfully so fingerprints taken
/// under different planner versions never collide.
pub const PLANNER_VERSION: u32 = 2;

impl PlanExplanation {
    /// Creates a plan explanation carrying the current planner version and an
    /// empty fingerprint. The domain layer binds the stable fingerprint to a
    /// pinned generation before the plan is exposed.
    #[must_use]
    pub fn new(estimated_cost: u64, operators: Vec<String>, applied_limits: Vec<String>) -> Self {
        Self {
            estimated_cost,
            operators,
            applied_limits,
            planner_version: PLANNER_VERSION,
            fingerprint: String::new(),
        }
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        ContextPackObjective, EvidenceRole, OBJECTIVE_ROLE_POLICY_VERSION, PLANNER_VERSION,
        PlanExplanation, RoleCoverageEntry, RoleCoverageStatus, RoleCoverageSummary,
        RoleRequirement,
    };

    #[test]
    fn new_plans_carry_the_current_planner_version() {
        let plan = PlanExplanation::new(1, vec!["catalog_snapshot".to_owned()], Vec::new());

        assert_eq!(PLANNER_VERSION, 2);
        assert_eq!(plan.planner_version, PLANNER_VERSION);
    }

    #[test]
    fn objective_role_policy_is_versioned_complete_and_disjoint() {
        let all_roles = BTreeSet::from([
            EvidenceRole::Definition,
            EvidenceRole::Implementation,
            EvidenceRole::Caller,
            EvidenceRole::Test,
            EvidenceRole::Risk,
            EvidenceRole::Architecture,
            EvidenceRole::Change,
        ]);
        let cases = [
            (
                ContextPackObjective::BugFix,
                vec![
                    EvidenceRole::Definition,
                    EvidenceRole::Implementation,
                    EvidenceRole::Test,
                ],
                vec![
                    EvidenceRole::Caller,
                    EvidenceRole::Risk,
                    EvidenceRole::Architecture,
                    EvidenceRole::Change,
                ],
            ),
            (
                ContextPackObjective::Refactor,
                vec![
                    EvidenceRole::Definition,
                    EvidenceRole::Caller,
                    EvidenceRole::Test,
                ],
                vec![
                    EvidenceRole::Implementation,
                    EvidenceRole::Risk,
                    EvidenceRole::Architecture,
                    EvidenceRole::Change,
                ],
            ),
            (
                ContextPackObjective::Explanation,
                vec![EvidenceRole::Definition, EvidenceRole::Architecture],
                vec![
                    EvidenceRole::Implementation,
                    EvidenceRole::Caller,
                    EvidenceRole::Test,
                    EvidenceRole::Risk,
                    EvidenceRole::Change,
                ],
            ),
            (
                ContextPackObjective::Migration,
                vec![
                    EvidenceRole::Definition,
                    EvidenceRole::Caller,
                    EvidenceRole::Change,
                ],
                vec![
                    EvidenceRole::Implementation,
                    EvidenceRole::Test,
                    EvidenceRole::Risk,
                    EvidenceRole::Architecture,
                ],
            ),
            (
                ContextPackObjective::Review,
                vec![
                    EvidenceRole::Change,
                    EvidenceRole::Definition,
                    EvidenceRole::Risk,
                ],
                vec![
                    EvidenceRole::Implementation,
                    EvidenceRole::Caller,
                    EvidenceRole::Test,
                    EvidenceRole::Architecture,
                ],
            ),
        ];
        for (objective, expected_required, expected_optional) in cases {
            assert_eq!(objective.required_roles(), expected_required);
            assert_eq!(objective.optional_roles(), expected_optional);
            let required = objective
                .required_roles()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let optional = objective
                .optional_roles()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            assert!(required.is_disjoint(&optional));
            assert_eq!(
                required.union(&optional).copied().collect::<BTreeSet<_>>(),
                all_roles
            );
        }
        assert_eq!(OBJECTIVE_ROLE_POLICY_VERSION, 1);
    }

    #[test]
    fn role_coverage_deserialization_rejects_an_independent_complete_flag() {
        let objective = ContextPackObjective::BugFix;
        let roles = [
            EvidenceRole::Definition,
            EvidenceRole::Implementation,
            EvidenceRole::Caller,
            EvidenceRole::Test,
            EvidenceRole::Risk,
            EvidenceRole::Architecture,
            EvidenceRole::Change,
        ]
        .into_iter()
        .map(|role| RoleCoverageEntry {
            role,
            requirement: if objective.required_roles().contains(&role) {
                RoleRequirement::Required
            } else {
                RoleRequirement::Optional
            },
            status: RoleCoverageStatus::Satisfied,
            observed_candidates: 1,
            selected_items: 1,
            missing_reason: None,
        })
        .collect();
        let coverage = RoleCoverageSummary::new(objective, roles).expect("valid coverage summary");
        assert!(coverage.complete());
        assert_eq!(
            coverage.objective_rule_version(),
            OBJECTIVE_ROLE_POLICY_VERSION
        );

        let mut encoded = serde_json::to_value(coverage).expect("coverage serializes");
        encoded["complete"] = serde_json::Value::Bool(false);
        assert!(serde_json::from_value::<RoleCoverageSummary>(encoded).is_err());
    }
}
