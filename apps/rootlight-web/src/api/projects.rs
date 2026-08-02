//! Immutable repository catalog and generation-status browser projections.

use std::{str::FromStr as _, time::Duration};

use axum::{
    Json,
    extract::{Path, RawQuery, State},
};
use data_encoding::BASE64URL_NOPAD;
use rootlight_client::{
    GenerationId, GenerationSelector, OperationKind, OperationState,
    REPOSITORY_CATALOG_SORT_VERSION, RepositoryCatalogEntry, RepositoryCatalogFreshness,
    RepositoryCatalogPageRequest, RepositoryCatalogSnapshotId, RepositoryCatalogSortKey,
    RepositoryCatalogState, RepositoryCoverageEntry, RepositoryId, RepositoryStatus,
    RepositoryStatusCoverageDetail, RepositoryStatusFreshnessRequirement,
    RepositoryStatusOperation, RepositoryStatusRequest, RequestTimeout,
};
use serde::Serialize;

use crate::app::{ApiError, AppState};

const CATALOG_PAGE_SIZE: u16 = 50;
const MAX_WEB_CATALOG_PAGE_SIZE: u16 = 100;
const MAX_CATALOG_QUERY_BYTES: usize = 4 * 1024;
const MAX_DETAIL_QUERY_BYTES: usize = 2 * 1024;
const MAX_QUERY_PARAMETERS: usize = 16;
const MAX_CATALOG_STATES: usize = 8;
const SNAPSHOT_ENCODED_BYTES: usize = 43;
const MIN_SORT_KEY_ENCODED_BYTES: usize = 24;
const MAX_SORT_KEY_ENCODED_BYTES: usize = 1_390;
const PROJECT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectCatalogPage {
    schema: &'static str,
    projects: Vec<ProjectSummary>,
    snapshot: String,
    next_after: Option<String>,
    total_count: Option<String>,
    truncated: bool,
    sort_version: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectSummary {
    repository_id: String,
    active_generation_id: Option<String>,
    display_name: String,
    alias: Option<String>,
    generation_count: String,
    lifecycle_state: &'static str,
    languages: Vec<String>,
    structural_freshness: &'static str,
    semantic_freshness: &'static str,
    coverage: Vec<CoverageEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CoverageEntry {
    language: String,
    tier: String,
    status: String,
    discovered_files: String,
    indexed_files: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectDetail {
    schema: &'static str,
    repository_id: String,
    display_name: String,
    alias: Option<String>,
    resolved_generation_id: String,
    active_generation_id: String,
    parent_generation_id: Option<String>,
    active_parent_generation_id: Option<String>,
    active_structural_freshness: String,
    active_semantic_freshness: String,
    structural_freshness: String,
    semantic_freshness: String,
    lifecycle_state: String,
    publication_state: String,
    coverage: Vec<CoverageEntry>,
    operations: Vec<ProjectOperation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectOperation {
    operation_id: String,
    kind: &'static str,
    state: &'static str,
    completed_units: u32,
    total_units: u32,
    owned_by_client: bool,
    started_unix_ms: String,
}

pub(crate) async fn list(
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ProjectCatalogPage>, ApiError> {
    let query = CatalogQuery::parse(parse_query_pairs(
        raw_query.as_deref(),
        MAX_CATALOG_QUERY_BYTES,
    )?)?;
    let request = RepositoryCatalogPageRequest::new(
        query.page_size,
        query.query.as_deref(),
        (!query.states.is_empty()).then_some(query.states.as_slice()),
        query.snapshot,
        query.after,
    )
    .map_err(|_| ApiError::bad_request())?;
    let timeout = project_timeout()?;
    let page = state
        .daemon()
        .repository_catalog_page(&request, timeout)
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    Ok(Json(map_catalog_page(page)))
}

pub(crate) async fn detail(
    State(state): State<AppState>,
    Path(repository_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<ProjectDetail>, ApiError> {
    let repository = RepositoryId::from_str(&repository_id).map_err(|_| ApiError::bad_request())?;
    let query = DetailQuery::parse(parse_query_pairs(
        raw_query.as_deref(),
        MAX_DETAIL_QUERY_BYTES,
    )?)?;
    let request = RepositoryStatusRequest::new(repository, query.generation)
        .with_coverage_detail(query.coverage_detail)
        .with_operations(query.include_operations)
        .with_freshness_requirement(query.freshness);
    let timeout = project_timeout()?;
    let status = state
        .daemon()
        .repository_status(request, timeout)
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    Ok(Json(map_project_detail(status)))
}

struct CatalogQuery {
    page_size: u16,
    query: Option<String>,
    states: Vec<RepositoryCatalogState>,
    snapshot: Option<RepositoryCatalogSnapshotId>,
    after: Option<RepositoryCatalogSortKey>,
}

impl CatalogQuery {
    fn parse(parameters: Vec<(String, String)>) -> Result<Self, ApiError> {
        let mut page_size = None;
        let mut query = None;
        let mut states = Vec::new();
        let mut snapshot = None;
        let mut after = None;
        let mut sort_version = None;
        for (key, value) in parameters {
            match key.as_str() {
                "page_size" => set_once(
                    &mut page_size,
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|value| (1..=MAX_WEB_CATALOG_PAGE_SIZE).contains(value))
                        .ok_or_else(ApiError::bad_request)?,
                )?,
                "query" => set_once(&mut query, value)?,
                "state" => {
                    if states.len() == MAX_CATALOG_STATES {
                        return Err(ApiError::bad_request());
                    }
                    states.push(parse_catalog_state(&value)?);
                }
                "snapshot" => set_once(&mut snapshot, decode_snapshot(&value)?)?,
                "after" => set_once(&mut after, decode_sort_key(&value)?)?,
                "sort_version" => set_once(
                    &mut sort_version,
                    value.parse::<u32>().map_err(|_| ApiError::bad_request())?,
                )?,
                _ => return Err(ApiError::bad_request()),
            }
        }
        if sort_version.is_some_and(|value| value != REPOSITORY_CATALOG_SORT_VERSION) {
            return Err(ApiError::bad_request());
        }
        Ok(Self {
            page_size: page_size.unwrap_or(CATALOG_PAGE_SIZE),
            query,
            states,
            snapshot,
            after,
        })
    }
}

struct DetailQuery {
    generation: GenerationSelector,
    coverage_detail: RepositoryStatusCoverageDetail,
    include_operations: bool,
    freshness: RepositoryStatusFreshnessRequirement,
}

impl DetailQuery {
    fn parse(parameters: Vec<(String, String)>) -> Result<Self, ApiError> {
        let mut generation = None;
        let mut coverage_detail = None;
        let mut include_operations = None;
        let mut freshness = None;
        for (key, value) in parameters {
            match key.as_str() {
                "generation" => set_once(&mut generation, parse_generation(&value)?)?,
                "coverage_detail" => {
                    set_once(&mut coverage_detail, parse_coverage_detail(&value)?)?;
                }
                "include_operations" => {
                    set_once(&mut include_operations, parse_boolean(&value)?)?;
                }
                "require_freshness" => {
                    set_once(&mut freshness, parse_freshness_requirement(&value)?)?;
                }
                _ => return Err(ApiError::bad_request()),
            }
        }
        Ok(Self {
            generation: generation.unwrap_or(GenerationSelector::Active),
            coverage_detail: coverage_detail.unwrap_or(RepositoryStatusCoverageDetail::Language),
            include_operations: include_operations.unwrap_or(true),
            freshness: freshness.unwrap_or(RepositoryStatusFreshnessRequirement::None),
        })
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), ApiError> {
    if slot.replace(value).is_some() {
        return Err(ApiError::bad_request());
    }
    Ok(())
}

fn parse_query_pairs(
    raw_query: Option<&str>,
    maximum_bytes: usize,
) -> Result<Vec<(String, String)>, ApiError> {
    let Some(raw_query) = raw_query else {
        return Ok(Vec::new());
    };
    if raw_query.len() > maximum_bytes {
        return Err(ApiError::bad_request());
    }
    let mut parameters = Vec::with_capacity(MAX_QUERY_PARAMETERS);
    for (key, value) in form_urlencoded::parse(raw_query.as_bytes()) {
        if parameters.len() == MAX_QUERY_PARAMETERS {
            return Err(ApiError::bad_request());
        }
        parameters.push((key.into_owned(), value.into_owned()));
    }
    Ok(parameters)
}

fn parse_catalog_state(value: &str) -> Result<RepositoryCatalogState, ApiError> {
    match value {
        "ready" => Ok(RepositoryCatalogState::Ready),
        "indexing" => Ok(RepositoryCatalogState::Indexing),
        "degraded" => Ok(RepositoryCatalogState::Degraded),
        "corrupt" => Ok(RepositoryCatalogState::Corrupt),
        "migration_required" => Ok(RepositoryCatalogState::MigrationRequired),
        "rebuild_required" => Ok(RepositoryCatalogState::RebuildRequired),
        _ => Err(ApiError::bad_request()),
    }
}

fn parse_generation(value: &str) -> Result<GenerationSelector, ApiError> {
    if value == "active" {
        return Ok(GenerationSelector::Active);
    }
    GenerationId::from_str(value)
        .map(GenerationSelector::Generation)
        .map_err(|_| ApiError::bad_request())
}

fn parse_coverage_detail(value: &str) -> Result<RepositoryStatusCoverageDetail, ApiError> {
    match value {
        "summary" => Ok(RepositoryStatusCoverageDetail::Summary),
        "language" => Ok(RepositoryStatusCoverageDetail::Language),
        _ => Err(ApiError::bad_request()),
    }
}

fn parse_freshness_requirement(
    value: &str,
) -> Result<RepositoryStatusFreshnessRequirement, ApiError> {
    match value {
        "none" => Ok(RepositoryStatusFreshnessRequirement::None),
        "structural" => Ok(RepositoryStatusFreshnessRequirement::Structural),
        "semantic" => Ok(RepositoryStatusFreshnessRequirement::Semantic),
        _ => Err(ApiError::bad_request()),
    }
}

fn parse_boolean(value: &str) -> Result<bool, ApiError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ApiError::bad_request()),
    }
}

fn decode_snapshot(value: &str) -> Result<RepositoryCatalogSnapshotId, ApiError> {
    if value.len() != SNAPSHOT_ENCODED_BYTES {
        return Err(ApiError::bad_request());
    }
    let bytes = BASE64URL_NOPAD
        .decode(value.as_bytes())
        .map_err(|_| ApiError::bad_request())?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| ApiError::bad_request())?;
    Ok(RepositoryCatalogSnapshotId::from_bytes(bytes))
}

fn decode_sort_key(value: &str) -> Result<RepositoryCatalogSortKey, ApiError> {
    if !(MIN_SORT_KEY_ENCODED_BYTES..=MAX_SORT_KEY_ENCODED_BYTES).contains(&value.len()) {
        return Err(ApiError::bad_request());
    }
    let bytes = BASE64URL_NOPAD
        .decode(value.as_bytes())
        .map_err(|_| ApiError::bad_request())?;
    RepositoryCatalogSortKey::from_bytes(&bytes).map_err(|_| ApiError::bad_request())
}

fn project_timeout() -> Result<RequestTimeout, ApiError> {
    RequestTimeout::new(PROJECT_REQUEST_TIMEOUT).map_err(|_| ApiError::daemon_unavailable())
}

fn map_catalog_page(page: rootlight_client::RepositoryCatalogPage) -> ProjectCatalogPage {
    ProjectCatalogPage {
        schema: "rootlight.web-project-catalog-page/1",
        projects: page
            .repositories
            .into_iter()
            .map(map_project_summary)
            .collect(),
        snapshot: BASE64URL_NOPAD.encode(page.snapshot_id.as_bytes()),
        next_after: page
            .next_after
            .as_ref()
            .map(|after| BASE64URL_NOPAD.encode(after.as_bytes())),
        total_count: page.total_count.map(|value| value.to_string()),
        truncated: page.truncated,
        sort_version: page.sort_version,
    }
}

fn map_project_summary(project: RepositoryCatalogEntry) -> ProjectSummary {
    ProjectSummary {
        repository_id: project.repository_id.to_string(),
        active_generation_id: project
            .active_generation
            .map(|generation| generation.to_string()),
        display_name: project.display_name,
        alias: project.alias,
        generation_count: project.generation_count.to_string(),
        lifecycle_state: catalog_state_label(project.state),
        languages: project.languages,
        structural_freshness: catalog_freshness_label(project.structural_freshness),
        semantic_freshness: catalog_freshness_label(project.semantic_freshness),
        coverage: project.coverage.into_iter().map(map_coverage).collect(),
    }
}

fn map_project_detail(status: RepositoryStatus) -> ProjectDetail {
    ProjectDetail {
        schema: "rootlight.web-project-detail/1",
        repository_id: status.repository_id.to_string(),
        display_name: status.display_name,
        alias: status.alias,
        resolved_generation_id: status.resolved_generation.to_string(),
        active_generation_id: status.active_generation.to_string(),
        parent_generation_id: status.parent_generation.map(|value| value.to_string()),
        active_parent_generation_id: status
            .active_parent_generation
            .map(|value| value.to_string()),
        active_structural_freshness: status.active_structural_freshness,
        active_semantic_freshness: status.active_semantic_freshness,
        structural_freshness: status.structural_freshness,
        semantic_freshness: status.semantic_freshness,
        lifecycle_state: status.state,
        publication_state: status.publication_state,
        coverage: status.coverage.into_iter().map(map_coverage).collect(),
        operations: status.operations.into_iter().map(map_operation).collect(),
    }
}

fn map_coverage(coverage: RepositoryCoverageEntry) -> CoverageEntry {
    CoverageEntry {
        language: coverage.language,
        tier: coverage.tier,
        status: coverage.status,
        discovered_files: coverage.discovered_files.to_string(),
        indexed_files: coverage.indexed_files.to_string(),
    }
}

fn map_operation(operation: RepositoryStatusOperation) -> ProjectOperation {
    ProjectOperation {
        operation_id: operation.operation.to_string(),
        kind: operation_kind_label(operation.kind),
        state: operation_state_label(operation.state),
        completed_units: operation.completed_units,
        total_units: operation.total_units,
        owned_by_client: operation.owned_by_client,
        started_unix_ms: operation.started_unix_ms.to_string(),
    }
}

const fn catalog_state_label(value: RepositoryCatalogState) -> &'static str {
    match value {
        RepositoryCatalogState::Ready => "ready",
        RepositoryCatalogState::Indexing => "indexing",
        RepositoryCatalogState::Degraded => "degraded",
        RepositoryCatalogState::Corrupt => "corrupt",
        RepositoryCatalogState::MigrationRequired => "migration_required",
        RepositoryCatalogState::RebuildRequired => "rebuild_required",
    }
}

const fn catalog_freshness_label(value: RepositoryCatalogFreshness) -> &'static str {
    match value {
        RepositoryCatalogFreshness::Current => "current",
        RepositoryCatalogFreshness::Superseded => "superseded",
        RepositoryCatalogFreshness::Stale => "stale",
    }
}

const fn operation_kind_label(value: OperationKind) -> &'static str {
    match value {
        OperationKind::ControlProbe => "control_probe",
        OperationKind::RepositoryIndex => "repository_index",
    }
}

