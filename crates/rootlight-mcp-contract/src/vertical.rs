//! Source definitions for the first MCP vertical-slice tool schemas.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_error::PublicError;
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{CoverageStatus, SourceRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TrustClassification;

/// One tool exposed by the first secure MCP vertical slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerticalTool {
    /// Registers or rebuilds one repository.
    RepoIndex,
    /// Reads or cancels one operation.
    OperationStatus,
    /// Locates bounded structural or lexical matches.
    CodeLocate,
    /// Explains one or more stable symbols.
    SymbolExplain,
    /// Reads generation-pinned source ranges.
    SourceRead,
}

impl VerticalTool {
    /// Complete deterministic first-slice tool catalog.
    pub const ALL: [Self; 5] = [
        Self::RepoIndex,
        Self::OperationStatus,
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SourceRead,
    ];

    /// Stable tool name advertised through MCP.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RepoIndex => "repo.index",
            Self::OperationStatus => "operation.status",
            Self::CodeLocate => "code.locate",
            Self::SymbolExplain => "symbol.explain",
            Self::SourceRead => "source.read",
        }
    }

    /// Static source-free title intended for clients.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::RepoIndex => "Index repository",
            Self::OperationStatus => "Inspect operation",
            Self::CodeLocate => "Locate code",
            Self::SymbolExplain => "Explain symbol",
            Self::SourceRead => "Read source",
        }
    }

    /// Static source-free description intended for models and clients.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::RepoIndex => {
                "Create or update one local repository generation and return its operation handle."
            }
            Self::OperationStatus => "Read or cancel one known long-running Rootlight operation.",
            Self::CodeLocate => {
                "Find bounded, generation-pinned code and file matches by identifier, text, path, or structure."
            }
            Self::SymbolExplain => {
                "Return bounded semantic evidence for stable symbol identifiers."
            }
            Self::SourceRead => {
                "Read exact bounded ranges from a pinned source snapshot as untrusted repository data."
            }
        }
    }

    /// Checked JSON Schema 2020-12 input artifact for this tool.
    #[must_use]
    pub const fn input_schema_json(self) -> &'static str {
        match self {
            Self::RepoIndex => {
                include_str!("../../../schemas/generated/json/mcp-repo-index-input-1.0.schema.json")
            }
            Self::OperationStatus => include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.0.schema.json"
            ),
            Self::CodeLocate => include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.0.schema.json"
            ),
            Self::SymbolExplain => include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.0.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-input-1.0.schema.json"
            ),
        }
    }

    /// Checked JSON Schema 2020-12 output artifact for this tool.
    #[must_use]
    pub const fn output_schema_json(self) -> &'static str {
        match self {
            Self::RepoIndex => include_str!(
                "../../../schemas/generated/json/mcp-repo-index-output-1.0.schema.json"
            ),
            Self::OperationStatus => include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.0.schema.json"
            ),
            Self::CodeLocate => include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.0.schema.json"
            ),
            Self::SymbolExplain => include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.0.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-output-1.0.schema.json"
            ),
        }
    }

    /// Whether the tool only reads already published state.
    #[must_use]
    pub const fn read_only(self) -> bool {
        matches!(
            self,
            Self::CodeLocate | Self::SymbolExplain | Self::SourceRead
        )
    }

    /// Whether repeating the same admitted request has the same intended effect.
    #[must_use]
    pub const fn idempotent(self) -> bool {
        true
    }

    /// Whether the tool performs a destructive update.
    #[must_use]
    pub const fn destructive(self) -> bool {
        false
    }
}

/// Version marker carried by every first-slice response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SchemaVersion {
    /// Tool contract version 1.0.
    #[serde(rename = "1.0")]
    V1_0,
}

/// A property that must be present and may contain JSON `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequiredNullable<T>(pub Option<T>);

/// Repository selector accepted by first-slice read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RepositorySelector {
    /// Select by stable repository identifier.
    ById(RepositoryIdSelector),
    /// Select by a configured local alias.
    ByAlias(RepositoryAliasSelector),
}

/// Stable repository-ID selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIdSelector {
    /// Repository identity.
    pub repository_id: RepositoryId,
}

/// Registered repository-alias selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAliasSelector {
    /// Registered alias, resolved to exactly one repository.
    #[schemars(length(min = 1, max = 256))]
    pub alias: String,
}

