//! Strict typed schemas for repository lifecycle MCP tools.
//!
//! These types define the bounded wire contract for `repo.status` and
//! `repo.list`. The schema generator derives checked public artifacts from
//! these bounded types; transport routing consumes only those generated
//! artifacts.

use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TrustClassification;
use crate::vertical::{
    AnalysisReadEnvelope, AnalysisToolResponse, ContinuationCursor, Freshness, GenerationSelector,
    GenerationSummary, OperationState, ReadEnvelope, RepositorySelector, RequiredNullable,
    ResponseBudget, ResponseProfile, ResponseWarning, ToolResponse, UsageSummary,
};
use rootlight_ir::CoverageStatus;

const MAX_CATALOG_SNAPSHOT_ID_BYTES: usize = 128;

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
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
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

/// Publication relationship of the selected generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenerationPublicationState {
    /// The selected generation is the active published generation.
    Published,
    /// The selected generation is retained but no longer active.
    Retained,
}

/// Overall repository health state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
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

impl RepositoryState {
    /// Returns the canonical wire label used for filtering and plan binding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Indexing => "indexing",
            Self::Degraded => "degraded",
            Self::Corrupt => "corrupt",
            Self::MigrationRequired => "migration_required",
            Self::RebuildRequired => "rebuild_required",
        }
    }
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
    /// Explicit generation requested by the caller, null for the active selector.
    pub requested_generation: RequiredNullable<GenerationId>,
    /// Exact immutable generation resolved for this response.
    pub resolved_generation: GenerationId,
    /// Active generation summary, null when no generation is published.
    pub active_generation: RequiredNullable<GenerationSummary>,
    /// Publication relationship of the selected generation.
    pub publication_state: GenerationPublicationState,
    /// Durable bytes retained for the selected immutable generation.
    pub retained_durable_bytes: u64,
    /// Registered repository alias, when configured.
    pub alias: RequiredNullable<String>,
    /// Coverage at the requested granularity.
    pub coverage: CoverageReport,
    /// Bounded operation list, most recent first.
    #[schemars(length(max = 100))]
    pub operations: Vec<OperationSummary>,
    /// Recommended next actions for the agent.
    #[schemars(length(max = 8))]
    pub recommended_actions: Vec<crate::vertical::SourceFreeMessage>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `repo.status` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "RepoStatusData")]
pub struct RepoStatusDataV1_0 {
    /// Overall repository health.
    pub repository_state: RepositoryState,
    /// Explicit generation requested by the caller, null for the active selector.
    pub requested_generation: RequiredNullable<GenerationId>,
    /// Exact immutable generation resolved for this response.
    pub resolved_generation: GenerationId,
    /// Active generation summary, null when no generation is published.
    pub active_generation: RequiredNullable<GenerationSummary>,
    /// Publication relationship of the selected generation.
    pub publication_state: GenerationPublicationState,
    /// Registered repository alias, when configured.
    pub alias: RequiredNullable<String>,
    /// Coverage at the requested granularity.
    pub coverage: CoverageReport,
    /// Bounded operation list, most recent first.
    #[schemars(length(max = 100))]
    pub operations: Vec<OperationSummary>,
    /// Recommended next actions for the agent.
    #[schemars(length(max = 8))]
    pub recommended_actions: Vec<crate::vertical::SourceFreeMessage>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked `repo.status` output retained for explicit 1.0 callers.
pub type RepoStatusOutputV1_0 = ToolResponse<ReadEnvelope<RepoStatusDataV1_0>>;

/// Checked `repo.status` output for additive schema 1.1.
pub type RepoStatusOutputV1_1 = AnalysisToolResponse<AnalysisReadEnvelope<RepoStatusData>>;

/// Current checked success-or-error output for `repo.status`.
pub type RepoStatusOutput = RepoStatusOutputV1_1;

/// Strict input for `repo.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoListInput {
    /// Case-folded authoritative display-name or alias filter.
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
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Version marker carried by successful `repo.list` catalog responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RepoListSchemaVersion {
    /// Repository catalog contract version 2.0.
    #[serde(rename = "2.0")]
    V2_0,
}

/// A bounded opaque source-free identity for one immutable catalog snapshot.
///
/// The identifier is correlation metadata only. It cannot contain a storage
/// key, local path, credential, or other secret-shaped punctuation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct CatalogSnapshotId(
    #[schemars(length(min = 1, max = 128), regex(pattern = r"^[a-z0-9_-]+$"))] String,
);

