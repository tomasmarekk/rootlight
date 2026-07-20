//! Source definitions for the first MCP vertical-slice tool schemas.
//!
//! The schema generator derives checked public artifacts from these bounded
//! types; transport routing consumes only those generated artifacts.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_error::{PublicError, SafeLabel};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{CoverageStatus, SourceRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ErrorResponse, TrustClassification};

const MAX_SOURCE_FREE_MESSAGE_BYTES: usize = 1_024;
const MAX_CONTINUATION_CURSOR_BYTES: usize = 4_096;
const MAX_SOURCE_READ_BYTES: u64 = 524_288;

/// One tool exposed by the first secure MCP vertical slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerticalTool {
    /// Registers or rebuilds one repository.
    RepoIndex,
    /// Inspects repository state, generation, coverage, and operations.
    RepoStatus,
    /// Lists registered repositories.
    RepoList,
    /// Reads or cancels one operation.
    OperationStatus,
    /// Locates bounded structural or lexical matches.
    CodeLocate,
    /// Explains one or more stable symbols.
    SymbolExplain,
    /// Gets bounded typed relationships around symbols.
    SymbolRelationships,
    /// Traces bounded paths through relation graphs.
    FlowTrace,
    /// Maps changes to affected symbols, dependents, and risks.
    ChangeImpact,
    /// Ranks tests relevant to symbols or changes.
    TestsSelect,
    /// Produces a scoped architecture map.
    ArchitectureOverview,
    /// Finds dependency cycles in a relation projection.
    ArchitectureCycles,
    /// Finds dead or unreachable code candidates.
    CodeDead,
    /// Compares two revisions or generations structurally.
    HistoryCompare,
    /// Produces an ordered change plan.
    PlanChange,
    /// Assembles task-specific evidence under a token budget.
    ContextPack,
    /// Reads generation-pinned source ranges.
    SourceRead,
    /// Executes a bounded expert query over the safe AST.
    QueryAdvanced,
    /// Executes up to sixteen read operations under one generation.
    QueryBatch,
}

impl VerticalTool {
    /// Complete deterministic first-slice tool catalog.
    pub const ALL: [Self; 19] = [
        Self::RepoIndex,
        Self::RepoStatus,
        Self::RepoList,
        Self::OperationStatus,
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SymbolRelationships,
        Self::FlowTrace,
        Self::ChangeImpact,
        Self::TestsSelect,
        Self::ArchitectureOverview,
        Self::ArchitectureCycles,
        Self::CodeDead,
        Self::HistoryCompare,
        Self::PlanChange,
        Self::ContextPack,
        Self::SourceRead,
        Self::QueryAdvanced,
        Self::QueryBatch,
    ];