/// Generation selector shared by first-slice read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum GenerationSelector {
    /// Resolve the currently active immutable generation.
    Active(ActiveGeneration),
    /// Pin an explicit immutable generation.
    Explicit(GenerationId),
}

/// Active-generation keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActiveGeneration {
    /// Select the active generation.
    #[serde(rename = "active")]
    Active,
}

/// Requested response representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProfile {
    /// Smallest complete correctness-bearing response.
    Compact,
    /// More explanatory response within the same hard budgets.
    Standard,
    /// Maximum bounded provenance and evidence.
    Evidence,
}

/// Optional response limits that can only reduce server hard limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseBudget {
    /// Maximum returned result objects.
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: Option<u16>,
    /// Maximum estimated output tokens.
    #[schemars(range(min = 100, max = 3200))]
    pub max_tokens: Option<u16>,
    /// Maximum source bytes before JSON escaping.
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Maximum relationship or traversal facts examined.
    #[schemars(range(min = 1, max = 100_000))]
    pub max_traversal_facts: Option<u32>,
    /// Maximum plan depth.
    #[schemars(range(min = 1, max = 16))]
    pub max_depth: Option<u8>,
    /// Maximum independently returned paths.
    #[schemars(range(min = 1, max = 1000))]
    pub max_paths: Option<u16>,
    /// Cooperative request deadline in milliseconds.
    #[schemars(range(min = 10, max = 30_000))]
    pub timeout_ms: Option<u32>,
    /// Requested evidence detail.
    pub evidence_level: Option<ProvenanceLevel>,
}

/// Scope accepted by repository indexing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum IndexScope {
    /// Index the complete repository policy scope.
    Repository(RepositoryScope),
    /// Index selected repository-relative paths.
    Paths(PathScope),
    /// Index selected package identities.
    Packages(PackageScope),
    /// Index selected build-target identities without executing builds.
    BuildTargets(BuildTargetScope),
}

/// Whole-repository scope marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryScope {
    /// Must be the repository scope keyword.
    pub repository: RepositoryScopeValue,
}

/// Whole-repository scope keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryScopeValue {
    /// Complete repository policy scope.
    Whole,
}

/// Repository-relative path scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathScope {
    /// Distinct repository-relative paths.
    #[schemars(length(min = 1, max = 256))]
    pub paths: BTreeSet<String>,
}

/// Package scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageScope {
    /// Distinct package identities.
    #[schemars(length(min = 1, max = 256))]
    pub packages: BTreeSet<String>,
}

/// Build-target scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildTargetScope {
    /// Distinct build-target identities.
    #[schemars(length(min = 1, max = 256))]
    pub build_targets: BTreeSet<String>,
}

/// Indexing mode requested by `repo.index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexMode {
    /// Select the strongest available safe mode.
    Auto,
    /// Build the structural tier only.
    Structural,
    /// Request available deep tiers.
    Deep,
    /// Rebuild from a clean generation.
    Rebuild,
}

/// Requested or observed language support tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AnalysisTier {
    /// Compiler-quality or equivalent semantic evidence.
    A,
    /// High-confidence partial semantic evidence.
    B,
    /// Generic structural extraction.
    C,
    /// Lexical-only support.
    D,
}

/// Strict input for `repo.index`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexInput {
    /// Canonicalizable local root for first registration.
    #[schemars(length(min = 1, max = 4096))]
    pub root: Option<String>,
    /// Existing repository identity to update.
    pub repository_id: Option<RepositoryId>,
    /// Optional indexing scope.
    pub scope: Option<IndexScope>,
    /// Requested indexing mode.
    pub mode: Option<IndexMode>,
    /// Per-language maximum requested tier.
    pub requested_tiers: Option<BTreeMap<String, AnalysisTier>>,
    /// Validated operation-scoped configuration override.
    pub configuration_patch: Option<BTreeMap<String, Value>>,
    /// Maximum time to wait for publication or a terminal state.
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Whether the operation may continue after client disconnect.
    pub detached: Option<bool>,
}

