//! Canonical capability registry for Rootlight's public MCP boundary.
//!
//! The registry records tool-level discovery facts and field/value-level
//! runtime dispositions. The capability gate combines these declarations with
//! the generated input schemas, so adding a field or enum value without
//! reviewing its runtime behavior fails deterministically. Declarations remain
//! an honest inventory, not a substitute for process-level acceptance tests.

use crate::ErrorCode;
use crate::batch::{BATCH_TOOL_REGISTRY, batch_descriptor_for_tool};
use crate::catalog::{ExposureProfile, McpTool};
use crate::vertical::ResponseProfile;
use serde::Serialize;

/// Namespaced MCP `_meta` key carrying Rootlight capability discovery.
pub const DISCOVERY_METADATA_KEY: &str = "rootlight/capabilities";

/// How a public capability is currently satisfied at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// Fully implemented and accepted with process evidence.
    Implemented,
    /// Rejected before execution with the attached stable public error.
    UnsupportedStableError,
    /// Available only within the attached bounded fallback.
    FallbackLimited,
    /// Accepted by the schema but not safely or observably implemented.
    Blocked,
}

impl CapabilityStatus {
    /// Returns the stable machine-readable status name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::UnsupportedStableError => "unsupported_stable_error",
            Self::FallbackLimited => "fallback_limited",
            Self::Blocked => "blocked",
        }
    }
}

/// An exception to a tool's default field disposition.
///
/// Rules use generated-schema paths such as `budget.max_tokens` and
/// `operations[].local_budget`. A value of `None` applies to the field and its
/// descendants; a value-specific rule takes precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRule {
    /// Generated-schema field path.
    pub path: &'static str,
    /// Optional JSON enum, const, or boolean value.
    pub value: Option<&'static str>,
    /// Runtime disposition for this field or value.
    pub status: CapabilityStatus,
    /// Stable public error returned before execution, when applicable.
    pub error_code: Option<ErrorCode>,
    /// Source-free explanation of the limitation or supported behavior.
    pub summary: &'static str,
}

impl CapabilityRule {
    /// Reports whether discovery should expose this rule as a limitation.
    ///
    /// Explicit accepted ancestors are review markers for fail-closed
    /// admission, not user-facing limitations.
    #[must_use]
    fn is_public_limitation(self) -> bool {
        self.status != CapabilityStatus::Implemented
            && !(self.status == CapabilityStatus::FallbackLimited
                && self.summary == ACCEPTED_FALLBACK_SUMMARY)
    }
}

/// Pagination behavior advertised for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationSemantics {
    /// The operation does not return a read result.
    NotApplicable,
    /// The cursor is authenticated and bound to the request shape.
    AuthenticatedCursor,
    /// The result is complete within its fixed construction bound.
    BoundedComplete,
    /// Truncation is explicit and callers must narrow the request.
    ExplicitTruncation,
    /// More detail is retrieved through a separately authenticated handle.
    ProgressiveHandle,
    /// Batch children preserve their own continuation semantics.
    ChildContinuations,
}

impl PaginationSemantics {
    /// Returns the stable machine-readable pagination name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::AuthenticatedCursor => "authenticated_cursor",
            Self::BoundedComplete => "bounded_complete",
            Self::ExplicitTruncation => "explicit_truncation",
            Self::ProgressiveHandle => "progressive_handle",
            Self::ChildContinuations => "child_continuations",
        }
    }
}

/// Generation behavior advertised for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationSemantics {
    /// The operation is not generation-bound.
    None,
    /// The operation creates and may publish a new generation.
    CreatesGeneration,
    /// The request selects an active or explicit immutable generation.
    SelectsGeneration,
    /// The request accepts a selector, but the current adapter returns active.
    ActiveGenerationFallback,
    /// Two immutable generations are selected for structural comparison.
    ComparesGenerations,
    /// A batch resolves and pins one selector for its nested operations.
    ///
    /// Field rules separately describe whether explicit historical selectors
    /// are currently available.
    BatchInherited,
}

impl GenerationSemantics {
    /// Returns the stable machine-readable generation name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CreatesGeneration => "creates_generation",
            Self::SelectsGeneration => "selects_generation",
            Self::ActiveGenerationFallback => "active_generation_fallback",
            Self::ComparesGenerations => "compares_generations",
            Self::BatchInherited => "batch_inherited",
        }
    }
}

/// Budget behavior advertised for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSemantics {
    /// The public input has no budget control.
    None,
    /// At least one request budget dimension is enforced.
    PerRequest,
    /// A dedicated token budget bounds the assembled response.
    TokenBudget,
    /// A schema-level budget exists but is rejected by the current executor.
    Unsupported,
}

impl BudgetSemantics {
    /// Returns the stable machine-readable budget name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::PerRequest => "per_request",
            Self::TokenBudget => "token_budget",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Public input field used to select a response representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProfileField {
    /// Canonical response-profile field spelling.
    ResponseProfile,
    /// Legacy field spelling retained by change-domain contracts.
    Profile,
}

impl ResponseProfileField {
    /// Returns the exact field name accepted by the tool's current wire contract.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ResponseProfile => "response_profile",
            Self::Profile => "profile",
        }
    }
}

/// Response representations truthfully available for one public tool.
///
/// Fixed tools have no profile selector. Selectable tools advertise the exact
/// current wire field, omission default, and closed set of accepted values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ResponseProfileSupport {
    /// The tool always returns one fixed representation.
    Fixed {
        /// Representation applied to every successful response.
        representation: ResponseProfile,
    },
    /// The caller may select from a closed set of representations.
    Selectable {
        /// Exact input field accepted by the current contract version.
        wire_field: ResponseProfileField,
        /// Closed set of accepted response profiles.
        supported: &'static [ResponseProfile],
        /// Profile applied when the selector is omitted.
        default: ResponseProfile,
    },
}

/// Safe machine-readable capability metadata exposed through MCP discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapabilityMetadata {
    /// Public tool contract version.
    pub contract_version: &'static str,
    /// Fingerprint of the reviewed generated input shape.
    pub input_shape_hash: &'static str,
    /// Aggregate runtime disposition.
    pub status: &'static str,
    /// Profiles in which the tool may be discovered.
    pub profiles: Vec<&'static str>,
    /// Response representations available independently of exposure profiles.
    pub response_profiles: ResponseProfileSupport,
    /// Whether the tool may be nested in a public batch.
    pub batch_eligible: bool,
    /// Whether bounded explain mode is served.
    pub explain_supported: bool,
    /// Pagination behavior.
    pub pagination: &'static str,
    /// Immutable-generation behavior.
    pub generation: &'static str,
    /// Request-budget behavior.
    pub budget: &'static str,
    /// Whether nested operations share one enforced child-execution budget.
    pub batch_shared_budget: bool,
    /// Concise source-free aggregate limitation.
    pub fallback_summary: &'static str,
    /// Versioned lifecycle semantics for tools that create operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<DiscoveryLifecycleMetadata>,
    /// Reviewed field or value limitations.
    pub limitations: Vec<DiscoveryCapabilityLimit>,
}

/// Machine-readable lifecycle semantics for an operation-creating tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryLifecycleMetadata {
    /// Version of this discovery-only lifecycle profile.
    pub version: &'static str,
    /// Whether an existing repository identity may select an update target.
    pub update_by_repository_id: bool,
    /// Closed indexing modes accepted by the fallback.
    pub accepted_modes: Vec<&'static str>,
    /// Closed indexing scope accepted by the fallback.
    pub scope: &'static str,
    /// Whether a successful call always returns a terminal result.
    pub synchronous_terminal: bool,
    /// Maximum attached call lifetime.
    pub max_wait_ms: u32,
    /// Whether work may outlive the submitting connection.
    pub detached: bool,
    /// Idempotency guarantee at the public tool boundary.
    pub public_idempotency: &'static str,
    /// Whether retrying the same internal operation identity is idempotent.
    pub internal_operation_retry: bool,
    /// Persistence scope for operation and generation state.
    pub state_persistence: &'static str,
    /// Required recovery action after process restart.
    pub restart_behavior: &'static str,
    /// Visibility guarantee for successful publication.
    pub publication: &'static str,
}