    /// Stable tool name advertised through MCP.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RepoIndex => "repo.index",
            Self::RepoStatus => "repo.status",
            Self::RepoList => "repo.list",
            Self::OperationStatus => "operation.status",
            Self::CodeLocate => "code.locate",
            Self::SymbolExplain => "symbol.explain",
            Self::SymbolRelationships => "symbol.relationships",
            Self::FlowTrace => "flow.trace",
            Self::ChangeImpact => "change.impact",
            Self::TestsSelect => "tests.select",
            Self::ArchitectureOverview => "architecture.overview",
            Self::ArchitectureCycles => "architecture.cycles",
            Self::CodeDead => "code.dead",
            Self::HistoryCompare => "history.compare",
            Self::PlanChange => "plan.change",
            Self::ContextPack => "context.pack",
            Self::SourceRead => "source.read",
            Self::QueryAdvanced => "query.advanced",
            Self::QueryBatch => "query.batch",
        }
    }

    /// Static source-free title intended for clients.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::RepoIndex => "Index repository",
            Self::RepoStatus => "Inspect repository",
            Self::RepoList => "List repositories",
            Self::OperationStatus => "Inspect operation",
            Self::CodeLocate => "Locate code",
            Self::SymbolExplain => "Explain symbol",
            Self::SymbolRelationships => "Symbol relationships",
            Self::FlowTrace => "Trace flow",
            Self::ChangeImpact => "Change impact",
            Self::TestsSelect => "Select tests",
            Self::ArchitectureOverview => "Architecture overview",
            Self::ArchitectureCycles => "Architecture cycles",
            Self::CodeDead => "Dead code",
            Self::HistoryCompare => "Compare history",
            Self::PlanChange => "Plan change",
            Self::ContextPack => "Context pack",
            Self::SourceRead => "Read source",
            Self::QueryAdvanced => "Advanced query",
            Self::QueryBatch => "Batch query",
        }
    }

    /// Static source-free description intended for models and clients.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::RepoIndex => {
                "Create or update one local repository generation and return its operation handle."
            }
            Self::RepoStatus => {
                "Inspect repository state, generation freshness, coverage, and active operations."
            }
            Self::RepoList => "List registered repositories and workspaces.",
            Self::OperationStatus => "Read or cancel one known long-running Rootlight operation.",
            Self::CodeLocate => {
                "Find bounded, generation-pinned code and file matches by identifier, text, path, or structure."
            }
            Self::SymbolExplain => {
                "Return bounded semantic evidence for stable symbol identifiers."
            }
            Self::SymbolRelationships => {
                "Get bounded typed callers, callees, references, types, implementations, dependencies, tests, or ownership around symbols."
            }
            Self::FlowTrace => {
                "Trace bounded paths through calls, data flow, services, messaging, build, or dependency relations."
            }
            Self::ChangeImpact => {
                "Map a working-tree or Git change set to affected symbols, dependents, services, risks, and tests."
            }
            Self::TestsSelect => {
                "Rank tests relevant to symbols or changes with rationale and uncertainty."
            }
            Self::ArchitectureOverview => {
                "Produce a scoped architecture map of modules, packages, services, data stores, routes, ownership, and hotspots."
            }
            Self::ArchitectureCycles => {
                "Find and explain dependency cycles in a selected relation projection."
            }
            Self::CodeDead => {
                "Find dead or unreachable candidates with entry-point and coverage caveats."
            }
            Self::HistoryCompare => {
                "Compare two revisions or generations structurally and semantically."
            }
            Self::PlanChange => {
                "Produce an ordered change plan with affected symbols, files, tests, risks, and verification steps."
            }
            Self::ContextPack => {
                "Assemble minimal task-specific evidence and source snippets under a token budget."
            }
            Self::SourceRead => {
                "Read exact bounded ranges from a pinned source snapshot as untrusted repository data."
            }
            Self::QueryAdvanced => {
                "Execute a bounded expert query over the documented safe query AST."
            }
            Self::QueryBatch => {
                "Execute up to sixteen independent or dependency-linked read operations under one pinned generation and one shared budget."
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
            Self::RepoStatus => include_str!(
                "../../../schemas/generated/json/mcp-repo-status-input-1.0.schema.json"
            ),
            Self::RepoList => include_str!(
                "../../../schemas/generated/json/mcp-repo-list-input-1.0.schema.json"
            ),
            Self::OperationStatus => include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.0.schema.json"
            ),
            Self::CodeLocate => include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.0.schema.json"
            ),
            Self::SymbolExplain => include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.0.schema.json"
            ),
            Self::SymbolRelationships => include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-input-1.0.schema.json"
            ),
            Self::FlowTrace => include_str!(
                "../../../schemas/generated/json/mcp-flow-trace-input-1.0.schema.json"
            ),
            Self::ChangeImpact => include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.0.schema.json"
            ),
            Self::TestsSelect => include_str!(
                "../../../schemas/generated/json/mcp-tests-select-input-1.0.schema.json"
            ),
            Self::ArchitectureOverview => include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-input-1.0.schema.json"
            ),
            Self::ArchitectureCycles => include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-input-1.0.schema.json"
            ),
            Self::CodeDead => include_str!(
                "../../../schemas/generated/json/mcp-code-dead-input-1.0.schema.json"
            ),
            Self::HistoryCompare => include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.0.schema.json"
            ),
            Self::PlanChange => include_str!(
                "../../../schemas/generated/json/mcp-plan-change-input-1.0.schema.json"
            ),
            Self::ContextPack => include_str!(
                "../../../schemas/generated/json/mcp-context-pack-input-1.0.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-input-1.0.schema.json"
            ),
            Self::QueryAdvanced => include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.0.schema.json"
            ),
            Self::QueryBatch => include_str!(
                "../../../schemas/generated/json/mcp-query-batch-input-1.0.schema.json"
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
            Self::RepoStatus => include_str!(
                "../../../schemas/generated/json/mcp-repo-status-output-1.0.schema.json"
            ),
            Self::RepoList => include_str!(
                "../../../schemas/generated/json/mcp-repo-list-output-1.0.schema.json"
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
            Self::SymbolRelationships => include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-output-1.0.schema.json"
            ),
            Self::FlowTrace => include_str!(
                "../../../schemas/generated/json/mcp-flow-trace-output-1.0.schema.json"
            ),
            Self::ChangeImpact => include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.0.schema.json"
            ),
            Self::TestsSelect => include_str!(
                "../../../schemas/generated/json/mcp-tests-select-output-1.0.schema.json"
            ),
            Self::ArchitectureOverview => include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-output-1.0.schema.json"
            ),
            Self::ArchitectureCycles => include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-output-1.0.schema.json"
            ),
            Self::CodeDead => include_str!(
                "../../../schemas/generated/json/mcp-code-dead-output-1.0.schema.json"
            ),
            Self::HistoryCompare => include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.0.schema.json"
            ),
            Self::PlanChange => include_str!(
                "../../../schemas/generated/json/mcp-plan-change-output-1.0.schema.json"
            ),
            Self::ContextPack => include_str!(
                "../../../schemas/generated/json/mcp-context-pack-output-1.0.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-output-1.0.schema.json"
            ),
            Self::QueryAdvanced => include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.0.schema.json"
            ),
            Self::QueryBatch => include_str!(
                "../../../schemas/generated/json/mcp-query-batch-output-1.0.schema.json"
            ),
        }
    }

    /// Whether the tool only reads already published state.
    #[must_use]
    pub const fn read_only(self) -> bool {
        !matches!(self, Self::RepoIndex)
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

/// A checked success-or-error result accepted by one tool output schema.
///
/// Successful variants preserve each tool's documented response shape.
/// Expected domain failures use the same versioned [`ErrorResponse`] and
/// checked [`PublicError`] contract for every tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolResponse<T> {
    /// A tool-specific successful response.
    Success(T),
    /// A versioned source-redacted domain error.
    Error(ErrorResponse),
}