/// Summary of one admitted indexing plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexPlanSummary {
    /// Normalized scope class.
    pub scope: IndexPlanScope,
    /// Selected analysis mode.
    pub mode: IndexMode,
    /// Admitted providers in deterministic order.
    #[schemars(length(max = 64))]
    pub providers: Vec<String>,
    /// Parent generation, when an active generation existed.
    pub parent_generation: RequiredNullable<GenerationId>,
    /// Estimated staging and publication bytes.
    pub estimated_disk_bytes: u64,
}

/// Compact normalized scope class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexPlanScope {
    /// Whole repository.
    Repository,
    /// Selected paths.
    Paths,
    /// Selected packages.
    Packages,
    /// Selected build targets.
    BuildTargets,
}

/// Durable operation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Accepted but not started.
    Queued,
    /// Actively running.
    Running,
    /// Published an immutable generation.
    Published,
    /// Failed without publishing partial state.
    Failed,
    /// Cancelled without publishing partial state.
    Cancelled,
    /// Waiting for explicit build or environment context.
    WaitingForContext,
}

/// Source-free diagnostic attached to an operational response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable diagnostic code.
    #[schemars(length(min = 1, max = 128))]
    pub code: String,
    /// Static or Rootlight-generated source-free message.
    #[schemars(length(min = 1, max = 1024))]
    pub message: String,
}

/// `repo.index` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexData {
    /// Registered repository identity.
    pub repository_id: RepositoryId,
    /// Durable operation identity.
    pub operation_id: OperationId,
    /// Plan admitted by policy and resource checks.
    pub accepted_plan: IndexPlanSummary,
    /// Current operation state.
    pub state: OperationState,
    /// Generation published within `wait_ms`, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Source-free validation and capability notes.
    #[schemars(length(max = 100))]
    pub diagnostics: Vec<Diagnostic>,
}

/// Strict output for `repo.index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexOutput {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operational result.
    pub data: RepoIndexData,
}

/// Action accepted by `operation.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationAction {
    /// Read current state.
    Get,
    /// Request cooperative cancellation.
    Cancel,
}

/// Strict input for `operation.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusInput {
    /// Operation handle returned by the creating tool.
    pub operation_id: OperationId,
    /// Read or request cancellation.
    pub action: Option<OperationAction>,
    /// Maximum long-poll duration.
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Return immediately only after this journal revision.
    pub after_revision: Option<u64>,
}

/// Progress units reported by an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationProgress {
    /// Completed units.
    pub completed_units: u64,
    /// Known total units, when measurable.
    pub total_units: RequiredNullable<u64>,
}

/// Bounded resource counters for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationResources {
    /// Peak resident bytes observed so far.
    pub peak_rss_bytes: u64,
    /// Durable bytes written so far.
    pub written_bytes: u64,
    /// Files examined so far.
    pub files_examined: u64,
}

/// One operation journal view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDetail {
    /// Operation kind.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Durable state.
    pub state: OperationState,
    /// Current source-free stage name.
    #[schemars(length(min = 1, max = 128))]
    pub stage: String,
    /// Bounded progress counters.
    pub progress: OperationProgress,
    /// Monotonic journal revision.
    pub revision: u64,
    /// RFC 3339 UTC creation time.
    #[schemars(length(min = 20, max = 35))]
    pub started_at: String,
    /// Bounded resource summary.
    pub resources: OperationResources,
}