/// One safe field/value limitation exposed through discovery metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapabilityLimit {
    /// Generated-schema field path.
    pub field: &'static str,
    /// Closed value when the rule applies to one value only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<&'static str>,
    /// Runtime disposition.
    pub status: &'static str,
    /// Stable pre-execution error when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    /// Concise source-free explanation.
    pub summary: &'static str,
}

/// One tool's canonical capability entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCapability {
    /// The tool this entry describes.
    pub tool: McpTool,
    /// Contract schema version this entry is written against.
    pub contract_version: &'static str,
    /// SHA-256 of sorted generated input field paths and closed values.
    pub input_shape_hash: &'static str,
    /// Profiles that expose this tool, in ascending privilege order.
    pub profiles: &'static [ExposureProfile],
    /// Response representations accepted by the current public contract.
    pub response_profiles: ResponseProfileSupport,
    /// Whether the tool may appear inside a public `query.batch`.
    pub batch_eligible: bool,
    /// Whether the tool exposes a source-free explain plan.
    pub explain_supported: bool,
    /// Internal process handler used to route the public tool.
    ///
    /// This path is retained in evidence artifacts and is intentionally omitted
    /// from public discovery metadata.
    pub handler_path: Option<&'static str>,
    /// Honest aggregate runtime disposition.
    pub status: CapabilityStatus,
    /// Disposition inherited by fields without a more specific rule.
    pub default_field_status: CapabilityStatus,
    /// Field and value exceptions reviewed against the current executor.
    pub rules: &'static [CapabilityRule],
    /// Public pagination behavior.
    pub pagination: PaginationSemantics,
    /// Public generation behavior.
    pub generation: GenerationSemantics,
    /// Public budget behavior.
    pub budget: BudgetSemantics,
    /// Whether `query.batch` enforces one shared child-execution budget.
    pub batch_shared_budget: bool,
    /// Source-free, concise fallback description safe for discovery.
    pub fallback_summary: &'static str,
}

impl ToolCapability {
    /// Resolves the reviewed disposition for one generated field or value.
    ///
    /// Value-specific rules win over exact field rules, and exact field rules
    /// win over ancestor rules. Unlisted fields inherit the conservative tool
    /// default, while the shape hash ensures that schema additions still
    /// require an explicit registry review.
    #[must_use]
    pub fn disposition(self, path: &str, value: Option<&str>) -> CapabilityRule {
        if let Some(value) = value
            && let Some(rule) = self
                .rules
                .iter()
                .find(|rule| rule.path == path && rule.value == Some(value))
        {
            return *rule;
        }
        if let Some(rule) = self
            .rules
            .iter()
            .filter(|rule| rule.value.is_none() && path_is_within(path, rule.path))
            .max_by_key(|rule| rule.path.len())
        {
            return *rule;
        }
        CapabilityRule {
            path: "",
            value: None,
            status: self.default_field_status,
            error_code: None,
            summary: self.fallback_summary,
        }
    }
}

fn path_is_within(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with("[]"))
}

const SCOUT_PROFILES: &[ExposureProfile] = &[
    ExposureProfile::Scout,
    ExposureProfile::Analysis,
    ExposureProfile::Developer,
];
const ANALYSIS_PROFILES: &[ExposureProfile] =
    &[ExposureProfile::Analysis, ExposureProfile::Developer];
const DEVELOPER_PROFILES: &[ExposureProfile] = &[ExposureProfile::Developer];
const COMPACT_RESPONSE_PROFILES: &[ResponseProfile] = &[ResponseProfile::Compact];
const ANALYTICAL_RESPONSE_PROFILES: &[ResponseProfile] = &[
    ResponseProfile::Compact,
    ResponseProfile::Standard,
    ResponseProfile::Evidence,
];

const fn unsupported(path: &'static str, summary: &'static str) -> CapabilityRule {
    CapabilityRule {
        path,
        value: None,
        status: CapabilityStatus::UnsupportedStableError,
        error_code: Some(ErrorCode::UnsupportedCapability),
        summary,
    }
}

const fn unsupported_value(
    path: &'static str,
    value: &'static str,
    summary: &'static str,
) -> CapabilityRule {
    CapabilityRule {
        path,
        value: Some(value),
        status: CapabilityStatus::UnsupportedStableError,
        error_code: Some(ErrorCode::UnsupportedCapability),
        summary,
    }
}

const fn implemented(path: &'static str, summary: &'static str) -> CapabilityRule {
    CapabilityRule {
        path,
        value: None,
        status: CapabilityStatus::Implemented,
        error_code: None,
        summary,
    }
}

const fn implemented_value(
    path: &'static str,
    value: &'static str,
    summary: &'static str,
) -> CapabilityRule {
    CapabilityRule {
        path,
        value: Some(value),
        status: CapabilityStatus::Implemented,
        error_code: None,
        summary,
    }
}

const fn fallback_limited(path: &'static str, summary: &'static str) -> CapabilityRule {
    CapabilityRule {
        path,
        value: None,
        status: CapabilityStatus::FallbackLimited,
        error_code: None,
        summary,
    }
}

const ACCEPTED_FALLBACK_SUMMARY: &str =
    "accepted with the tool's documented bounded fallback semantics";

const fn accepted_fallback(path: &'static str) -> CapabilityRule {
    fallback_limited(path, ACCEPTED_FALLBACK_SUMMARY)
}

const fn blocked(path: &'static str, summary: &'static str) -> CapabilityRule {
    CapabilityRule {
        path,
        value: None,
        status: CapabilityStatus::Blocked,
        error_code: None,
        summary,
    }
}

const REPO_INDEX_RULES: &[CapabilityRule] = &[
    accepted_fallback("root"),
    accepted_fallback("mode"),
    implemented("detached", "omission or false selects attached execution"),
    unsupported_value(
        "detached",
        "true",
        "detached indexing is not served because operation handles are process-local",
    ),
    unsupported(
        "repository_id",
        "updating a registered repository is not served",
    ),
    unsupported("scope", "request-scoped index selection is not served"),
    unsupported(
        "requested_tiers",
        "explicit analysis-tier selection is not served",
    ),
    unsupported(
        "configuration_patch",
        "configuration patching is not served",
    ),
    unsupported("wait_ms", "synchronous index waiting is not served"),
    unsupported_value("mode", "deep", "deep indexing is not served"),
    unsupported_value("mode", "rebuild", "rebuild indexing is not served"),
];

const REPO_STATUS_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    implemented(
        "generation",
        "selects the active or one retained exact repository generation",
    ),
    implemented(
        "coverage_detail",
        "summary is the default and language returns bounded per-language coverage",
    ),
    implemented_value(
        "coverage_detail",
        "summary",
        "returns aggregate repository coverage",
    ),
    implemented_value(
        "coverage_detail",
        "language",
        "returns aggregate and bounded per-language coverage",
    ),
    unsupported_value(
        "coverage_detail",
        "project",
        "project coverage is not served because project boundaries are not indexed",
    ),
    unsupported_value(
        "coverage_detail",
        "file",
        "file coverage is not served because repo status has no file scope",
    ),
    implemented(
        "include_operations",
        "true returns bounded current and recent repository index operations",
    ),
    implemented(
        "require_freshness",
        "enforces structural or semantic freshness before returning status",
    ),
    accepted_fallback("response_profile"),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("budget", "custom response budgets are not served"),
    unsupported_value(
        "response_profile",
        "evidence",
        "only compact response projection is served",
    ),
    unsupported_value(
        "response_profile",
        "standard",
        "only compact response projection is served",
    ),
];