/// A property that must be present and may contain JSON `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequiredNullable<T>(pub Option<T>);

/// A bounded opaque continuation cursor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ContinuationCursor(#[schemars(length(min = 1, max = 4096))] String);

impl ContinuationCursor {
    /// Parses a nonempty cursor within the 4096-byte wire limit.
    ///
    /// # Errors
    ///
    /// Returns [`McpContractError::InvalidContinuationCursor`] when the value
    /// is empty or exceeds the byte limit.
    pub fn parse(value: &str) -> Result<Self, McpContractError> {
        if value.is_empty() || value.len() > MAX_CONTINUATION_CURSOR_BYTES {
            return Err(McpContractError::InvalidContinuationCursor);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the opaque cursor text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContinuationCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A checked Rootlight-generated message that cannot contain paths or source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct SourceFreeMessage(
    #[schemars(length(min = 1, max = 1024), regex(pattern = r"^[a-z0-9 -]+$"))] String,
);

impl SourceFreeMessage {
    /// Parses a bounded lowercase source-free message template.
    ///
    /// # Errors
    ///
    /// Returns [`McpContractError::InvalidSourceFreeMessage`] when the value
    /// is empty, oversized, or outside the safe character allow-list.
    pub fn parse(value: &str) -> Result<Self, McpContractError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_SOURCE_FREE_MESSAGE_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b' ' | b'-')
            });
        if !valid {
            return Err(McpContractError::InvalidSourceFreeMessage);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the checked source-free message.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SourceFreeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Semantic validation failures in the MCP wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpContractError {
    /// A continuation cursor is empty or exceeds its byte ceiling.
    #[error("invalid continuation cursor")]
    InvalidContinuationCursor,
    /// A source-free message violates its bounded template policy.
    #[error("invalid source-free message")]
    InvalidSourceFreeMessage,
    /// A direct file range is inverted.
    #[error("invalid source file range")]
    InvalidFileRange,
    /// A source chunk does not match its reference, encoding, or range.
    #[error("invalid source chunk")]
    InvalidSourceChunk,
    /// Source chunk bytes do not match the declared aggregate.
    #[error("invalid source byte total")]
    InvalidSourceByteTotal,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: Option<u16>,
    /// Maximum estimated output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 100, max = 3200))]
    pub max_tokens: Option<u16>,
    /// Maximum source bytes before JSON escaping.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Maximum relationship or traversal facts examined.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100_000))]
    pub max_traversal_facts: Option<u32>,
    /// Maximum plan depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 16))]
    pub max_depth: Option<u8>,
    /// Maximum independently returned paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_paths: Option<u16>,
    /// Cooperative request deadline in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 10, max = 30_000))]
    pub timeout_ms: Option<u32>,
    /// Requested evidence detail.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 8192)))]
    pub paths: BTreeSet<String>,
}

/// Package scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageScope {
    /// Distinct package identities.
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 512)))]
    pub packages: BTreeSet<String>,
}