/// `operation.status` result data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusData {
    /// Current operation view.
    pub operation: OperationDetail,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<PublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for `operation.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusOutput {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operation result.
    pub data: OperationStatusData,
}

/// Common scope selector for read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ScopeSelector {
    /// Limit to repository-relative paths.
    Paths(PathScope),
    /// Limit to packages.
    Packages(PackageScope),
    /// Limit to build targets.
    BuildTargets(BuildTargetScope),
    /// Limit to stable symbols.
    Symbols(SymbolScope),
}

/// Stable-symbol scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolScope {
    /// Distinct symbol identities.
    #[schemars(length(min = 1, max = 64))]
    pub symbols: BTreeSet<SymbolId>,
}

/// Entity classes supported by the first locate slice.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
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

/// Locate retrieval mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Exact identifier match.
    Exact,
    /// Indexed lexical retrieval.
    Lexical,
    /// Structural filtering.
    Structural,
    /// Documentation retrieval.
    Docs,
    /// Repository-relative path retrieval.
    Path,
    /// Optional local semantic extension.
    Semantic,
}

/// Strict input for `code.locate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLocateInput {
    /// Repository to query.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    pub generation: Option<GenerationSelector>,
    /// Identifier, path, text, or concept query.
    #[schemars(length(min = 1, max = 2048))]
    pub query: String,
    /// Entity kinds to retain.
    #[schemars(length(max = 32))]
    pub kinds: Option<BTreeSet<EntityKind>>,
    /// Optional structural scope.
    pub scope: Option<ScopeSelector>,
    /// Language identity filters.
    #[schemars(length(max = 32))]
    pub languages: Option<BTreeSet<String>>,
    /// Retrieval modes.
    #[schemars(length(max = 6))]
    pub search_modes: Option<BTreeSet<SearchMode>>,
    /// Structural seed symbols.
    #[schemars(length(max = 16))]
    pub related_to: Option<BTreeSet<SymbolId>>,
    /// Minimum relation confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Maximum returned matches.
    #[schemars(range(min = 1, max = 200))]
    pub max_results: Option<u16>,
    /// Optional lower response limits.
    pub budget: Option<ResponseBudget>,
    /// Opaque generation-bound continuation cursor.
    #[schemars(length(min = 1, max = 4096))]
    pub cursor: Option<String>,
    /// Requested representation.
    pub response_profile: Option<ResponseProfile>,
}

/// Resolved repository metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRepository {
    /// Stable repository identity.
    pub repository_id: RepositoryId,
    /// Rootlight-owned display label.
    #[schemars(length(min = 1, max = 256))]
    pub display_name: String,
}

/// Freshness of one immutable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Matches the currently observed repository snapshot.
    Current,
    /// Queryable but a newer generation exists.
    Superseded,
    /// Queryable with a known stale source snapshot.
    Stale,
}

/// Generation metadata carried by each read response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationSummary {
    /// Pinned generation.
    pub generation_id: GenerationId,
    /// Parent generation, if any.
    pub parent_generation: RequiredNullable<GenerationId>,
    /// Structural freshness.
    pub structural_freshness: Freshness,
    /// Semantic freshness.
    pub semantic_freshness: Freshness,
}

/// Per-language coverage summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LanguageCoverage {
    /// Language identity.
    #[schemars(length(min = 1, max = 64))]
    pub language: String,
    /// Observed support tier.
    pub tier: AnalysisTier,
    /// Coverage state for this language.
    pub status: CoverageStatus,
}

/// Coverage metadata relevant to one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageSummary {
    /// Aggregate query-domain coverage.
    pub status: CoverageStatus,
    /// Deterministically ordered language coverage.
    #[schemars(length(max = 64))]
    pub languages: Vec<LanguageCoverage>,
    /// Inputs skipped by policy, limits, or capability.
    pub skipped_inputs: u64,
}

/// Cache classification for usage reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    /// No cache entry was used.
    Miss,
    /// A verified generation-bound cache entry was used.
    Hit,
    /// The operator does not use a cache.
    NotApplicable,
}

/// Bounded counters returned by each read tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageSummary {
    /// Storage rows examined.
    pub rows: u64,
    /// Relationship edges examined.
    pub edges: u64,
    /// Raw source bytes returned.
    pub source_bytes: u64,
    /// Encoded structured result bytes.
    pub json_bytes: u64,
    /// Deterministic token estimate.
    pub estimated_tokens: u64,
    /// Cooperative wall time in milliseconds.
    pub wall_time_ms: u64,
    /// Cache outcome.
    pub cache_status: CacheStatus,
    /// Source-free trace identity.
    #[schemars(length(min = 1, max = 128))]
    pub trace_id: String,
}

/// Source-free response warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseWarning {
    /// Stable warning code.
    #[schemars(length(min = 1, max = 128))]
    pub code: String,
    /// Rootlight-generated source-free explanation.
    #[schemars(length(min = 1, max = 1024))]
    pub message: String,
}

/// Common strict response envelope for generation-pinned reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadEnvelope<T> {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Resolved repository.
    pub repository: ResolvedRepository,
    /// Pinned generation and freshness.
    pub generation: GenerationSummary,
    /// Relevant coverage.
    pub coverage: CoverageSummary,
    /// Tool-specific result.
    pub data: T,
    /// Whether any hard or requested limit stopped completion.
    pub truncated: bool,
    /// Safe continuation cursor, when the result is pageable.
    #[schemars(required)]
    pub next_cursor: Option<String>,
    /// Runtime resource accounting.
    pub usage: UsageSummary,
    /// Source-free warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
}