const REPO_LIST_RULES: &[CapabilityRule] = &[
    accepted_fallback("max_results"),
    accepted_fallback("response_profile"),
    implemented("explain", "returns a deterministic source-free plan"),
    implemented("cursor", "uses an authenticated request-bound continuation"),
    implemented(
        "query",
        "filters canonical display names and aliases before pagination",
    ),
    implemented(
        "states",
        "filters canonical lifecycle states before pagination",
    ),
    unsupported_value(
        "response_profile",
        "evidence",
        "only compact response projection is served",
    ),
    unsupported_value(
        "response_profile",
        "standard",
        "only compact response projection is served",
    ),
];

const OPERATION_STATUS_RULES: &[CapabilityRule] = &[
    implemented("operation_id", "selects the repository operation"),
    implemented("action", "selects status retrieval or cancellation"),
    implemented("wait_ms", "bounds the operation status wait"),
    implemented("after_revision", "waits for a newer operation revision"),
];

const CODE_LOCATE_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("query"),
    accepted_fallback("search_modes"),
    accepted_fallback("max_results"),
    implemented("budget", "reduces the common hard execution budget"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    implemented("cursor", "uses an authenticated request-bound continuation"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("kinds", "kind filtering is not served"),
    unsupported("scope", "structural scope filtering is not served"),
    unsupported("languages", "language filtering is not served"),
    unsupported(
        "related_to",
        "relationship-constrained lookup is not served",
    ),
    unsupported("min_confidence", "confidence filtering is not served"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported_value(
        "search_modes[]",
        "docs",
        "documentation search is not served",
    ),
    unsupported_value("search_modes[]", "path", "path search is not served"),
    unsupported_value(
        "search_modes[]",
        "semantic",
        "semantic search is not served",
    ),
    unsupported_value(
        "search_modes[]",
        "structural",
        "structural search is not served",
    ),
];

const SYMBOL_EXPLAIN_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("symbol_ids"),
    accepted_fallback("include_provenance"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("sections", "section selection is not served"),
    unsupported(
        "relation_sample_limit",
        "custom relation samples are not served",
    ),
    unsupported(
        "source_preview_lines",
        "custom source previews are not served",
    ),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported_value(
        "include_provenance",
        "full",
        "full provenance projection is not served",
    ),
];

const SYMBOL_RELATIONSHIPS_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("symbol_ids"),
    accepted_fallback("relations"),
    accepted_fallback("direction"),
    accepted_fallback("min_confidence"),
    accepted_fallback("include_candidates"),
    accepted_fallback("max_results"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    implemented("cursor", "uses an authenticated request-bound continuation"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    unsupported_value(
        "include_candidates",
        "true",
        "ambiguous candidate projection is not served",
    ),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
];

const FLOW_TRACE_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("from"),
    accepted_fallback("to"),
    accepted_fallback("relations"),
    accepted_fallback("direction"),
    accepted_fallback("max_depth"),
    accepted_fallback("max_paths"),
    accepted_fallback("min_confidence"),
    accepted_fallback("cross_repository"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported_value(
        "cross_repository",
        "true",
        "cross-repository traversal is not served",
    ),
    unsupported(
        "path_policy",
        "explicit path selection policy is not served",
    ),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported("from.route_id", "route endpoints are not served"),
    unsupported("from.service_id", "service endpoints are not served"),
    unsupported(
        "from.database_object_id",
        "database endpoints are not served",
    ),
    unsupported("to.route_id", "route endpoints are not served"),
    unsupported("to.service_id", "service endpoints are not served"),
    unsupported("to.database_object_id", "database endpoints are not served"),
];

const CHANGE_IMPACT_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("change"),
    accepted_fallback("relation_policy"),
    accepted_fallback("max_depth"),
    accepted_fallback("include_tests"),
    accepted_fallback("include_history"),
    accepted_fallback("min_confidence"),
    implemented(
        "profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported(
        "change.working_tree",
        "working-tree diff resolution is not served",
    ),
    unsupported(
        "change.revision_range",
        "revision-range resolution is not served",
    ),
    unsupported_value(
        "include_history",
        "true",
        "history-derived signals are not served",
    ),
    unsupported_value(
        "relation_policy",
        "conservative",
        "conservative relation expansion is not served",
    ),
];

const TESTS_SELECT_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("seeds"),
    accepted_fallback("test_kinds"),
    accepted_fallback("max_tests"),
    accepted_fallback("include_commands"),
    implemented(
        "profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported("execution_budget", "execution-time budgeting is not served"),
    unsupported("frameworks", "framework filtering is not served"),
    unsupported("seeds.paths", "path seeds are not served"),
    unsupported("seeds.change", "change seeds are not served"),
    unsupported("seeds.build_targets", "build-target seeds are not served"),
    unsupported_value(
        "test_kinds[]",
        "integration",
        "integration-test classification is not served",
    ),
    unsupported_value(
        "test_kinds[]",
        "e2e",
        "end-to-end test classification is not served",
    ),
    unsupported_value(
        "test_kinds[]",
        "contract",
        "contract-test classification is not served",
    ),
    unsupported_value(
        "test_kinds[]",
        "static",
        "static-check classification is not served",
    ),
    unsupported_value(
        "test_kinds[]",
        "build",
        "build-check classification is not served",
    ),
];

const ARCHITECTURE_OVERVIEW_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("views"),
    accepted_fallback("max_components"),
    accepted_fallback("include_edges"),
    accepted_fallback("min_confidence"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    unsupported("detail", "explicit detail projection is not served"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported_value("views[]", "build", "build view is not served"),
    unsupported_value("views[]", "communities", "community view is not served"),
    unsupported_value("views[]", "data", "data view is not served"),
    unsupported_value("views[]", "modules", "module view is not served"),
    unsupported_value("views[]", "ownership", "ownership view is not served"),
    unsupported_value("views[]", "packages", "package view is not served"),
    unsupported_value("views[]", "services", "service view is not served"),
];

const ARCHITECTURE_CYCLES_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("projection"),
    accepted_fallback("min_size"),
    accepted_fallback("max_cycles"),
    accepted_fallback("include_self_cycles"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    unsupported(
        "projection.level",
        "only symbol-level cycle projection is served",
    ),
    implemented_value(
        "projection.level",
        "symbol",
        "detects cycles between symbols",
    ),
    unsupported("rank_by", "cycle ranking strategy is not served"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
];

const CODE_DEAD_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("entry_point_policy"),
    accepted_fallback("include_exported"),
    accepted_fallback("include_tests"),
    accepted_fallback("min_confidence"),
    accepted_fallback("max_candidates"),
    implemented(
        "response_profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
];

const HISTORY_COMPARE_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("base"),
    accepted_fallback("head"),
    accepted_fallback("change_kinds"),
    accepted_fallback("max_results"),
    accepted_fallback("include_unchanged_context"),
    accepted_fallback("profile"),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("scope", "structural scope filtering is not served"),
    unsupported_value(
        "include_unchanged_context",
        "true",
        "unchanged-context projection is not served",
    ),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported("base.git", "git revision resolution is not served"),
    unsupported("head.git", "git revision resolution is not served"),
    unsupported_value(
        "change_kinds[]",
        "relations",
        "relation delta comparison is not served",
    ),
    unsupported_value(
        "change_kinds[]",
        "architecture",
        "architecture delta comparison is not served",
    ),
    unsupported_value(
        "change_kinds[]",
        "ownership",
        "ownership delta comparison is not served",
    ),
    unsupported_value(
        "change_kinds[]",
        "tests",
        "test delta comparison is not served",
    ),
    unsupported_value(
        "change_kinds[]",
        "routes",
        "route delta comparison is not served",
    ),
    unsupported_value(
        "change_kinds[]",
        "data",
        "data-schema delta comparison is not served",
    ),
    unsupported_value(
        "profile",
        "evidence",
        "the current output has no optional evidence representation",
    ),
    unsupported_value(
        "profile",
        "standard",
        "the current output has no optional standard representation",
    ),
];

const PLAN_CHANGE_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("objective"),
    implemented(
        "objective_text",
        "is preserved as a caller-authored outcome to validate in the first plan step",
    ),
    accepted_fallback("targets"),
    accepted_fallback("max_steps"),
    implemented(
        "profile",
        "selects compact, standard, or bounded evidence representation",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("change_context", "change-context resolution is not served"),
    unsupported("constraints", "user constraint evaluation is not served"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
];

const CONTEXT_PACK_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("task"),
    accepted_fallback("seeds"),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    unsupported("seeds.paths", "path seeds are not served"),
    unsupported("seeds.routes", "route seeds are not served"),
    unsupported("seeds.located", "located-result seeds are not served"),
    unsupported("seeds.change", "change seeds are not served"),
    unsupported("seeds.plan", "plan seeds are not served"),
    implemented(
        "source_policy",
        "controls references, signatures, or bounded source snippets",
    ),
    implemented("sections", "selects compatible typed evidence roles"),
    implemented(
        "diversity",
        "biases optional evidence without displacing required roles",
    ),
    implemented(
        "min_confidence",
        "filters evidence below the inclusive confidence threshold",
    ),
    implemented(
        "response_profile",
        "selects compact, standard, or evidence source representation",
    ),
    implemented(
        "continuation",
        "resumes authenticated request-bound evidence frontiers",
    ),
    implemented("token_budget", "bounds assembled context tokens"),
];

const SOURCE_READ_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("references"),
    implemented(
        "merge_overlaps",
        "canonically merges overlapping immutable ranges",
    ),
    implemented(
        "include_line_numbers",
        "selects optional one-based line metadata",
    ),
    implemented("encoding", "selects exact UTF-8 or explicit base64 bytes"),
    accepted_fallback("response_profile"),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    implemented(
        "context_lines_before",
        "expands UTF-8 selections by bounded leading lines",
    ),
    implemented(
        "context_lines_after",
        "expands UTF-8 selections by bounded trailing lines",
    ),
    unsupported(
        "references[].symbol_id",
        "symbol selectors are not served by source reads",
    ),
    unsupported(
        "references[].file_id",
        "file range selectors are not served by source reads",
    ),
    unsupported(
        "references[].start_byte",
        "file range selectors are not served by source reads",
    ),
    unsupported(
        "references[].end_byte",
        "file range selectors are not served by source reads",
    ),
    implemented("max_source_bytes", "reduces the common source-byte ceiling"),
    implemented("budget", "reduces the common hard execution budget"),
    unsupported("budget.evidence_level", "evidence projection is not served"),
    unsupported_value(
        "response_profile",
        "evidence",
        "only compact response projection is served",
    ),
    unsupported_value(
        "response_profile",
        "standard",
        "only compact response projection is served",
    ),
];

const QUERY_ADVANCED_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("generation"),
    accepted_fallback("query"),
    implemented("explain", "returns a deterministic source-free plan"),
    implemented("cursor", "uses an authenticated request-bound continuation"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    implemented(
        "parameters",
        "binds typed scalars only at AST value positions before execution",
    ),
    implemented(
        "cost_limit",
        "rejects plans above the requested static cost ceiling",
    ),
    implemented("max_results", "bounds returned query rows"),
    implemented("max_depth", "bounds accepted query nesting"),
];

const QUERY_BATCH_RULES: &[CapabilityRule] = &[
    accepted_fallback("repository"),
    accepted_fallback("operations"),
    accepted_fallback("failure_policy"),
    implemented(
        "response_profile",
        "propagates compact, standard, or evidence representation to compatible child tools",
    ),
    implemented("explain", "returns a deterministic source-free plan"),
    unsupported(
        "repository.alias",
        "only stable repository identifiers are served",
    ),
    fallback_limited(
        "generation",
        "active is resolved and pinned once; non-active explicit generations fail closed until retained-generation lookup is available",
    ),
    implemented_value(
        "generation",
        "active",
        "resolves and pins the active generation once for all nested operations",
    ),
    blocked(
        "budget",
        "measured child usage is bounded but orchestration and response serialization are not fully charged",
    ),
    blocked(
        "operations[].local_budget",
        "intersects representable caps and rejects unsupported child dimensions before work",
    ),
    implemented(
        "operations[].local_budget.timeout_ms",
        "bounds every child call by the lower local deadline",
    ),
];

/// The closed set of tools permitted inside a public `query.batch`.
pub const BATCH_ELIGIBLE: [McpTool; 12] = build_batch_eligible();

const fn build_batch_eligible() -> [McpTool; 12] {
    let mut tools = [McpTool::CodeLocate; 12];
    let mut index = 0;
    while index < BATCH_TOOL_REGISTRY.len() {
        tools[index] = BATCH_TOOL_REGISTRY[index].tool;
        index += 1;
    }
    tools
}

/// Reports whether a tool is permitted inside a public batch.
#[must_use]
pub const fn is_batch_eligible(tool: McpTool) -> bool {
    match batch_descriptor_for_tool(tool) {
        Some(descriptor) => descriptor.eligible,
        None => false,
    }
}

/// The canonical capability registry, one entry per tool in catalog order.
pub const CAPABILITIES: [ToolCapability; 19] = build_capabilities();

/// Returns the canonical capability entry for one catalog tool.
#[must_use]
pub const fn capability_for(tool: McpTool) -> &'static ToolCapability {
    &CAPABILITIES[tool_as_u8(tool) as usize]
}

/// Builds the source-free capability metadata served through `tools/list`.
#[must_use]
pub fn discovery_metadata(tool: McpTool) -> DiscoveryCapabilityMetadata {
    let capability = capability_for(tool);
    let limitations = capability
        .rules
        .iter()
        .filter(|rule| rule.is_public_limitation())
        .map(|rule| DiscoveryCapabilityLimit {
            field: rule.path,
            value: rule.value,
            status: rule.status.name(),
            error_code: rule.error_code,
            summary: rule.summary,
        })
        .collect();
    DiscoveryCapabilityMetadata {
        contract_version: capability.contract_version,
        input_shape_hash: capability.input_shape_hash,
        status: capability.status.name(),
        profiles: capability
            .profiles
            .iter()
            .map(|profile| profile.name())
            .collect(),
        response_profiles: capability.response_profiles,
        batch_eligible: capability.batch_eligible,
        explain_supported: capability.explain_supported,
        pagination: capability.pagination.name(),
        generation: capability.generation.name(),
        budget: capability.budget.name(),
        batch_shared_budget: capability.batch_shared_budget,
        fallback_summary: capability.fallback_summary,
        lifecycle: lifecycle_metadata(tool),
        limitations,
    }
}

fn lifecycle_metadata(tool: McpTool) -> Option<DiscoveryLifecycleMetadata> {
    (tool == McpTool::RepoIndex).then_some(DiscoveryLifecycleMetadata {
        version: "1.0",
        update_by_repository_id: false,
        accepted_modes: vec!["auto", "structural"],
        scope: "whole_repository",
        synchronous_terminal: true,
        max_wait_ms: 30_000,
        detached: false,
        public_idempotency: "none",
        internal_operation_retry: true,
        state_persistence: "process_local",
        restart_behavior: "reindex_required",
        publication: "atomic_on_terminal_success",
    })
}

const fn build_capabilities() -> [ToolCapability; 19] {
    let mut entries = [tool_capability(McpTool::RepoIndex); 19];
    let mut index = 0;
    while index < McpTool::ALL.len() {
        entries[index] = tool_capability(McpTool::ALL[index]);
        index += 1;
    }
    entries
}

const fn tool_capability(tool: McpTool) -> ToolCapability {
    ToolCapability {
        tool,
        contract_version: tool.contract_version(),
        input_shape_hash: input_shape_hash(tool),
        profiles: tool_profiles(tool),
        response_profiles: response_profile_support(tool),
        batch_eligible: is_batch_eligible(tool),
        explain_supported: !matches!(tool, McpTool::RepoIndex | McpTool::OperationStatus),
        handler_path: Some(handler_path(tool)),
        status: tool_status(tool),
        default_field_status: CapabilityStatus::Blocked,
        rules: tool_rules(tool),
        pagination: pagination_semantics(tool),
        generation: generation_semantics(tool),
        budget: budget_semantics(tool),
        batch_shared_budget: matches!(tool, McpTool::QueryBatch),
        fallback_summary: tool_fallback_summary(tool),
    }
}

const fn handler_path(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => "rootlight-mcp::executor::execute_repository_index",
        McpTool::RepoStatus => "rootlight-mcp::executor::execute_repo_status",
        McpTool::RepoList => "rootlight-mcp::executor::execute_repo_list",
        McpTool::OperationStatus => "rootlight-mcp::executor::execute_operation_status",
        McpTool::CodeLocate => "rootlight-mcp::executor::execute_code_locate",
        McpTool::SymbolExplain => "rootlight-mcp::executor::execute_symbol_explain",
        McpTool::SymbolRelationships => "rootlight-mcp::executor::execute_symbol_relationships",
        McpTool::FlowTrace => "rootlight-mcp::executor::execute_flow_trace",
        McpTool::ChangeImpact => "rootlight-mcp::executor::execute_change_impact",
        McpTool::TestsSelect => "rootlight-mcp::executor::execute_tests_select",
        McpTool::ArchitectureOverview => "rootlight-mcp::executor::execute_architecture_overview",
        McpTool::ArchitectureCycles => "rootlight-mcp::executor::execute_architecture_cycles",
        McpTool::CodeDead => "rootlight-mcp::executor::execute_code_dead",
        McpTool::HistoryCompare => "rootlight-mcp::executor::execute_history_compare",
        McpTool::PlanChange => "rootlight-mcp::executor::execute_plan_change",
        McpTool::ContextPack => "rootlight-mcp::executor::execute_context_pack",
        McpTool::SourceRead => "rootlight-mcp::executor::execute_source_read",
        McpTool::QueryAdvanced => "rootlight-mcp::executor::execute_query_advanced",
        McpTool::QueryBatch => "rootlight-mcp::executor::execute_query_batch",
    }
}