/// Build-target scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildTargetScope {
    /// Distinct build-target identities.
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 512)))]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub root: Option<String>,
    /// Existing repository identity to update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    /// Optional indexing scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<IndexScope>,
    /// Requested indexing mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<IndexMode>,
    /// Per-language maximum requested tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 64))]
    pub requested_tiers: Option<BTreeMap<String, AnalysisTier>>,
    /// Validated operation-scoped configuration override.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 128))]
    pub configuration_patch: Option<BTreeMap<String, Value>>,
    /// Maximum time to wait for publication or a terminal state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Whether the operation may continue after client disconnect.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[schemars(length(max = 64), inner(length(min = 1, max = 128)))]
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
    pub code: SafeLabel,
    /// Static or Rootlight-generated source-free message.
    pub message: SourceFreeMessage,
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
pub struct RepoIndexSuccess {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operational result.
    pub data: RepoIndexData,
}

/// Checked success-or-error output for `repo.index`.
pub type RepoIndexOutput = ToolResponse<RepoIndexSuccess>;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<OperationAction>,
    /// Maximum long-poll duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Return immediately only after this journal revision.
    #[serde(skip_serializing_if = "Option::is_none")]
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
pub struct OperationStatusSuccess {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operation result.
    pub data: OperationStatusData,
}

/// Checked success-or-error output for `operation.status`.
pub type OperationStatusOutput = ToolResponse<OperationStatusSuccess>;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Identifier, path, text, or concept query.
    #[schemars(length(min = 1, max = 2048))]
    pub query: String,
    /// Entity kinds to retain.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub kinds: Option<BTreeSet<EntityKind>>,
    /// Optional structural scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Language identity filters.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32), inner(length(min = 1, max = 64)))]
    pub languages: Option<BTreeSet<String>>,
    /// Retrieval modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 6))]
    pub search_modes: Option<BTreeSet<SearchMode>>,
    /// Structural seed symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 16))]
    pub related_to: Option<BTreeSet<SymbolId>>,
    /// Minimum relation confidence from 0 through 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Maximum returned matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: Option<u16>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Opaque generation-bound continuation cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    pub code: SafeLabel,
    /// Rootlight-generated source-free explanation.
    pub message: SourceFreeMessage,
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
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting.
    pub usage: UsageSummary,
    /// Source-free warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
    /// Response-level classification for all repository-derived content.
    pub trust: TrustClassification,
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
    #[schemars(length(max = 128), inner(length(min = 1, max = 256)))]
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

/// Checked success-or-error output for `code.locate`.
pub type CodeLocateOutput = ToolResponse<ReadEnvelope<CodeLocateData>>;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Stable unambiguous symbols.
    #[schemars(length(min = 1, max = 16))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Requested sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10))]
    pub sections: Option<BTreeSet<ExplainSection>>,
    /// Per-relation evidence sample ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 25))]
    pub relation_sample_limit: Option<u8>,
    /// Source preview lines per symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 40))]
    pub source_preview_lines: Option<u8>,
    /// Provenance detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_provenance: Option<ProvenanceLevel>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// Checked success-or-error output for `symbol.explain`.
pub type SymbolExplainOutput = ToolResponse<ReadEnvelope<SymbolExplainData>>;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileRangeSelector {
    /// Stable indexed file identity.
    pub file_id: FileId,
    /// Inclusive start byte.
    pub start_byte: u64,
    /// Exclusive end byte.
    pub end_byte: u64,
}