/// Why a locate item ranked in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocateReason {
    /// Exact identifier match.
    #[serde(rename = "identifier_match")]
    Identifier,
    /// Indexed lexical match.
    #[serde(rename = "lexical_match")]
    Lexical,
    /// Documentation match.
    #[serde(rename = "docs_match")]
    Docs,
    /// Path match.
    #[serde(rename = "path_match")]
    Path,
    /// Structural relation match.
    #[serde(rename = "structural_match")]
    Structural,
}

/// One bounded locate result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocatedItem {
    /// Stable symbol identity when the item is a symbol.
    pub symbol_id: Option<SymbolId>,
    /// Stable file identity when available.
    pub file_id: Option<FileId>,
    /// Entity kind.
    pub kind: EntityKind,
    /// Repository-controlled display name; always untrusted data.
    #[schemars(length(min = 1, max = 1024))]
    pub display_name: String,
    /// Compact repository-controlled signature; always untrusted data.
    #[schemars(length(max = 4096))]
    pub signature: Option<String>,
    /// Repository-relative display path; always untrusted data.
    #[schemars(length(min = 1, max = 8192))]
    pub path: String,
    /// Deterministic integer score from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub score: u16,
    /// Deterministically ordered ranking evidence.
    #[schemars(length(min = 1, max = 16))]
    pub why: Vec<LocateReason>,
    /// Exact source evidence, when available.
    pub source_ref: Option<SourceRef>,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// Server interpretation of a locate query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryInterpretation {
    /// Normalized query tokens.
    #[schemars(length(max = 128))]
    pub tokens: Vec<String>,
    /// Applied search modes.
    #[schemars(length(max = 6))]
    pub modes: BTreeSet<SearchMode>,
    /// Whether optional semantic retrieval was available.
    pub semantic_available: bool,
}

/// Tool suggested as a bounded next action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolSuggestion {
    /// Suggested first-slice read tool.
    pub tool: SuggestedTool,
    /// Stable symbols to carry forward.
    #[schemars(length(max = 16))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Exact source references to carry forward.
    #[schemars(length(max = 32))]
    pub source_refs: Vec<SourceRef>,
}

/// Read tools that may be suggested by the first slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SuggestedTool {
    /// Explain stable symbols.
    #[serde(rename = "symbol.explain")]
    SymbolExplain,
    /// Read exact source evidence.
    #[serde(rename = "source.read")]
    SourceRead,
    /// Refine a locate request.
    #[serde(rename = "code.locate")]
    CodeLocate,
}

/// `code.locate` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLocateData {
    /// Deterministically ranked matches.
    #[schemars(length(max = 200))]
    pub matches: Vec<LocatedItem>,
    /// Deterministic request interpretation.
    pub query_interpretation: QueryInterpretation,
    /// Bounded next-action suggestions.
    #[schemars(length(max = 16))]
    pub suggested_next: Vec<ToolSuggestion>,
}

/// Strict output for `code.locate`.
pub type CodeLocateOutput = ReadEnvelope<CodeLocateData>;

/// Sections accepted by `symbol.explain`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExplainSection {
    /// Compact signature.
    Signature,
    /// Documentation evidence.
    Docs,
    /// Containment evidence.
    Containment,
    /// Type evidence.
    Types,
    /// Outbound and inbound call summary.
    CallsSummary,
    /// Reference summary.
    ReferencesSummary,
    /// History evidence.
    History,
    /// Ownership evidence.
    Ownership,
    /// Diagnostics.
    Diagnostics,
    /// Small source preview.
    SourcePreview,
}

/// Provenance detail requested by a read tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceLevel {
    /// Omit provenance detail beyond mandatory source identity.
    None,
    /// Return compact provider and confidence evidence.
    Compact,
    /// Return maximum bounded provenance evidence.
    Full,
}