const fn tool_status(tool: McpTool) -> CapabilityStatus {
    match tool {
        McpTool::OperationStatus => CapabilityStatus::Implemented,
        _ => CapabilityStatus::FallbackLimited,
    }
}

const fn tool_profiles(tool: McpTool) -> &'static [ExposureProfile] {
    match tool {
        McpTool::RepoStatus
        | McpTool::CodeLocate
        | McpTool::SymbolExplain
        | McpTool::ContextPack
        | McpTool::SourceRead
        | McpTool::QueryBatch => SCOUT_PROFILES,
        McpTool::SymbolRelationships
        | McpTool::FlowTrace
        | McpTool::ChangeImpact
        | McpTool::TestsSelect
        | McpTool::ArchitectureOverview
        | McpTool::ArchitectureCycles
        | McpTool::CodeDead => ANALYSIS_PROFILES,
        _ => DEVELOPER_PROFILES,
    }
}

const fn response_profile_support(tool: McpTool) -> ResponseProfileSupport {
    match tool {
        McpTool::RepoIndex | McpTool::OperationStatus | McpTool::QueryAdvanced => {
            ResponseProfileSupport::Fixed {
                representation: ResponseProfile::Compact,
            }
        }
        McpTool::RepoStatus | McpTool::RepoList | McpTool::SourceRead => {
            ResponseProfileSupport::Selectable {
                wire_field: ResponseProfileField::ResponseProfile,
                supported: COMPACT_RESPONSE_PROFILES,
                default: ResponseProfile::Compact,
            }
        }
        McpTool::QueryBatch => ResponseProfileSupport::Selectable {
            wire_field: ResponseProfileField::ResponseProfile,
            supported: ANALYTICAL_RESPONSE_PROFILES,
            default: ResponseProfile::Compact,
        },
        McpTool::ContextPack => ResponseProfileSupport::Selectable {
            wire_field: ResponseProfileField::ResponseProfile,
            supported: ANALYTICAL_RESPONSE_PROFILES,
            default: ResponseProfile::Compact,
        },
        McpTool::HistoryCompare => ResponseProfileSupport::Selectable {
            wire_field: ResponseProfileField::Profile,
            supported: COMPACT_RESPONSE_PROFILES,
            default: ResponseProfile::Compact,
        },
        McpTool::ChangeImpact | McpTool::TestsSelect | McpTool::PlanChange => {
            ResponseProfileSupport::Selectable {
                wire_field: ResponseProfileField::Profile,
                supported: ANALYTICAL_RESPONSE_PROFILES,
                default: ResponseProfile::Compact,
            }
        }
        McpTool::CodeLocate
        | McpTool::SymbolExplain
        | McpTool::SymbolRelationships
        | McpTool::FlowTrace
        | McpTool::ArchitectureOverview
        | McpTool::ArchitectureCycles
        | McpTool::CodeDead => ResponseProfileSupport::Selectable {
            wire_field: ResponseProfileField::ResponseProfile,
            supported: ANALYTICAL_RESPONSE_PROFILES,
            default: ResponseProfile::Compact,
        },
    }
}