impl<'de> Deserialize<'de> for FileRangeSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireFileRangeSelector {
            file_id: FileId,
            start_byte: u64,
            end_byte: u64,
        }

        let wire = WireFileRangeSelector::deserialize(deserializer)?;
        if wire.start_byte > wire.end_byte {
            return Err(serde::de::Error::custom(McpContractError::InvalidFileRange));
        }
        Ok(Self {
            file_id: wire.file_id,
            start_byte: wire.start_byte,
            end_byte: wire.end_byte,
        })
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Exact source selectors.
    #[schemars(length(min = 1, max = 32))]
    pub references: Vec<SourceReadSelector>,
    /// Leading context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_before: Option<u8>,
    /// Trailing context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_after: Option<u8>,
    /// Merge overlapping verified ranges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_overlaps: Option<bool>,
    /// Aggregate raw source-byte ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Include one-based line numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_line_numbers: Option<bool>,
    /// Requested encoding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<SourceEncodingRequest>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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

impl SourceChunk {
    fn represented_source_bytes(&self) -> Result<u64, McpContractError> {
        represented_source_bytes(&self.content, self.encoding)
            .ok_or(McpContractError::InvalidSourceChunk)
    }
}

impl<'de> Deserialize<'de> for SourceChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSourceChunk {
            source_ref: SourceRef,
            path: String,
            start_byte: u64,
            end_byte: u64,
            start_line: u64,
            end_line: u64,
            content: String,
            encoding: SourceEncoding,
            content_hash: ContentHash,
            language: String,
            generated: bool,
            trust: TrustClassification,
        }

        let wire = WireSourceChunk::deserialize(deserializer)?;
        let span = wire.source_ref.span();
        let represented_bytes = represented_source_bytes(&wire.content, wire.encoding)
            .ok_or_else(|| serde::de::Error::custom(McpContractError::InvalidSourceChunk))?;
        let span_bytes = wire
            .end_byte
            .checked_sub(wire.start_byte)
            .ok_or_else(|| serde::de::Error::custom(McpContractError::InvalidSourceChunk))?;
        let line_hint_matches = wire.source_ref.line_hint().is_none_or(|line_hint| {
            line_hint.start_line() == wire.start_line && line_hint.end_line() == wire.end_line
        });
        if wire.start_line == 0
            || wire.start_line > wire.end_line
            || represented_bytes != span_bytes
            || span.start_byte() != wire.start_byte
            || span.end_byte() != wire.end_byte
            || wire.source_ref.content_hash() != wire.content_hash
            || !line_hint_matches
        {
            return Err(serde::de::Error::custom(
                McpContractError::InvalidSourceChunk,
            ));
        }

        Ok(Self {
            source_ref: wire.source_ref,
            path: wire.path,
            start_byte: wire.start_byte,
            end_byte: wire.end_byte,
            start_line: wire.start_line,
            end_line: wire.end_line,
            content: wire.content,
            encoding: wire.encoding,
            content_hash: wire.content_hash,
            language: wire.language,
            generated: wire.generated,
            trust: wire.trust,
        })
    }
}

fn represented_source_bytes(content: &str, encoding: SourceEncoding) -> Option<u64> {
    match encoding {
        SourceEncoding::Utf8 => u64::try_from(content.len()).ok(),
        SourceEncoding::Base64 => canonical_base64_decoded_len(content),
    }
}

fn canonical_base64_decoded_len(content: &str) -> Option<u64> {
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return Some(0);
    }
    if !bytes.len().is_multiple_of(4) {
        return None;
    }

    let padding = if bytes.ends_with(b"==") {
        2usize
    } else if bytes.ends_with(b"=") {
        1usize
    } else {
        0usize
    };
    let data_len = bytes.len().checked_sub(padding)?;
    if bytes[..data_len]
        .iter()
        .any(|byte| base64_value(*byte).is_none())
        || bytes[data_len..].iter().any(|byte| *byte != b'=')
    {
        return None;
    }

    if padding == 1 {
        let last = *bytes.get(data_len.checked_sub(1)?)?;
        if base64_value(last)? & 0b11 != 0 {
            return None;
        }
    } else if padding == 2 {
        let last = *bytes.get(data_len.checked_sub(1)?)?;
        if base64_value(last)? & 0b1111 != 0 {
            return None;
        }
    }

    let quartets = u64::try_from(bytes.len() / 4).ok()?;
    quartets
        .checked_mul(3)?
        .checked_sub(u64::try_from(padding).ok()?)
}