/// Strict input for `symbol.explain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplainInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    pub generation: Option<GenerationSelector>,
    /// Stable unambiguous symbols.
    #[schemars(length(min = 1, max = 16))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Requested sections.
    #[schemars(length(max = 10))]
    pub sections: Option<BTreeSet<ExplainSection>>,
    /// Per-relation evidence sample ceiling.
    #[schemars(range(min = 0, max = 25))]
    pub relation_sample_limit: Option<u8>,
    /// Source preview lines per symbol.
    #[schemars(range(min = 0, max = 40))]
    pub source_preview_lines: Option<u8>,
    /// Provenance detail.
    pub include_provenance: Option<ProvenanceLevel>,
    /// Optional lower response limits.
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    pub response_profile: Option<ResponseProfile>,
}

/// Compact relation counts for one symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationSummary {
    /// Exact outbound calls.
    pub outbound_exact: u64,
    /// Candidate outbound calls.
    pub outbound_candidates: u64,
    /// Exact inbound calls.
    pub inbound_exact: u64,
    /// Candidate inbound calls.
    pub inbound_candidates: u64,
    /// Exact references.
    pub references_exact: u64,
}

/// Compact provenance item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSummary {
    /// Provider identity.
    #[schemars(length(min = 1, max = 128))]
    pub provider: String,
    /// Evidence class.
    #[schemars(length(min = 1, max = 128))]
    pub evidence: String,
    /// Confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
}

/// One bounded symbol explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplanation {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind.
    pub kind: EntityKind,
    /// Repository-controlled display name.
    #[schemars(length(min = 1, max = 1024))]
    pub display_name: String,
    /// Repository-controlled signature.
    #[schemars(length(max = 4096))]
    pub signature: Option<String>,
    /// Exact definition evidence.
    pub definition: SourceRef,
    /// Compact relation counts.
    pub relations: RelationSummary,
    /// Bounded provenance.
    #[schemars(length(max = 64))]
    pub provenance: Vec<ProvenanceSummary>,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source-free uncertainty notes.
    #[schemars(length(max = 32))]
    pub uncertainty: Vec<ResponseWarning>,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// Progressive detail handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DetailHandle {
    /// Opaque generation-bound handle.
    #[schemars(length(min = 1, max = 4096))]
    pub handle: String,
    /// Detail class exposed by the handle.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
}

/// `symbol.explain` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplainData {
    /// Explanations in request identity order.
    #[schemars(length(max = 16))]
    pub symbols: Vec<SymbolExplanation>,
    /// Requested identities absent from the pinned generation.
    #[schemars(length(max = 16))]
    pub unresolved_ids: Vec<SymbolId>,
    /// Bounded progressive-disclosure handles.
    #[schemars(length(max = 64))]
    pub detail_handles: Vec<DetailHandle>,
}

/// Strict output for `symbol.explain`.
pub type SymbolExplainOutput = ReadEnvelope<SymbolExplainData>;

/// One source selector accepted by `source.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceReadSelector {
    /// Select an exact source reference.
    Reference(SourceReferenceSelector),
    /// Select a symbol definition.
    Symbol(SymbolDefinitionSelector),
    /// Select a verified byte range in one indexed file.
    FileRange(FileRangeSelector),
}

/// Exact source-reference selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReferenceSelector {
    /// Generation-bound source reference.
    pub source_ref: SourceRef,
}

/// Symbol-definition selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolDefinitionSelector {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
}

/// Explicit indexed-file range selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileRangeSelector {
    /// Stable indexed file identity.
    pub file_id: FileId,
    /// Inclusive start byte.
    pub start_byte: u64,
    /// Exclusive end byte.
    pub end_byte: u64,
}

/// Requested source encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncodingRequest {
    /// Return exact UTF-8 when the complete verified file is valid UTF-8.
    Utf8LosslessWhenValid,
    /// Return explicit base64 for a small non-UTF-8 read.
    BytesBase64,
}

/// Strict input for `source.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    pub generation: Option<GenerationSelector>,
    /// Exact source selectors.
    #[schemars(length(min = 1, max = 32))]
    pub references: Vec<SourceReadSelector>,
    /// Leading context lines.
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_before: Option<u8>,
    /// Trailing context lines.
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_after: Option<u8>,
    /// Merge overlapping verified ranges.
    pub merge_overlaps: Option<bool>,
    /// Aggregate raw source-byte ceiling.
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Include one-based line numbers.
    pub include_line_numbers: Option<bool>,
    /// Requested encoding.
    pub encoding: Option<SourceEncodingRequest>,
    /// Optional lower response limits.
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    pub response_profile: Option<ResponseProfile>,
}