impl CatalogSnapshotId {
    /// Parses a bounded opaque catalog snapshot identity.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogSnapshotIdError`] when the value is empty, exceeds 128
    /// bytes, or contains characters outside lowercase ASCII, digits, `_`, and
    /// `-`.
    pub fn parse(value: &str) -> Result<Self, CatalogSnapshotIdError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_CATALOG_SNAPSHOT_ID_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            });
        if !valid {
            return Err(CatalogSnapshotIdError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the opaque source-free snapshot text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for CatalogSnapshotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Invalid catalog snapshot identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid catalog snapshot identity")]
pub struct CatalogSnapshotIdError;

/// Strict response envelope for an immutable cross-repository catalog page.
///
/// Catalog pages deliberately have no single repository, generation, or
/// coverage identity. The catalog snapshot identifies the consistent listing
/// session while each entry carries only its own observed lifecycle metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogEnvelope<T> {
    /// Repository catalog response schema version.
    pub schema_version: RepoListSchemaVersion,
    /// Opaque source-free immutable catalog snapshot identity.
    pub snapshot_id: CatalogSnapshotId,
    /// Tool-specific catalog page.
    pub data: T,
    /// Whether a hard or requested page limit stopped completion.
    pub truncated: bool,
    /// Authenticated continuation bound to this snapshot, when another page exists.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting for this page.
    pub usage: UsageSummary,
    /// Source-free catalog warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
    /// Response-level classification for repository-derived catalog metadata.
    pub trust: TrustClassification,
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
    #[schemars(length(min = 1, max = 256))]
    pub alias: RequiredNullable<String>,
    /// Observed repository languages in deterministic canonical order.
    #[schemars(length(max = 64), inner(length(min = 1, max = 64)))]
    pub languages: Vec<String>,
    /// Structural freshness, null when no generation or observation exists.
    pub structural_freshness: RequiredNullable<Freshness>,
    /// Semantic freshness, null when no generation or observation exists.
    pub semantic_freshness: RequiredNullable<Freshness>,
    /// Aggregate observed coverage, null when coverage has not been measured.
    pub coverage: RequiredNullable<CoverageReport>,
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
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `repo.list`.
///
/// Successful catalog pages use the 2.0 list-specific envelope while expected
/// failures retain the common independently versioned 1.0 error taxonomy.
pub type RepoListOutput = ToolResponse<CatalogEnvelope<RepoListData>>;

#[cfg(test)]
mod tests {
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::CoverageStatus;
    use serde_json::json;

    use super::{
        CatalogEnvelope, CatalogSnapshotId, CoverageReport, LanguageCoverageReport, RepoListData,
        RepoListInput, RepoListOutput, RepoListSchemaVersion, RepoStatusInput, RepositoryEntry,
        RepositoryState,
    };
    use crate::{
        ErrorCode, TrustClassification,
        vertical::{
            CacheStatus, Freshness, RequiredNullable, ResponseWarning, SchemaVersion, ToolResponse,
            UsageSummary,
        },
    };

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

    fn repository_entry(
        marker: u8,
        display_name: &str,
        coverage: Option<CoverageReport>,
    ) -> RepositoryEntry {
        RepositoryEntry {
            repository_id: RepositoryId::from_bytes([marker; 16]),
            display_name: display_name.to_owned(),
            state: RepositoryState::Ready,
            active_generation: RequiredNullable(Some(GenerationId::from_bytes([marker; 20]))),
            generation_count: u64::from(marker),
            alias: RequiredNullable(Some(format!("{display_name}-alias"))),
            languages: vec!["rust".to_owned(), "typescript".to_owned()],
            structural_freshness: RequiredNullable(Some(Freshness::Current)),
            semantic_freshness: RequiredNullable(Some(Freshness::Superseded)),
            coverage: RequiredNullable(coverage),
        }
    }

    fn catalog_output(repositories: Vec<RepositoryEntry>) -> RepoListOutput {
        let total_count = u64::try_from(repositories.len()).expect("test catalog is bounded");
        ToolResponse::Success(CatalogEnvelope {
            schema_version: RepoListSchemaVersion::V2_0,
            snapshot_id: CatalogSnapshotId::parse("catalog1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .expect("test snapshot identity is valid"),
            data: RepoListData {
                repositories,
                total_count,
                explanation: None,
            },
            truncated: false,
            next_cursor: RequiredNullable(None),
            usage: UsageSummary {
                rows: 1,
                edges: 0,
                source_bytes: 0,
                json_bytes: 512,
                estimated_tokens: 64,
                wall_time_ms: 2,
                cache_status: CacheStatus::Miss,
                trace_id: "catalog-page".to_owned(),
            },
            warnings: Vec::<ResponseWarning>::new(),
            trust: TrustClassification::UntrustedRepositoryData,
        })
    }

    #[test]
    fn repo_list_success_uses_catalog_identity_without_repository_borrowing() {
        let encoded =
            serde_json::to_value(catalog_output(vec![repository_entry(1, "payments", None)]))
                .expect("catalog output serializes");

        assert_eq!(encoded["schema_version"], "2.0");
        assert_eq!(
            encoded["snapshot_id"],
            "catalog1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(encoded.get("repository").is_none());
        assert!(encoded.get("generation").is_none());
        assert!(encoded.get("coverage").is_none());
        assert!(encoded["data"]["repositories"][0]["coverage"].is_null());
        serde_json::from_value::<RepoListOutput>(encoded).expect("catalog output round trips");
    }

    #[test]
    fn repo_list_entry_preserves_observed_coverage_and_lifecycle_metadata() {
        let coverage = CoverageReport {
            status: CoverageStatus::Bounded,
            languages: vec![LanguageCoverageReport {
                language: "rust".to_owned(),
                tier: "C".to_owned(),
                files_indexed: 9,
                files_skipped: 1,
                missing_build_context: 0,
            }],
            total_files: 10,
            indexed_files: 9,
            skipped_files: 1,
        };
        let encoded = serde_json::to_value(catalog_output(vec![repository_entry(
            7,
            "payments",
            Some(coverage),
        )]))
        .expect("catalog output serializes");
        let entry = &encoded["data"]["repositories"][0];

        assert_eq!(entry["display_name"], "payments");
        assert_eq!(entry["alias"], "payments-alias");
        assert_eq!(entry["generation_count"], 7);
        assert_eq!(entry["languages"], json!(["rust", "typescript"]));
        assert_eq!(entry["structural_freshness"], "current");
        assert_eq!(entry["semantic_freshness"], "superseded");
        assert_eq!(entry["coverage"]["status"], "bounded");
    }

    #[test]
    fn repo_list_envelope_supports_empty_catalog_snapshots() {
        let encoded =
            serde_json::to_value(catalog_output(Vec::new())).expect("empty catalog serializes");

        assert_eq!(encoded["data"]["repositories"], json!([]));
        assert_eq!(encoded["data"]["total_count"], 0);
        assert_eq!(encoded["truncated"], false);
        assert!(encoded["next_cursor"].is_null());
        serde_json::from_value::<RepoListOutput>(encoded).expect("empty catalog round trips");
    }

    #[test]
    fn repo_list_envelope_supports_multiple_repository_entries() {
        let encoded = serde_json::to_value(catalog_output(vec![
            repository_entry(1, "payments", None),
            repository_entry(2, "search", None),
        ]))
        .expect("multi-entry catalog serializes");

        assert_eq!(encoded["data"]["total_count"], 2);
        assert_eq!(
            encoded["data"]["repositories"][0]["display_name"],
            "payments"
        );
        assert_eq!(encoded["data"]["repositories"][1]["display_name"], "search");
        assert!(encoded.get("repository").is_none());
        assert!(encoded.get("generation").is_none());
        serde_json::from_value::<RepoListOutput>(encoded).expect("multi-entry catalog round trips");
    }

    #[test]
    fn catalog_snapshot_identity_is_bounded_and_source_free() {
        let valid = CatalogSnapshotId::parse("catalog1_abc-123").expect("valid identity parses");
        assert_eq!(valid.as_str(), "catalog1_abc-123");

        for invalid in [
            "",
            "Catalog1_abc",
            "catalog/abc",
            r"catalog\abc",
            "catalog.abc",
        ] {
            assert!(
                CatalogSnapshotId::parse(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
        assert!(CatalogSnapshotId::parse(&"a".repeat(129)).is_err());
        assert!(serde_json::from_value::<CatalogSnapshotId>(json!("catalog/path")).is_err());
    }

    #[test]
    fn derived_repo_list_schema_enforces_the_two_zero_catalog_shape() {
        let schema =
            serde_json::to_value(schemars::schema_for!(RepoListOutput)).expect("schema serializes");
        let validator = jsonschema::draft202012::new(&schema).expect("derived schema compiles");
        let valid =
            serde_json::to_value(catalog_output(vec![repository_entry(1, "payments", None)]))
                .expect("catalog output serializes");
        assert!(validator.is_valid(&valid));

        let mut borrowed_repository = valid.clone();
        borrowed_repository
            .as_object_mut()
            .expect("success envelope is an object")
            .insert("repository".to_owned(), json!("repo1_forbidden"));
        assert!(!validator.is_valid(&borrowed_repository));

        let mut empty_display_name = valid;
        empty_display_name["data"]["repositories"][0]["display_name"] = json!("");
        assert!(!validator.is_valid(&empty_display_name));
    }

    #[test]
    fn repo_list_errors_keep_the_common_one_zero_taxonomy() {
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
        let decoded =
            serde_json::from_value::<RepoListOutput>(error).expect("common error decodes");
        let ToolResponse::Error(error) = decoded else {
            panic!("expected common error response");
        };
        assert_eq!(error.schema_version, SchemaVersion::V1_0);
        assert_eq!(error.error.code(), ErrorCode::NotFound);
    }
}