const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// A source selector that no longer resolves in the pinned snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaleSourceReference {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free reason code.
    pub reason: SafeLabel,
}

/// One source-read elision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceElision {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free elision reason.
    pub reason: SafeLabel,
    /// Raw bytes omitted.
    pub omitted_bytes: u64,
}

/// `source.read` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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

impl<'de> Deserialize<'de> for SourceReadData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSourceReadData {
            chunks: Vec<SourceChunk>,
            stale_references: Vec<StaleSourceReference>,
            elisions: Vec<SourceElision>,
            total_source_bytes: u32,
        }

        let wire = WireSourceReadData::deserialize(deserializer)?;
        let mut observed = 0u64;
        for chunk in &wire.chunks {
            observed = observed
                .checked_add(
                    chunk
                        .represented_source_bytes()
                        .map_err(serde::de::Error::custom)?,
                )
                .ok_or_else(|| {
                    serde::de::Error::custom(McpContractError::InvalidSourceByteTotal)
                })?;
        }
        if observed != u64::from(wire.total_source_bytes) || observed > MAX_SOURCE_READ_BYTES {
            return Err(serde::de::Error::custom(
                McpContractError::InvalidSourceByteTotal,
            ));
        }

        Ok(Self {
            chunks: wire.chunks,
            stale_references: wire.stale_references,
            elisions: wire.elisions,
            total_source_bytes: wire.total_source_bytes,
        })
    }
}