const fn tool_rules(tool: McpTool) -> &'static [CapabilityRule] {
    match tool {
        McpTool::RepoIndex => REPO_INDEX_RULES,
        McpTool::RepoStatus => REPO_STATUS_RULES,
        McpTool::RepoList => REPO_LIST_RULES,
        McpTool::OperationStatus => OPERATION_STATUS_RULES,
        McpTool::CodeLocate => CODE_LOCATE_RULES,
        McpTool::SymbolExplain => SYMBOL_EXPLAIN_RULES,
        McpTool::SymbolRelationships => SYMBOL_RELATIONSHIPS_RULES,
        McpTool::FlowTrace => FLOW_TRACE_RULES,
        McpTool::ChangeImpact => CHANGE_IMPACT_RULES,
        McpTool::TestsSelect => TESTS_SELECT_RULES,
        McpTool::ArchitectureOverview => ARCHITECTURE_OVERVIEW_RULES,
        McpTool::ArchitectureCycles => ARCHITECTURE_CYCLES_RULES,
        McpTool::CodeDead => CODE_DEAD_RULES,
        McpTool::HistoryCompare => HISTORY_COMPARE_RULES,
        McpTool::PlanChange => PLAN_CHANGE_RULES,
        McpTool::ContextPack => CONTEXT_PACK_RULES,
        McpTool::SourceRead => SOURCE_READ_RULES,
        McpTool::QueryAdvanced => QUERY_ADVANCED_RULES,
        McpTool::QueryBatch => QUERY_BATCH_RULES,
    }
}

const fn pagination_semantics(tool: McpTool) -> PaginationSemantics {
    match tool {
        McpTool::RepoIndex => PaginationSemantics::NotApplicable,
        McpTool::RepoList
        | McpTool::CodeLocate
        | McpTool::SymbolRelationships
        | McpTool::ContextPack
        | McpTool::QueryAdvanced => PaginationSemantics::AuthenticatedCursor,
        McpTool::RepoStatus | McpTool::OperationStatus => PaginationSemantics::BoundedComplete,
        McpTool::SymbolExplain => PaginationSemantics::ProgressiveHandle,
        McpTool::QueryBatch => PaginationSemantics::ChildContinuations,
        McpTool::FlowTrace
        | McpTool::ChangeImpact
        | McpTool::TestsSelect
        | McpTool::ArchitectureOverview
        | McpTool::ArchitectureCycles
        | McpTool::CodeDead
        | McpTool::HistoryCompare
        | McpTool::PlanChange
        | McpTool::SourceRead => PaginationSemantics::ExplicitTruncation,
    }
}