/// Encoding used by one source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncoding {
    /// Exact UTF-8.
    Utf8,
    /// Base64-encoded exact bytes.
    Base64,
}

/// One exact verified source chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceChunk {
    /// Selected generation-bound reference.
    pub source_ref: SourceRef,
    /// Repository-relative display path.
    #[schemars(length(min = 1, max = 8192))]
    pub path: String,
    /// Inclusive byte start.
    pub start_byte: u64,
    /// Exclusive byte end.
    pub end_byte: u64,
    /// One-based first included line.
    pub start_line: u64,
    /// One-based last included line.
    pub end_line: u64,
    /// Exact UTF-8 or base64 text.
    #[schemars(length(max = 699_052))]
    pub content: String,
    /// Content representation.
    pub encoding: SourceEncoding,
    /// Complete-file content identity.
    pub content_hash: ContentHash,
    /// Indexed language identity.
    #[schemars(length(min = 1, max = 256))]
    pub language: String,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// A source selector that no longer resolves in the pinned snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaleSourceReference {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free reason code.
    #[schemars(length(min = 1, max = 128))]
    pub reason: String,
}

/// One source-read elision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceElision {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free elision reason.
    #[schemars(length(min = 1, max = 128))]
    pub reason: String,
    /// Raw bytes omitted.
    pub omitted_bytes: u64,
}

/// `source.read` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadData {
    /// Verified chunks in request order.
    #[schemars(length(max = 32))]
    pub chunks: Vec<SourceChunk>,
    /// Selectors invalid for the pinned generation or source snapshot.
    #[schemars(length(max = 32))]
    pub stale_references: Vec<StaleSourceReference>,
    /// Merged, truncated, or unavailable ranges.
    #[schemars(length(max = 64))]
    pub elisions: Vec<SourceElision>,
    /// Raw bytes returned before JSON escaping.
    #[schemars(range(max = 524_288))]
    pub total_source_bytes: u32,
}

/// Strict output for `source.read`.
pub type SourceReadOutput = ReadEnvelope<SourceReadData>;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::VerticalTool;

    #[test]
    fn embedded_vertical_schemas_are_unique_strict_draft_2020_12_objects() {
        let mut names = BTreeSet::new();
        let mut identifiers = BTreeSet::new();

        for tool in VerticalTool::ALL {
            assert!(names.insert(tool.name()));
            for schema_text in [tool.input_schema_json(), tool.output_schema_json()] {
                let schema: Value =
                    serde_json::from_str(schema_text).expect("checked schema is valid JSON");
                jsonschema::draft202012::new(&schema)
                    .expect("checked schema compiles as JSON Schema 2020-12");
                assert_eq!(
                    schema["$schema"],
                    "https://json-schema.org/draft/2020-12/schema"
                );
                assert_eq!(schema["type"], "object");
                assert_eq!(schema["additionalProperties"], false);
                let identifier = schema["$id"]
                    .as_str()
                    .expect("tool schema has a stable identifier")
                    .to_owned();
                assert!(identifiers.insert(identifier));
            }
        }
    }

    #[test]
    fn repo_index_schema_requires_exactly_one_non_null_target() {
        let schema: Value = serde_json::from_str(VerticalTool::RepoIndex.input_schema_json())
            .expect("checked schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("checked schema compiles");
        assert!(validator.is_valid(&json!({"root": "C:/fixture"})));
        assert!(validator.is_valid(&json!({
            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        })));
        assert!(!validator.is_valid(&json!({})));
        assert!(!validator.is_valid(&json!({
            "root": "C:/fixture",
            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        })));
        assert!(!validator.is_valid(&json!({"root": null})));
    }

    #[test]
    fn optional_input_fields_reject_explicit_null_and_unknown_properties() {
        let schema: Value = serde_json::from_str(VerticalTool::CodeLocate.input_schema_json())
            .expect("checked schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("checked schema compiles");
        let required = json!({
            "repository": {
                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
            },
            "query": "publish"
        });
        assert!(validator.is_valid(&required));

        let mut explicit_null = required.clone();
        explicit_null["max_results"] = Value::Null;
        assert!(!validator.is_valid(&explicit_null));

        let mut unknown = required;
        unknown["host_path"] = json!("must not be accepted");
        assert!(!validator.is_valid(&unknown));
    }
}
