//! Strict typed schemas for repository lifecycle MCP tools.
//!
//! These types define the bounded wire contract for `repo.status` and
//! `repo.list`. The schema generator derives checked public artifacts from
//! these bounded types; transport routing consumes only those generated
//! artifacts.

use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use rootlight_ir::CoverageStatus;
use crate::vertical::{
    GenerationSelector, GenerationSummary, OperationState, ReadEnvelope, RepositorySelector,
    RequiredNullable, ResponseBudget, ResponseProfile, ToolResponse,
};

/// Strict input for `repo.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoStatusInput {
    /// Repository to inspect.
    pub repository: RepositorySelector,
    /// Active or explicit generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Requested coverage granularity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_detail: Option<CoverageDetail>,
    /// Include active and most recent operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_operations: Option<bool>,
    /// Minimum freshness requirement for the response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_freshness: Option<FreshnessRequirement>,
    /// Optional response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested evidence detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
}

/// Requested coverage reporting granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageDetail {
    /// Aggregate status only.
    Summary,
    /// Per-language tier and file counts.
    Language,
    /// Per-project or package breakdown.
    Project,
    /// Per-file coverage rows, requires a scope.
    File,
}

/// Minimum freshness the caller requires before the response is useful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessRequirement {
    /// No freshness requirement.
    None,
    /// Structural tier must be fresh.
    Structural,
    /// Semantic tier must be fresh.
    Semantic,
}

/// Overall repository health state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryState {
    /// Repository is indexed and queryable.
    Ready,
    /// An indexing operation is in progress.
    Indexing,
    /// Repository is queryable but some capabilities are reduced.
    Degraded,
    /// Index integrity checks failed.
    Corrupt,
    /// A schema migration is required before use.
    MigrationRequired,
    /// A full rebuild is required.
    RebuildRequired,
}

/// Compact operation summary for repository status reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationSummary {
    /// Stable operation identity.
    pub operation_id: OperationId,
    /// Operation kind label.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Current operation state.
    pub state: OperationState,
    /// Completion fraction, zero to one thousand.
    #[schemars(range(max = 1000))]
    pub progress_permille: u16,
    /// Whether this operation was started by the current session.
    pub owned_by_session: bool,
}

/// Per-language coverage report entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LanguageCoverageReport {
    /// Language identifier.
    #[schemars(length(min = 1, max = 64))]
    pub language: String,
    /// Observed analysis tier.
    #[schemars(length(min = 1, max = 2))]
    pub tier: String,
    /// Number of files indexed for this language.
    pub files_indexed: u64,
    /// Number of files skipped or unresolved.
    pub files_skipped: u64,
    /// Number of files requiring build context not available.
    pub missing_build_context: u64,
}

/// Coverage report at the requested granularity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageReport {
    /// Aggregate coverage status.
    pub status: CoverageStatus,
    /// Per-language breakdown, deterministically ordered.
    #[schemars(length(max = 64))]
    pub languages: Vec<LanguageCoverageReport>,
    /// Total files discovered in the repository scope.
    pub total_files: u64,
    /// Total files indexed.
    pub indexed_files: u64,
    /// Total files skipped by policy or capability.
    pub skipped_files: u64,
}

/// `repo.status` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoStatusData {
    /// Overall repository health.
    pub repository_state: RepositoryState,
    /// Active generation summary, null when no generation is published.
    pub active_generation: RequiredNullable<GenerationSummary>,
    /// Coverage at the requested granularity.
    pub coverage: CoverageReport,
    /// Bounded operation list, most recent first.
    #[schemars(length(max = 100))]
    pub operations: Vec<OperationSummary>,
    /// Recommended next actions for the agent.
    #[schemars(length(max = 8))]
    pub recommended_actions: Vec<crate::vertical::SourceFreeMessage>,
}

/// Checked success-or-error output for `repo.status`.
pub type RepoStatusOutput = ToolResponse<ReadEnvelope<RepoStatusData>>;

/// Strict input for `repo.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoListInput {
    /// Case-folded display-name or root filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 256))]
    pub query: Option<String>,
    /// Filter by repository state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 8))]
    pub states: Option<Vec<RepositoryState>>,
    /// Maximum results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: Option<u16>,
    /// Opaque continuation cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<crate::vertical::ContinuationCursor>,
    /// Requested evidence detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
}

/// One registered repository entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryEntry {
    /// Stable repository identity.
    pub repository_id: RepositoryId,
    /// Rootlight-owned display label.
    #[schemars(length(min = 1, max = 256))]
    pub display_name: String,
    /// Current repository state.
    pub state: RepositoryState,
    /// Active generation, if published.
    pub active_generation: RequiredNullable<GenerationId>,
    /// Number of published generations.
    pub generation_count: u64,
    /// Registered alias, if configured.
    #[schemars(length(max = 256))]
    pub alias: RequiredNullable<String>,
}

/// `repo.list` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoListData {
    /// Registered repositories in deterministic order.
    #[schemars(length(max = 200))]
    pub repositories: Vec<RepositoryEntry>,
    /// Total registered repositories matching the filter.
    pub total_count: u64,
}

/// Checked success-or-error output for `repo.list`.
pub type RepoListOutput = ToolResponse<ReadEnvelope<RepoListData>>;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RepoListInput, RepoStatusInput};

    #[test]
    fn repo_status_input_requires_repository() {
        let valid: RepoStatusInput = serde_json::from_value(json!({
            "repository": {"repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"}
        }))
        .expect("valid input decodes");
        assert!(valid.generation.is_none());
        assert!(valid.coverage_detail.is_none());

        let invalid = serde_json::from_value::<RepoStatusInput>(json!({}));
        assert!(invalid.is_err(), "missing repository must be rejected");
    }

    #[test]
    fn repo_status_input_rejects_unknown_fields() {
        let invalid = serde_json::from_value::<RepoStatusInput>(json!({
            "repository": {"repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"},
            "host_path": "must not be accepted"
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn repo_list_input_accepts_empty_object() {
        let valid: RepoListInput = serde_json::from_value(json!({})).expect("empty is valid");
        assert!(valid.query.is_none());
        assert!(valid.states.is_none());
        assert!(valid.max_results.is_none());
        assert!(valid.cursor.is_none());
    }

    #[test]
    fn repo_list_input_accepts_bounded_query_and_max_results() {
        let valid: RepoListInput = serde_json::from_value(json!({
            "query": "a".repeat(256),
            "max_results": 200
        }))
        .expect("boundary values decode");
        assert_eq!(valid.query.as_deref().map(str::len), Some(256));
        assert_eq!(valid.max_results, Some(200));
    }
}