const fn generation_semantics(tool: McpTool) -> GenerationSemantics {
    match tool {
        McpTool::RepoIndex => GenerationSemantics::CreatesGeneration,
        McpTool::RepoStatus => GenerationSemantics::SelectsGeneration,
        McpTool::RepoList | McpTool::OperationStatus => GenerationSemantics::None,
        McpTool::HistoryCompare => GenerationSemantics::ComparesGenerations,
        McpTool::QueryBatch => GenerationSemantics::BatchInherited,
        _ => GenerationSemantics::SelectsGeneration,
    }
}

const fn budget_semantics(tool: McpTool) -> BudgetSemantics {
    match tool {
        McpTool::CodeLocate
        | McpTool::SymbolExplain
        | McpTool::SymbolRelationships
        | McpTool::FlowTrace
        | McpTool::ChangeImpact
        | McpTool::TestsSelect
        | McpTool::ArchitectureOverview
        | McpTool::ArchitectureCycles
        | McpTool::CodeDead
        | McpTool::HistoryCompare
        | McpTool::PlanChange
        | McpTool::SourceRead
        | McpTool::QueryAdvanced => BudgetSemantics::PerRequest,
        McpTool::ContextPack => BudgetSemantics::TokenBudget,
        McpTool::RepoStatus => BudgetSemantics::Unsupported,
        McpTool::QueryBatch => BudgetSemantics::PerRequest,
        McpTool::RepoIndex | McpTool::RepoList | McpTool::OperationStatus => BudgetSemantics::None,
    }
}

const fn input_shape_hash(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => "ca3b1fcc7237dea36cfb927003b8a97b39baeefe2ef08cd0c8f116d6381f160d",
        McpTool::RepoStatus => "4c74ff8e95f44eb590430a3a603fbb6248a18e590fc6026989d99ed8d696796e",
        McpTool::RepoList => "5f2a9e3fe96343fa1e75e8c4151d07cbc38ca6b1935ee7a8fadfd9defa9759b7",
        McpTool::OperationStatus => {
            "9703820cfd7dba86a224059c47287e73e06876df4d3245375fef88489f2554f3"
        }
        McpTool::CodeLocate => "bdd2589fdad0697156bc225ccf4ebdb9257ccc8f95234c35dd9ae3c52d06fdad",
        McpTool::SymbolExplain => {
            "698666081b80d3b8a031dcc103b74f5281dd0caa073d42ff3ee1679c21cf41a7"
        }
        McpTool::SymbolRelationships => {
            "8023c0c362d18ed970b5a0d5ff8af700c986fd9ab186663fbd1e29ba07cd201a"
        }
        McpTool::FlowTrace => "b228a48614c0069f4347b6c9ce4040a8532db4c7a17a83b6d52108d50fcb1360",
        McpTool::ChangeImpact => "93aded61e1aa507496fd7179a636e8c0f1f8384a9304750d20f98d41ab03f717",
        McpTool::TestsSelect => "6d468db8eb4ce2585faf489f3de96fa2a326af799024e8a2bd741d417ee9c846",
        McpTool::ArchitectureOverview => {
            "86c25ac4db8505c1754dba09d9c73d122541095f69edc75b7f6851fdc9fa253a"
        }
        McpTool::ArchitectureCycles => {
            "081ec741b4821a67453a0161a900577670faf06eb2ee7c1ff8d681c743875879"
        }
        McpTool::CodeDead => "5a0fb3d5c1812d6ab023e4764e786dac63ec294e01d06466ae5f7e4e992c9f0b",
        McpTool::HistoryCompare => {
            "ca8734f87fb7a3c7e8215c19ff295d9ba37092ab4e9288f6f5f7e993fe5c777c"
        }
        McpTool::PlanChange => "6f2e2f974582a6025e15233d36ce798742089e8bb055c726b63c3258a27ef411",
        McpTool::ContextPack => "bcefc3fce03c389f26078725694aabdd2878e315e31e213c0ef8fb961b197142",
        McpTool::SourceRead => "df1472b995ed8d489d9abafb8adc95ab0b5da699beb3ed29dcc16b7293de32f8",
        McpTool::QueryAdvanced => {
            "2e8e0b28deda82821ecc8cacd45b424929303bb5ccca057e89cabfc1552aaa3b"
        }
        McpTool::QueryBatch => "7a9e82cf00569c6df57b9cf733703517e918ce5fbc0143c88c61514ef82fb927",
    }
}

const fn tool_fallback_summary(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => "bounded attached process-local structural generation creation",
        McpTool::RepoStatus => {
            "bounded process-local active or exact-generation status with coverage, operations, and freshness gates"
        }
        McpTool::RepoList => {
            "immutable catalog snapshot with bounded display-name or alias and lifecycle-state filters"
        }
        McpTool::OperationStatus => "bounded operation read and cancel",
        McpTool::CodeLocate => "bounded exact-identifier and lexical matching",
        McpTool::SymbolExplain => {
            "bounded profiled semantic evidence for explicit stable symbol identifiers"
        }
        McpTool::SymbolRelationships => {
            "bounded typed relationships around explicit stable symbol identifiers"
        }
        McpTool::FlowTrace => "bounded symbol relation path tracing",
        McpTool::ChangeImpact => "bounded explicit symbol-or-path change mapping",
        McpTool::TestsSelect => "bounded unit-test ranking from explicit symbol seeds",
        McpTool::ArchitectureOverview => {
            "bounded file-granularity architecture map with optional hotspots"
        }
        McpTool::ArchitectureCycles => "bounded cycle detection in a selected relation projection",
        McpTool::CodeDead => "bounded dead-code candidates with entry-point and blind-spot caveats",
        McpTool::HistoryCompare => {
            "bounded entity and signature comparison of two explicit retained generation identifiers"
        }
        McpTool::PlanChange => {
            "bounded change planning from a caller-authored objective and explicit targets"
        }
        McpTool::ContextPack => {
            "bounded profiled evidence assembly with authenticated continuation, generation-pinned references signatures and source snippets under a token budget"
        }
        McpTool::SourceRead => {
            "bounded source ranges from pinned source references as untrusted data"
        }
        McpTool::QueryAdvanced => {
            "bounded safe-ast query with typed value parameters, authenticated continuation, and enforced cost, row, and depth limits"
        }
        McpTool::QueryBatch => {
            "bounded active-generation batch dispatch for up to sixteen eligible reads with shared child accounting"
        }
    }
}

/// Const-compatible discriminant comparison for field-less tools.
const fn tool_as_u8(tool: McpTool) -> u8 {
    tool as u8
}

#[cfg(test)]
mod tests {
    use super::{
        ANALYTICAL_RESPONSE_PROFILES, BATCH_ELIGIBLE, CAPABILITIES, COMPACT_RESPONSE_PROFILES,
        CapabilityStatus, GenerationSemantics, McpTool, PaginationSemantics,
        ResponseProfileSupport, is_batch_eligible,
    };
    use crate::{ErrorCode, vertical::ResponseProfile};

    #[test]
    fn registry_covers_exactly_the_catalog_in_order() {
        assert_eq!(CAPABILITIES.len(), McpTool::ALL.len());
        for (entry, tool) in CAPABILITIES.iter().zip(McpTool::ALL) {
            assert_eq!(entry.tool, tool, "registry order must match the catalog");
            assert_eq!(entry.contract_version, tool.contract_version());
            assert_ne!(entry.input_shape_hash, "");
        }
        assert_eq!(
            super::capability_for(McpTool::RepoList).contract_version,
            crate::REPO_LIST_SCHEMA_VERSION
        );
        assert!(
            CAPABILITIES
                .iter()
                .filter(|entry| entry.tool != McpTool::RepoList)
                .all(|entry| entry.contract_version == crate::MCP_SCHEMA_VERSION)
        );
    }