const fn operation_state_label(value: OperationState) -> &'static str {
    match value {
        OperationState::Queued => "queued",
        OperationState::Running => "running",
        OperationState::Cancelling => "cancelling",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::Interrupted => "interrupted",
        OperationState::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_client::RepositoryCatalogPage;

    #[test]
    fn catalog_query_is_closed_bounded_and_supports_repeated_states() {
        let snapshot = BASE64URL_NOPAD.encode(&[7; 32]);
        let parsed = CatalogQuery::parse(vec![
            ("page_size".to_owned(), "100".to_owned()),
            ("state".to_owned(), "ready".to_owned()),
            ("state".to_owned(), "degraded".to_owned()),
            ("snapshot".to_owned(), snapshot),
            (
                "sort_version".to_owned(),
                REPOSITORY_CATALOG_SORT_VERSION.to_string(),
            ),
        ])
        .expect("bounded query parses");
        assert_eq!(parsed.page_size, 100);
        assert_eq!(
            parsed.states,
            [
                RepositoryCatalogState::Ready,
                RepositoryCatalogState::Degraded
            ]
        );
        assert!(parsed.snapshot.is_some());

        assert!(CatalogQuery::parse(vec![("unknown".to_owned(), "value".to_owned())]).is_err());
        assert!(CatalogQuery::parse(vec![("page_size".to_owned(), "101".to_owned())]).is_err());
        assert!(
            CatalogQuery::parse(vec![
                ("page_size".to_owned(), "10".to_owned()),
                ("page_size".to_owned(), "20".to_owned()),
            ])
            .is_err()
        );
        assert!(
            CatalogQuery::parse(
                (0..=MAX_CATALOG_STATES)
                    .map(|_| ("state".to_owned(), "ready".to_owned()))
                    .collect()
            )
            .is_err()
        );
        assert!(
            parse_query_pairs(
                Some(&"x".repeat(MAX_CATALOG_QUERY_BYTES + 1)),
                MAX_CATALOG_QUERY_BYTES
            )
            .is_err()
        );
        assert!(
            parse_query_pairs(
                Some(
                    &(0..=MAX_QUERY_PARAMETERS)
                        .map(|index| format!("p{index}=v"))
                        .collect::<Vec<_>>()
                        .join("&")
                ),
                MAX_CATALOG_QUERY_BYTES
            )
            .is_err()
        );
        assert!(decode_snapshot(&"x".repeat(SNAPSHOT_ENCODED_BYTES + 1)).is_err());
        assert!(decode_sort_key(&"x".repeat(MAX_SORT_KEY_ENCODED_BYTES + 1)).is_err());
    }

    #[test]
    fn catalog_mapping_uses_canonical_strings_for_ids_and_large_counts() {
        let repository = RepositoryId::from_bytes([3; 16]);
        let generation = GenerationId::from_bytes([4; 20]);
        let mapped = map_catalog_page(RepositoryCatalogPage {
            repositories: vec![RepositoryCatalogEntry {
                repository_id: repository,
                display_name: "rootlight".to_owned(),
                alias: None,
                active_generation: Some(generation),
                generation_count: u64::MAX,
                state: RepositoryCatalogState::Ready,
                languages: vec!["rust".to_owned()],
                structural_freshness: RepositoryCatalogFreshness::Current,
                semantic_freshness: RepositoryCatalogFreshness::Stale,
                coverage: vec![RepositoryCoverageEntry {
                    language: "rust".to_owned(),
                    tier: "tier_b".to_owned(),
                    status: "bounded".to_owned(),
                    discovered_files: u64::MAX,
                    indexed_files: 1,
                }],
            }],
            snapshot_id: RepositoryCatalogSnapshotId::from_bytes([5; 32]),
            next_after: None,
            total_count: Some(u64::MAX),
            truncated: false,
            sort_version: REPOSITORY_CATALOG_SORT_VERSION,
        });
        let json = serde_json::to_value(mapped).expect("catalog projection serializes");
        assert_eq!(json["projects"][0]["repositoryId"], repository.to_string());
        assert_eq!(
            json["projects"][0]["activeGenerationId"],
            generation.to_string()
        );
        assert_eq!(json["projects"][0]["generationCount"], u64::MAX.to_string());
        assert_eq!(
            json["projects"][0]["coverage"][0]["discoveredFiles"],
            u64::MAX.to_string()
        );
        assert_eq!(json["totalCount"], u64::MAX.to_string());
    }

    #[test]
    fn detail_query_rejects_ambiguous_or_unknown_values() {
        assert!(
            DetailQuery::parse(vec![("generation".to_owned(), "not-an-id".to_owned())]).is_err()
        );
        assert!(
            DetailQuery::parse(vec![("include_operations".to_owned(), "yes".to_owned())]).is_err()
        );
        assert!(
            DetailQuery::parse(vec![
                ("generation".to_owned(), "active".to_owned()),
                ("generation".to_owned(), "active".to_owned()),
            ])
            .is_err()
        );
    }
}