/// Checked success-or-error output for `source.read`.
pub type SourceReadOutput = ToolResponse<ReadEnvelope<SourceReadData>>;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::{
        CodeLocateInput, CodeLocateOutput, OperationStatusInput, OperationStatusOutput,
        RepoIndexInput, RepoIndexOutput, SourceReadInput, SourceReadOutput, SymbolExplainInput,
        SymbolExplainOutput, VerticalTool,
    };
    use crate::repository::{RepoListInput, RepoListOutput, RepoStatusInput, RepoStatusOutput};

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
                assert!(
                    schema["additionalProperties"] == false
                        || schema["unevaluatedProperties"] == false
                );
                assert_every_object_declares_additional_properties(&schema);
                let identifier = schema["$id"]
                    .as_str()
                    .expect("tool schema has a stable identifier")
                    .to_owned();
                assert!(identifiers.insert(identifier));
            }
        }
    }

    fn assert_every_object_declares_additional_properties(value: &Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_every_object_declares_additional_properties(value);
                }
            }
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("object") {
                    assert!(
                        object.contains_key("additionalProperties")
                            || object.contains_key("unevaluatedProperties"),
                        "object schema is missing a closed-properties contract: {object:?}"
                    );
                }
                for value in object.values() {
                    assert_every_object_declares_additional_properties(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
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

    #[test]
    fn retained_tool_contracts_round_trip_through_rust_and_json_schema() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        let tools = fixture["tools"]
            .as_array()
            .expect("retained tool contracts contain a tool array");
        assert_eq!(tools.len(), VerticalTool::ALL.len());

        for fixture in tools {
            let name = fixture["tool"].as_str().expect("tool name is a string");
            let input = fixture["input"].clone();
            let output = fixture["output"].clone();
            match name {
                "repo.index" => {
                    assert_round_trip::<RepoIndexInput>(VerticalTool::RepoIndex, &input, true);
                    assert_round_trip::<RepoIndexOutput>(VerticalTool::RepoIndex, &output, false);
                }
                "repo.status" => {
                    assert_round_trip::<RepoStatusInput>(VerticalTool::RepoStatus, &input, true);
                    assert_round_trip::<RepoStatusOutput>(VerticalTool::RepoStatus, &output, false);
                }
                "repo.list" => {
                    assert_round_trip::<RepoListInput>(VerticalTool::RepoList, &input, true);
                    assert_round_trip::<RepoListOutput>(VerticalTool::RepoList, &output, false);
                }
                "operation.status" => {
                    assert_round_trip::<OperationStatusInput>(
                        VerticalTool::OperationStatus,
                        &input,
                        true,
                    );
                    assert_round_trip::<OperationStatusOutput>(
                        VerticalTool::OperationStatus,
                        &output,
                        false,
                    );
                }
                "code.locate" => {
                    assert_round_trip::<CodeLocateInput>(VerticalTool::CodeLocate, &input, true);
                    assert_round_trip::<CodeLocateOutput>(VerticalTool::CodeLocate, &output, false);
                }
                "symbol.explain" => {
                    assert_round_trip::<SymbolExplainInput>(
                        VerticalTool::SymbolExplain,
                        &input,
                        true,
                    );
                    assert_round_trip::<SymbolExplainOutput>(
                        VerticalTool::SymbolExplain,
                        &output,
                        false,
                    );
                }
                "source.read" => {
                    assert_round_trip::<SourceReadInput>(VerticalTool::SourceRead, &input, true);
                    assert_round_trip::<SourceReadOutput>(VerticalTool::SourceRead, &output, false);
                }
                other => panic!("unexpected retained tool contract {other}"),
            }
        }
    }

    fn assert_round_trip<T>(tool: VerticalTool, fixture: &Value, input: bool)
    where
        T: DeserializeOwned + Serialize + PartialEq + Debug,
    {
        let decoded: T =
            serde_json::from_value(fixture.clone()).expect("fixture decodes through Rust");
        let encoded = serde_json::to_value(&decoded).expect("typed fixture serializes");
        assert_eq!(
            &encoded,
            fixture,
            "absent optional fields remain absent for {}",
            tool.name()
        );
        let schema_text = if input {
            tool.input_schema_json()
        } else {
            tool.output_schema_json()
        };
        let schema: Value = serde_json::from_str(schema_text).expect("tool schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("tool schema compiles");
        assert!(
            validator.is_valid(&encoded),
            "{} fixture passes its generated schema",
            tool.name()
        );
        let round_tripped: T =
            serde_json::from_value(encoded).expect("serialized fixture decodes through Rust");
        assert_eq!(round_tripped, decoded);
    }

    #[test]
    fn continuation_cursor_is_required_nullable_and_bounded() {
        let fixture = retained_tool_output("code.locate");
        let schema: Value = serde_json::from_str(VerticalTool::CodeLocate.output_schema_json())
            .expect("valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");
        assert!(validator.is_valid(&fixture));

        let mut absent = fixture.clone();
        absent
            .as_object_mut()
            .expect("output is an object")
            .remove("next_cursor");
        assert!(!validator.is_valid(&absent));

        let mut maximum = fixture.clone();
        maximum["next_cursor"] = json!("c".repeat(4_096));
        assert!(validator.is_valid(&maximum));
        serde_json::from_value::<CodeLocateOutput>(maximum).expect("maximum-sized cursor decodes");

        let mut oversized = fixture;
        oversized["next_cursor"] = json!("c".repeat(4_097));
        assert!(!validator.is_valid(&oversized));
        assert!(serde_json::from_value::<CodeLocateOutput>(oversized).is_err());
    }

    #[test]
    fn every_tool_accepts_only_the_checked_versioned_error_envelope() {
        let error = json!({
            "schema_version": "1.0",
            "error": {
                "code": "NOT_FOUND",
                "message": "requested entity was not found",
                "retryable": false,
                "retry_after_ms": null,
                "repository": null,
                "operation": null,
                "generation": null,
                "details": {},
                "next_actions": []
            }
        });
        for tool in VerticalTool::ALL {
            let schema: Value =
                serde_json::from_str(tool.output_schema_json()).expect("output schema is valid");
            let validator = jsonschema::draft202012::new(&schema).expect("output schema compiles");
            assert!(
                validator.is_valid(&error),
                "{} accepts the shared error envelope",
                tool.name()
            );
        }
        serde_json::from_value::<RepoIndexOutput>(error.clone()).expect("repo error decodes");
        serde_json::from_value::<OperationStatusOutput>(error.clone())
            .expect("operation error decodes");
        serde_json::from_value::<CodeLocateOutput>(error.clone()).expect("locate error decodes");
        serde_json::from_value::<SymbolExplainOutput>(error.clone())
            .expect("explain error decodes");
        serde_json::from_value::<SourceReadOutput>(error.clone()).expect("source error decodes");

        let mut arbitrary_code = error.clone();
        arbitrary_code["error"]["code"] = json!("EXECUTOR_PRIVATE_CODE");
        assert!(serde_json::from_value::<RepoIndexOutput>(arbitrary_code).is_err());

        let mut source_shaped_message = error;
        source_shaped_message["error"]["message"] = json!("C:\\Users\\person\\secret.rs");
        assert!(serde_json::from_value::<RepoIndexOutput>(source_shaped_message).is_err());
    }

    #[test]
    fn diagnostics_and_warnings_reject_source_shaped_messages() {
        let mut diagnostic = retained_tool_output("repo.index");
        diagnostic["data"]["diagnostics"] = json!([{
            "code": "fixture",
            "message": "C:\\Users\\person\\secret.rs"
        }]);
        assert!(serde_json::from_value::<RepoIndexOutput>(diagnostic).is_err());

        let mut warning = retained_tool_output("code.locate");
        warning["warnings"] = json!([{
            "code": "fixture",
            "message": "src/lib.rs was skipped"
        }]);
        assert!(serde_json::from_value::<CodeLocateOutput>(warning).is_err());
    }

    #[test]
    fn file_ranges_and_source_chunks_enforce_cross_field_invariants() {
        let mut inverted_input = retained_tool_input("source.read");
        inverted_input["references"][0]["start_byte"] = json!(11);
        inverted_input["references"][0]["end_byte"] = json!(10);
        assert!(serde_json::from_value::<SourceReadInput>(inverted_input).is_err());

        let fixture = retained_tool_output("source.read");
        let mut mismatched_span = fixture.clone();
        mismatched_span["data"]["chunks"][0]["end_byte"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_span).is_err());

        let mut mismatched_reference = fixture.clone();
        mismatched_reference["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_reference).is_err());

        let mut mismatched_hash = fixture.clone();
        mismatched_hash["data"]["chunks"][0]["content_hash"] =
            json!("b3_75bprqkwsgfv4kw74qjkj2xli5knc43nxoicyuklbc4qajuuj6gsxi36m4");
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_hash).is_err());

        let mut mismatched_line_hint = fixture.clone();
        mismatched_line_hint["data"]["chunks"][0]["source_ref"]["line_hint"]["end_line"] = json!(2);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_line_hint).is_err());

        let mut mismatched_total = fixture;
        mismatched_total["data"]["total_source_bytes"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_total).is_err());
    }

    #[test]
    fn source_chunk_base64_length_is_checked_canonically() {
        let fixture = retained_tool_output("source.read");
        let mut valid = fixture.clone();
        valid["data"]["chunks"][0]["content"] = json!("AQID");
        valid["data"]["chunks"][0]["encoding"] = json!("base64");
        valid["data"]["chunks"][0]["end_byte"] = json!(3);
        valid["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(3);
        valid["data"]["total_source_bytes"] = json!(3);
        serde_json::from_value::<SourceReadOutput>(valid).expect("canonical base64 decodes");

        let mut noncanonical = fixture;
        noncanonical["data"]["chunks"][0]["content"] = json!("AQI=");
        noncanonical["data"]["chunks"][0]["encoding"] = json!("base64");
        noncanonical["data"]["chunks"][0]["end_byte"] = json!(2);
        noncanonical["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(2);
        noncanonical["data"]["total_source_bytes"] = json!(2);
        noncanonical["data"]["chunks"][0]["content"] = json!("AQJ=");
        assert!(serde_json::from_value::<SourceReadOutput>(noncanonical).is_err());
    }

    fn retained_tool_input(name: &str) -> Value {
        retained_tool_fixture(name, "input")
    }

    fn retained_tool_output(name: &str) -> Value {
        retained_tool_fixture(name, "output")
    }

    fn retained_tool_fixture(name: &str, field: &str) -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        fixture["tools"]
            .as_array()
            .expect("tool contracts contain an array")
            .iter()
            .find(|entry| entry["tool"] == name)
            .unwrap_or_else(|| panic!("retained tool contract {name} exists"))[field]
            .clone()
    }
}