    #[test]
    fn response_profile_registry_covers_the_exact_public_matrix() {
        use super::ResponseProfileField::{
            Profile, ResponseProfile as CanonicalResponseProfileField,
        };
        use ResponseProfile::{Compact, Evidence, Standard};
        use ResponseProfileSupport::{Fixed, Selectable};

        let expected = [
            (
                McpTool::RepoIndex,
                Fixed {
                    representation: Compact,
                },
            ),
            (
                McpTool::RepoStatus,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: COMPACT_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::RepoList,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: COMPACT_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::OperationStatus,
                Fixed {
                    representation: Compact,
                },
            ),
            (
                McpTool::CodeLocate,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::SymbolExplain,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::SymbolRelationships,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::FlowTrace,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::ChangeImpact,
                Selectable {
                    wire_field: Profile,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::TestsSelect,
                Selectable {
                    wire_field: Profile,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::ArchitectureOverview,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::ArchitectureCycles,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::CodeDead,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::HistoryCompare,
                Selectable {
                    wire_field: Profile,
                    supported: COMPACT_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::PlanChange,
                Selectable {
                    wire_field: Profile,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::ContextPack,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::SourceRead,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: COMPACT_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
            (
                McpTool::QueryAdvanced,
                Fixed {
                    representation: Compact,
                },
            ),
            (
                McpTool::QueryBatch,
                Selectable {
                    wire_field: CanonicalResponseProfileField,
                    supported: ANALYTICAL_RESPONSE_PROFILES,
                    default: Compact,
                },
            ),
        ];

        assert_eq!(expected.len(), McpTool::ALL.len());
        for ((tool, expected_support), entry) in expected.into_iter().zip(&CAPABILITIES) {
            assert_eq!(entry.tool, tool);
            assert_eq!(
                entry.response_profiles,
                expected_support,
                "{} response-profile descriptor drifted",
                tool.name()
            );

            match entry.response_profiles {
                Fixed { .. } => {
                    assert!(
                        entry.rules.iter().all(|rule| {
                            rule.path != CanonicalResponseProfileField.name()
                                && rule.path != Profile.name()
                        }),
                        "{} is fixed but declares a profile selector",
                        tool.name()
                    );
                }
                Selectable {
                    wire_field,
                    supported,
                    default,
                } => {
                    assert!(!supported.is_empty());
                    assert!(supported.contains(&default));
                    for profile in [Compact, Standard, Evidence] {
                        let value = match profile {
                            Compact => "compact",
                            Standard => "standard",
                            Evidence => "evidence",
                        };
                        let disposition = entry.disposition(wire_field.name(), Some(value));
                        if supported.contains(&profile) {
                            assert!(
                                matches!(
                                    disposition.status,
                                    CapabilityStatus::Implemented
                                        | CapabilityStatus::FallbackLimited
                                ),
                                "{} advertises unsupported profile {value}",
                                tool.name()
                            );
                        } else {
                            assert_eq!(
                                disposition.status,
                                CapabilityStatus::UnsupportedStableError,
                                "{} must reject unadvertised profile {value}",
                                tool.name()
                            );
                            assert_eq!(
                                disposition.error_code,
                                Some(ErrorCode::UnsupportedCapability)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn discovery_distinguishes_exposure_and_response_profiles() {
        let fixed = serde_json::to_value(super::discovery_metadata(McpTool::RepoIndex))
            .expect("fixed capability metadata serializes");
        assert_eq!(fixed["profiles"], serde_json::json!(["developer"]));
        assert_eq!(
            fixed["responseProfiles"],
            serde_json::json!({
                "mode": "fixed",
                "representation": "compact"
            })
        );

        let selectable = serde_json::to_value(super::discovery_metadata(McpTool::ChangeImpact))
            .expect("selectable capability metadata serializes");
        assert_eq!(
            selectable["responseProfiles"],
            serde_json::json!({
                "mode": "selectable",
                "wireField": "profile",
                "supported": ["compact", "standard", "evidence"],
                "default": "compact"
            })
        );

        let batch = serde_json::to_value(super::discovery_metadata(McpTool::QueryBatch))
            .expect("batch capability metadata serializes");
        assert_eq!(
            batch["responseProfiles"],
            serde_json::json!({
                "mode": "selectable",
                "wireField": "response_profile",
                "supported": ["compact", "standard", "evidence"],
                "default": "compact"
            })
        );
    }

    #[test]
    fn unreviewed_fields_fail_closed_for_every_tool() {
        for entry in &CAPABILITIES {
            assert_eq!(
                entry.default_field_status,
                CapabilityStatus::Blocked,
                "{} must reject fields without an explicit rule",
                entry.tool.name()
            );
        }
    }

    #[test]
    fn batch_metadata_matches_the_canonical_allowlist_and_shared_budget() {
        for entry in &CAPABILITIES {
            assert_eq!(
                entry.batch_eligible,
                is_batch_eligible(entry.tool),
                "{} batch flag drifted from the allowlist",
                entry.tool.name()
            );
            assert_eq!(
                entry.batch_shared_budget,
                entry.tool == McpTool::QueryBatch,
                "{} shared child-budget flag drifted",
                entry.tool.name()
            );
        }
        assert_eq!(BATCH_ELIGIBLE.len(), 12);
        assert!(is_batch_eligible(McpTool::PlanChange));
        assert!(!is_batch_eligible(McpTool::QueryBatch));
    }

    #[test]
    fn explain_and_generation_metadata_match_the_public_surface() {
        for entry in &CAPABILITIES {
            assert_eq!(
                entry.explain_supported,
                !matches!(entry.tool, McpTool::RepoIndex | McpTool::OperationStatus)
            );
        }
        let status = CAPABILITIES[McpTool::RepoStatus as usize];
        assert_eq!(status.generation, GenerationSemantics::SelectsGeneration);
    }

    #[test]
    fn known_silent_fields_have_explicit_dispositions() {
        let repo_index = CAPABILITIES[McpTool::RepoIndex as usize];
        let scoped_index = repo_index.disposition("scope.repository", None);
        assert_eq!(
            scoped_index.status,
            CapabilityStatus::UnsupportedStableError
        );
        assert_eq!(
            scoped_index.error_code,
            Some(ErrorCode::UnsupportedCapability)
        );
        let detached_index = repo_index.disposition("detached", Some("true"));
        assert_eq!(
            detached_index.status,
            CapabilityStatus::UnsupportedStableError
        );
        assert_eq!(
            detached_index.error_code,
            Some(ErrorCode::UnsupportedCapability)
        );
        assert_eq!(
            repo_index.disposition("detached", Some("false")).status,
            CapabilityStatus::Implemented
        );

        let repo_status = CAPABILITIES[McpTool::RepoStatus as usize];
        let explicit_generation =
            repo_status.disposition("generation", Some("gen1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert_eq!(explicit_generation.status, CapabilityStatus::Implemented);
        assert_eq!(explicit_generation.error_code, None);
        assert_eq!(
            repo_status.disposition("generation", Some("active")).status,
            CapabilityStatus::Implemented
        );

        let repo_list = CAPABILITIES[McpTool::RepoList as usize];
        assert_eq!(
            repo_list.disposition("query", None).status,
            CapabilityStatus::Implemented
        );
        assert_eq!(
            repo_list.disposition("states", None).status,
            CapabilityStatus::Implemented
        );
        assert_eq!(
            repo_list.pagination,
            PaginationSemantics::AuthenticatedCursor
        );
        let batch = CAPABILITIES[McpTool::QueryBatch as usize];
        assert_eq!(
            batch
                .disposition("operations[].local_budget.max_tokens", None)
                .status,
            CapabilityStatus::Blocked
        );
        assert_eq!(
            batch
                .disposition("operations[].local_budget.timeout_ms", None)
                .status,
            CapabilityStatus::Implemented
        );
        assert_eq!(
            batch.disposition("generation", None).status,
            CapabilityStatus::FallbackLimited
        );
        assert_eq!(
            batch.disposition("generation", Some("active")).status,
            CapabilityStatus::Implemented
        );
        assert_eq!(
            batch
                .disposition("operations[].tool", Some("plan.change"))
                .status,
            CapabilityStatus::FallbackLimited
        );

        let cycles = CAPABILITIES[McpTool::ArchitectureCycles as usize];
        assert_eq!(
            cycles
                .disposition("projection.level", Some("symbol"))
                .status,
            CapabilityStatus::Implemented
        );
        let unsupported_level = cycles.disposition("projection.level", Some("module"));
        assert_eq!(
            unsupported_level.status,
            CapabilityStatus::UnsupportedStableError
        );
        assert_eq!(
            unsupported_level.error_code,
            Some(ErrorCode::UnsupportedCapability)
        );

        let source_read = CAPABILITIES[McpTool::SourceRead as usize];
        for path in [
            "references[].symbol_id",
            "references[].file_id",
            "references[].start_byte",
            "references[].end_byte",
        ] {
            let disposition = source_read.disposition(path, None);
            assert_eq!(
                disposition.status,
                CapabilityStatus::UnsupportedStableError,
                "{path} must fail closed"
            );
            assert_eq!(
                disposition.error_code,
                Some(ErrorCode::UnsupportedCapability),
                "{path} must preserve the stable public code"
            );
        }
        for path in ["context_lines_before", "context_lines_after"] {
            assert_eq!(
                source_read.disposition(path, None).status,
                CapabilityStatus::Implemented
            );
        }
    }

    #[test]
    fn discovery_metadata_is_source_free_and_matches_the_registry() {
        use super::discovery_metadata;

        for entry in &CAPABILITIES {
            let metadata = discovery_metadata(entry.tool);
            assert_eq!(metadata.contract_version, entry.contract_version);
            assert_eq!(metadata.input_shape_hash, entry.input_shape_hash);
            assert_eq!(
                metadata.profiles,
                entry
                    .profiles
                    .iter()
                    .map(|profile| profile.name())
                    .collect::<Vec<_>>()
            );
            assert_eq!(metadata.response_profiles, entry.response_profiles);
            assert_eq!(metadata.batch_eligible, entry.batch_eligible);
            assert_eq!(metadata.explain_supported, entry.explain_supported);
            assert_eq!(metadata.batch_shared_budget, entry.batch_shared_budget);
            assert_eq!(
                metadata.limitations.len(),
                entry
                    .rules
                    .iter()
                    .filter(|rule| rule.is_public_limitation())
                    .count()
            );
            let encoded = serde_json::to_string(&metadata).expect("capability metadata serializes");
            assert!(!encoded.contains('\\'));
            assert!(!encoded.contains("rootlight-mcp::"));
            for private_label in [["TASK", "-"].concat(), ["GATE", "-"].concat()] {
                assert!(!encoded.contains(&private_label));
            }
        }

        let operation_status = discovery_metadata(McpTool::OperationStatus);
        assert_eq!(operation_status.status, "implemented");
        assert!(operation_status.limitations.is_empty());

        let repo_index = discovery_metadata(McpTool::RepoIndex);
        let lifecycle = repo_index
            .lifecycle
            .as_ref()
            .expect("repo.index exposes its versioned lifecycle profile");
        assert_eq!(lifecycle.version, "1.0");
        assert!(!lifecycle.update_by_repository_id);
        assert_eq!(lifecycle.accepted_modes, ["auto", "structural"]);
        assert_eq!(lifecycle.scope, "whole_repository");
        assert!(lifecycle.synchronous_terminal);
        assert_eq!(lifecycle.max_wait_ms, 30_000);
        assert!(!lifecycle.detached);
        assert_eq!(lifecycle.public_idempotency, "none");
        assert!(lifecycle.internal_operation_retry);
        assert_eq!(lifecycle.state_persistence, "process_local");
        assert_eq!(lifecycle.restart_behavior, "reindex_required");
        assert_eq!(lifecycle.publication, "atomic_on_terminal_success");
        assert!(
            repo_index
                .limitations
                .iter()
                .all(|limitation| limitation.field != "root"),
            "accepted allowlist ancestors are not public limitations"
        );
        assert!(repo_index.limitations.iter().any(|limitation| {
            limitation.field == "detached"
                && limitation.value == Some("true")
                && limitation.status == "unsupported_stable_error"
                && limitation.error_code == Some(ErrorCode::UnsupportedCapability)
        }));
        for tool in McpTool::ALL {
            if tool != McpTool::RepoIndex {
                assert!(discovery_metadata(tool).lifecycle.is_none());
            }
        }

        let batch = discovery_metadata(McpTool::QueryBatch);
        assert_eq!(batch.generation, "batch_inherited");
        let generation = batch
            .limitations
            .iter()
            .find(|limitation| limitation.field == "generation")
            .expect("query.batch discovery exposes its generation limitation");
        assert_eq!(generation.status, "fallback_limited");
        assert!(
            generation
                .summary
                .contains("non-active explicit generations")
        );
        assert_eq!(generation.error_code, None);
    }

    #[test]
    fn every_tool_has_an_explicit_pagination_classification() {
        let expected = [
            (McpTool::RepoIndex, PaginationSemantics::NotApplicable),
            (
                McpTool::OperationStatus,
                PaginationSemantics::BoundedComplete,
            ),
            (McpTool::RepoList, PaginationSemantics::AuthenticatedCursor),
            (McpTool::RepoStatus, PaginationSemantics::BoundedComplete),
            (
                McpTool::CodeLocate,
                PaginationSemantics::AuthenticatedCursor,
            ),
            (
                McpTool::SymbolExplain,
                PaginationSemantics::ProgressiveHandle,
            ),
            (
                McpTool::SymbolRelationships,
                PaginationSemantics::AuthenticatedCursor,
            ),
            (McpTool::FlowTrace, PaginationSemantics::ExplicitTruncation),
            (
                McpTool::ChangeImpact,
                PaginationSemantics::ExplicitTruncation,
            ),
            (
                McpTool::TestsSelect,
                PaginationSemantics::ExplicitTruncation,
            ),
            (
                McpTool::ArchitectureOverview,
                PaginationSemantics::ExplicitTruncation,
            ),
            (
                McpTool::ArchitectureCycles,
                PaginationSemantics::ExplicitTruncation,
            ),
            (McpTool::CodeDead, PaginationSemantics::ExplicitTruncation),
            (
                McpTool::ContextPack,
                PaginationSemantics::AuthenticatedCursor,
            ),
            (
                McpTool::QueryAdvanced,
                PaginationSemantics::AuthenticatedCursor,
            ),
            (McpTool::QueryBatch, PaginationSemantics::ChildContinuations),
            (
                McpTool::HistoryCompare,
                PaginationSemantics::ExplicitTruncation,
            ),
            (McpTool::PlanChange, PaginationSemantics::ExplicitTruncation),
            (McpTool::SourceRead, PaginationSemantics::ExplicitTruncation),
        ];

        assert_eq!(expected.len(), McpTool::ALL.len());
        for (tool, semantics) in expected {
            assert_eq!(
                CAPABILITIES[tool as usize].pagination,
                semantics,
                "{} has the wrong public pagination classification",
                tool.name()
            );
        }
    }

    #[test]
    fn fallback_descriptions_are_bounded_by_the_registry_summary() {
        for entry in &CAPABILITIES {
            if entry.status == CapabilityStatus::Implemented {
                continue;
            }
            let description = entry.tool.description().to_ascii_lowercase();
            assert!(
                description.contains(entry.fallback_summary),
                "{} description is broader than its capability summary: {}",
                entry.tool.name(),
                entry.tool.description()
            );
        }
    }

    #[test]
    fn unsupported_values_carry_stable_error_metadata() {
        for entry in &CAPABILITIES {
            for rule in entry.rules {
                assert!(!rule.path.is_empty());
                assert!(!rule.summary.is_empty());
                if rule.status == CapabilityStatus::UnsupportedStableError {
                    assert!(
                        rule.error_code.is_some(),
                        "{} {} must declare its stable pre-execution error",
                        entry.tool.name(),
                        rule.path
                    );
                } else {
                    assert_eq!(rule.error_code, None);
                }
            }
        }
    }
}
