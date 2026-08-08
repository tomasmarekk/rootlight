//! Detached repository indexing and session-owned operation status routes.

use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use rootlight_client::{
    OperationId, OperationStage, OperationState, RecoveryClass, RepositoryIndex,
    RepositoryIndexMode, RepositoryOperationAction, RepositoryOperationStatus, RequestTimeout,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    api::filesystem::IndexMode,
    app::{ApiError, AppState},
    filesystem_registry::FilesystemRegistryError,
    index_registry::{IndexRegistryError, IndexSubmission, NewIndexSubmission},
    session::AuthenticatedSession,
};

const INDEX_ADMISSION_TIMEOUT: Duration = Duration::from_secs(15);
const OPERATION_STATUS_TIMEOUT_OVERHEAD: Duration = Duration::from_secs(5);
const MAX_OPERATION_QUERY_BYTES: usize = 1_024;
const MAX_OPERATION_QUERY_PARAMETERS: usize = 2;
const MAX_OPERATION_WAIT_MS: u32 = 30_000;
const MIN_IDEMPOTENCY_KEY_BYTES: usize = 16;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct IndexRequest {
    root_capability: String,
    mode: IndexMode,
    #[serde(default = "default_detached")]
    detached: bool,
    client_request_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IndexResponse {
    schema: &'static str,
    display_label: String,
    repository_id: String,
    operation_id: String,
    semantic_operation_id: Option<String>,
    state: &'static str,
    revision: String,
    mode: &'static str,
    parent_generation_id: Option<String>,
    published_generation_id: Option<String>,
    discovered_inputs: String,
    indexed_files: String,
    entities: String,
    elapsed_micros: String,
    estimated_disk_bytes: String,
    diagnostics: Vec<IndexDiagnosticResponse>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexDiagnosticResponse {
    code: String,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationResponse {
    schema: &'static str,
    display_label: String,
    mode: &'static str,
    owned_by_session: bool,
    operation_id: String,
    state: &'static str,
    revision: String,
    completed_units: u32,
    total_units: u32,
    kind: &'static str,
    stage: &'static str,
    detached: bool,
    cancellation_requested: bool,
    recovery_class: &'static str,
    error: Option<OperationErrorResponse>,
    published_generation_id: Option<String>,
    semantic_operation_id: Option<String>,
    started_unix_ms: String,
    peak_rss_bytes: String,
    written_bytes: String,
    files_examined: String,
    bytes_examined: String,
    index_stage: String,
    retry_after_ms: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationErrorResponse {
    code: i32,
    message: String,
    retryable: bool,
    retry_after_ms: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CancelResponse {
    schema: &'static str,
    accepted: bool,
    operation: OperationResponse,
}

pub(crate) async fn submit(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<IndexRequest>,
) -> Result<Json<IndexResponse>, ApiError> {
    if !request.detached || !valid_idempotency_key(&request.client_request_id) {
        return Err(ApiError::invalid_index_request());
    }
    let owner = session.identity();
    let now = Instant::now();
    let idempotency_digest = idempotency_digest(&request.client_request_id);
    let fingerprint = request_fingerprint(&request.root_capability, request.mode);
    let submission = match state
        .indexes()
        .find(owner, &idempotency_digest, &fingerprint, now)
        .map_err(map_index_registry_error)?
    {
        Some(submission) => submission,
        None => {
            let admission = state
                .filesystem()
                .bind_root(owner, &request.root_capability, idempotency_digest, now)
                .map_err(map_filesystem_error)?;
            state
                .indexes()
                .insert_or_find(NewIndexSubmission {
                    owner,
                    idempotency_digest,
                    request_fingerprint: fingerprint,
                    operation: random_operation_id()?,
                    mode: client_mode(request.mode),
                    admission,
                    now,
                })
                .map_err(map_index_registry_error)?
        }
    };
    let _guard = submission.gate().lock().await;
    if let Some(result) = submission.result().map_err(map_index_registry_error)? {
        return Ok(Json(map_index_response(&submission, result)));
    }
    let root = submission
        .admission()
        .local_path()
        .to_str()
        .ok_or_else(ApiError::filesystem_unavailable)?;
    let timeout =
        RequestTimeout::new(INDEX_ADMISSION_TIMEOUT).map_err(|_| ApiError::daemon_unavailable())?;
    let result = state
        .daemon()
        .repository_index(root, submission.operation(), submission.mode(), timeout)
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    submission
        .record_result(result.clone())
        .map_err(map_index_registry_error)?;
    Ok(Json(map_index_response(&submission, result)))
}

pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(operation): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<OperationResponse>, ApiError> {
    let operation = parse_operation_id(&operation)?;
    let submission = state
        .indexes()
        .find_operation(session.identity(), operation, Instant::now())
        .map_err(map_index_registry_error)?;
    let query = parse_operation_query(raw_query.as_deref())?;
    let timeout_duration = Duration::from_millis(u64::from(query.wait_ms.unwrap_or(0)))
        .checked_add(OPERATION_STATUS_TIMEOUT_OVERHEAD)
        .ok_or_else(ApiError::invalid_operation_request)?;
    let timeout =
        RequestTimeout::new(timeout_duration).map_err(|_| ApiError::invalid_operation_request())?;
    let status = state
        .daemon()
        .repository_operation_status(
            operation,
            RepositoryOperationAction::Get,
            query.wait_ms,
            query.after_revision,
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    Ok(Json(map_operation_response(&submission, status)))
}

pub(crate) async fn cancel(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(operation): Path<String>,
) -> Result<Json<CancelResponse>, ApiError> {
    let operation = parse_operation_id(&operation)?;
    let submission = state
        .indexes()
        .find_operation(session.identity(), operation, Instant::now())
        .map_err(map_index_registry_error)?;
    let timeout = RequestTimeout::new(OPERATION_STATUS_TIMEOUT_OVERHEAD)
        .map_err(|_| ApiError::daemon_unavailable())?;
    let status = state
        .daemon()
        .repository_operation_status(
            operation,
            RepositoryOperationAction::Cancel,
            None,
            None,
            timeout,
        )
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;
    let accepted = status.operation.cancellation_requested;
    Ok(Json(CancelResponse {
        schema: "rootlight.web-operation-cancel/1",
        accepted,
        operation: map_operation_response(&submission, status),
    }))
}

#[derive(Default)]
struct OperationQuery {
    wait_ms: Option<u32>,
    after_revision: Option<u64>,
}

fn parse_operation_query(raw: Option<&str>) -> Result<OperationQuery, ApiError> {
    let Some(raw) = raw else {
        return Ok(OperationQuery::default());
    };
    if raw.len() > MAX_OPERATION_QUERY_BYTES {
        return Err(ApiError::invalid_operation_request());
    }
    let mut query = OperationQuery::default();
    let mut count = 0usize;
    for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
        count = count.saturating_add(1);
        if count > MAX_OPERATION_QUERY_PARAMETERS {
            return Err(ApiError::invalid_operation_request());
        }
        match key.as_ref() {
            "wait_ms" if query.wait_ms.is_none() => {
                let wait_ms = value
                    .parse::<u32>()
                    .ok()
                    .filter(|wait| *wait <= MAX_OPERATION_WAIT_MS)
                    .ok_or_else(ApiError::invalid_operation_request)?;
                query.wait_ms = Some(wait_ms);
            }
            "after_revision" if query.after_revision.is_none() => {
                query.after_revision = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| ApiError::invalid_operation_request())?,
                );
            }
            _ => return Err(ApiError::invalid_operation_request()),
        }
    }
    if query.after_revision.is_some() && query.wait_ms.is_none() {
        return Err(ApiError::invalid_operation_request());
    }
    Ok(query)
}

fn map_index_response(submission: &IndexSubmission, index: RepositoryIndex) -> IndexResponse {
    IndexResponse {
        schema: "rootlight.web-project-index/1",
        display_label: submission.admission().display_label().to_owned(),
        repository_id: index.repository.to_string(),
        operation_id: index.operation.to_string(),
        semantic_operation_id: index
            .semantic_operation
            .map(|operation| operation.to_string()),
        state: operation_state_label(index.state),
        revision: index.revision.to_string(),
        mode: index_mode_label(index.mode),
        parent_generation_id: index
            .parent_generation
            .map(|generation| generation.to_string()),
        published_generation_id: index
            .published_generation
            .map(|generation| generation.to_string()),
        discovered_inputs: index.discovered_inputs.to_string(),
        indexed_files: index.indexed_files.to_string(),
        entities: index.entities.to_string(),
        elapsed_micros: index.elapsed_micros.to_string(),
        estimated_disk_bytes: index.estimated_disk_bytes.to_string(),
        diagnostics: index
            .diagnostics
            .into_iter()
            .map(|diagnostic| IndexDiagnosticResponse {
                code: diagnostic.code,
                message: diagnostic.message,
            })
            .collect(),
    }
}

fn map_operation_response(
    submission: &IndexSubmission,
    status: RepositoryOperationStatus,
) -> OperationResponse {
    let operation = status.operation;
    let error = operation
        .error
        .as_ref()
        .map(|error| OperationErrorResponse {
            code: error.code().wire_number(),
            message: error.message().to_owned(),
            retryable: error.retryable(),
            retry_after_ms: error.retry_after_ms().map(|delay| delay.to_string()),
        });
    OperationResponse {
        schema: "rootlight.web-repository-operation/1",
        display_label: submission.admission().display_label().to_owned(),
        mode: index_mode_label(submission.mode()),
        owned_by_session: true,
        operation_id: operation.operation.to_string(),
        state: operation_state_label(operation.state),
        revision: operation.revision.to_string(),
        completed_units: operation.completed_units,
        total_units: operation.total_units,
        kind: "repository_index",
        stage: operation_stage_label(operation.stage),
        detached: operation.detached,
        cancellation_requested: operation.cancellation_requested,
        recovery_class: recovery_class_label(operation.recovery_class),
        error,
        published_generation_id: status
            .published_generation
            .map(|generation| generation.to_string()),
        semantic_operation_id: status
            .semantic_operation
            .map(|semantic| semantic.to_string()),
        started_unix_ms: status.started_unix_ms.to_string(),
        peak_rss_bytes: status.peak_rss_bytes.to_string(),
        written_bytes: status.written_bytes.to_string(),
        files_examined: status.files_examined.to_string(),
        bytes_examined: status.bytes_examined.to_string(),
        index_stage: status.index_stage,
        retry_after_ms: status.retry_after_ms,
    }
}

fn parse_operation_id(value: &str) -> Result<OperationId, ApiError> {
    if value.len() > 64 {
        return Err(ApiError::invalid_operation_request());
    }
    value
        .parse::<OperationId>()
        .map_err(|_| ApiError::invalid_operation_request())
}

fn valid_idempotency_key(value: &str) -> bool {
    (MIN_IDEMPOTENCY_KEY_BYTES..=MAX_IDEMPOTENCY_KEY_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn idempotency_digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"rootlight.web-index-idempotency/1");
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

fn request_fingerprint(root_capability: &str, mode: IndexMode) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"rootlight.web-index-request/1");
    hasher.update(root_capability.as_bytes());
    hasher.update([match mode {
        IndexMode::Auto => 1,
        IndexMode::Structural => 2,
        IndexMode::Deep => 3,
    }]);
    hasher.finalize().into()
}

fn random_operation_id() -> Result<OperationId, ApiError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::daemon_unavailable())?;
    Ok(OperationId::from_bytes(bytes))
}

const fn client_mode(mode: IndexMode) -> RepositoryIndexMode {
    match mode {
        IndexMode::Auto => RepositoryIndexMode::Auto,
        IndexMode::Structural => RepositoryIndexMode::Structural,
        IndexMode::Deep => RepositoryIndexMode::Deep,
    }
}

const fn default_detached() -> bool {
    true
}

const fn index_mode_label(mode: RepositoryIndexMode) -> &'static str {
    match mode {
        RepositoryIndexMode::Auto => "auto",
        RepositoryIndexMode::Structural => "structural",
        RepositoryIndexMode::Deep => "deep",
    }
}

const fn operation_state_label(state: OperationState) -> &'static str {
    match state {
        OperationState::Queued => "queued",
        OperationState::Running => "running",
        OperationState::Cancelling => "cancelling",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::Interrupted => "interrupted",
        OperationState::Cancelled => "cancelled",
    }
}

const fn operation_stage_label(stage: OperationStage) -> &'static str {
    match stage {
        OperationStage::Accepted => "accepted",
        OperationStage::Executing => "executing",
        OperationStage::Cleanup => "cleanup",
    }
}

const fn recovery_class_label(classification: RecoveryClass) -> &'static str {
    match classification {
        RecoveryClass::NotApplicable => "not_applicable",
        RecoveryClass::InterruptedByRestart => "interrupted_by_restart",
        RecoveryClass::DeadlineElapsed => "deadline_elapsed",
        RecoveryClass::LeaseExpired => "lease_expired",
    }
}

fn map_index_registry_error(error: IndexRegistryError) -> ApiError {
    match error {
        IndexRegistryError::Conflict => ApiError::index_request_conflict(),
        IndexRegistryError::LimitReached => ApiError::index_limit_reached(),
        IndexRegistryError::NotFound => ApiError::operation_not_found(),
        IndexRegistryError::Unavailable => ApiError::daemon_unavailable(),
    }
}

fn map_filesystem_error(error: FilesystemRegistryError) -> ApiError {
    match error {
        FilesystemRegistryError::InvalidRequest => ApiError::invalid_index_request(),
        FilesystemRegistryError::CapabilityInvalid => ApiError::filesystem_capability_invalid(),
        FilesystemRegistryError::LimitReached => ApiError::filesystem_limit_reached(),
        FilesystemRegistryError::ResourceUnavailable => ApiError::filesystem_unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{
            Method, Request, StatusCode,
            header::{CONTENT_TYPE, COOKIE, HOST, ORIGIN},
        },
    };
    use data_encoding::HEXLOWER;
    use rootlight_cancel::Cancellation;
    use rootlight_client::{
        ClientError, Health, RepositoryCatalogPage, RepositoryCatalogPageRequest, RepositoryStatus,
        RepositoryStatusRequest,
    };
    use rootlight_vfs::BrowseDirectory;
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{
        app,
        assets::AssetInventory,
        daemon::DaemonClient,
        filesystem_registry::{FilesystemRegistry, RootAdmission},
        graph_registry::GraphRegistry,
        index_registry::IndexRegistry,
        security::SecurityPolicy,
        session::{CSRF_HEADER_NAME, SESSION_COOKIE_NAME, SessionRegistry},
        support_registry::SupportRegistry,
    };

    const TEST_PORT: u16 = 43_141;
    const TEST_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

    #[test]
    fn operation_query_and_idempotency_inputs_are_closed_and_bounded() {
        let query = parse_operation_query(Some("wait_ms=30000&after_revision=9"))
            .expect("bounded query parses");
        assert_eq!(query.wait_ms, Some(30_000));
        assert_eq!(query.after_revision, Some(9));
        assert!(parse_operation_query(Some("after_revision=9")).is_err());
        assert!(parse_operation_query(Some("wait_ms=30001")).is_err());
        assert!(parse_operation_query(Some("wait_ms=1&wait_ms=2")).is_err());
        assert!(parse_operation_query(Some("unknown=1")).is_err());
        assert!(valid_idempotency_key(&"a".repeat(16)));
        assert!(valid_idempotency_key(&"a".repeat(128)));
        assert!(!valid_idempotency_key(&"a".repeat(15)));
        assert!(!valid_idempotency_key("invalid/key/request"));
    }

    #[test]
    fn request_fingerprints_bind_root_and_mode() {
        assert_eq!(
            idempotency_digest("same-request-id"),
            idempotency_digest("same-request-id")
        );
        assert_ne!(
            request_fingerprint("a", IndexMode::Auto),
            request_fingerprint("b", IndexMode::Auto)
        );
        assert_ne!(
            request_fingerprint("a", IndexMode::Auto),
            request_fingerprint("a", IndexMode::Deep)
        );
    }

    #[tokio::test]
    async fn submit_retries_reuse_operation_and_status_cancel_remain_session_owned() {
        let fixture = TestApp::new();
        let request_id = "r".repeat(43);
        let request = json!({
            "rootCapability": fixture.root_capability,
            "mode": "auto",
            "detached": true,
            "clientRequestId": request_id,
        });

        let (status, first) = fixture
            .request(
                Method::POST,
                "/api/v1/projects/index",
                Some(request.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["schema"], "rootlight.web-project-index/1");
        assert_eq!(first["displayLabel"], "selected root");
        assert_eq!(first["state"], "queued");
        assert_eq!(first["mode"], "auto");
        let operation = first["operationId"]
            .as_str()
            .expect("operation response is a string")
            .parse::<OperationId>()
            .expect("operation response is canonical");
        assert_eq!(fixture.daemon.index_calls.load(Ordering::SeqCst), 1);

        let (status, retry) = fixture
            .request(
                Method::POST,
                "/api/v1/projects/index",
                Some(request.clone()),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(retry["operationId"], first["operationId"]);
        assert_eq!(fixture.daemon.index_calls.load(Ordering::SeqCst), 1);

        let conflicting = json!({
            "rootCapability": fixture.root_capability,
            "mode": "deep",
            "detached": true,
            "clientRequestId": request_id,
        });
        let (status, _) = fixture
            .request(Method::POST, "/api/v1/projects/index", Some(conflicting))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        let operation_uri = format!(
            "/api/v1/operations/{}?wait_ms=1&after_revision=0",
            operation
        );
        let (status, operation_status) = fixture.request(Method::GET, &operation_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(operation_status["state"], "running");
        assert_eq!(operation_status["indexStage"], "indexing");
        assert_eq!(operation_status["completedUnits"], 2);
        assert_eq!(operation_status["totalUnits"], 4);

        let cancel_uri = format!("/api/v1/operations/{operation}/cancel");
        let (status, cancelled) = fixture.request(Method::POST, &cancel_uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["accepted"], true);
        assert_eq!(cancelled["operation"]["state"], "cancelling");
        assert_eq!(fixture.daemon.status_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn index_submission_requires_detached_mode_and_exact_root_key_binding() {
        let fixture = TestApp::new();
        let (status, _) = fixture
            .request(
                Method::POST,
                "/api/v1/projects/index",
                Some(json!({
                    "rootCapability": fixture.root_capability,
                    "mode": "structural",
                    "detached": false,
                    "clientRequestId": "a".repeat(43),
                })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = fixture
            .request(
                Method::POST,
                "/api/v1/projects/index",
                Some(json!({
                    "rootCapability": fixture.root_capability,
                    "mode": "structural",
                    "detached": true,
                    "clientRequestId": "a".repeat(43),
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = fixture
            .request(
                Method::POST,
                "/api/v1/projects/index",
                Some(json!({
                    "rootCapability": fixture.root_capability,
                    "mode": "structural",
                    "detached": true,
                    "clientRequestId": "b".repeat(43),
                })),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    struct TestApp {
        router: Router,
        cookie: String,
        csrf: String,
        root_capability: String,
        daemon: Arc<IndexDaemon>,
        _root: TempDir,
    }

    impl TestApp {
        fn new() -> Self {
            let asset_root = crate::test_support::local_tempdir();
            let index = b"<!doctype html><html></html>";
            fs::write(asset_root.path().join("index.html"), index).expect("index writes");
            let manifest = serde_json::to_vec(&json!({
                "schema_version": 1,
                "assets": [{
                    "path": "index.html",
                    "bytes": index.len(),
                    "sha256": HEXLOWER.encode(Sha256::digest(index).as_ref())
                }]
            }))
            .expect("manifest serializes");
            fs::write(asset_root.path().join("asset-manifest.json"), manifest)
                .expect("manifest writes");
            let assets = AssetInventory::load(asset_root.path()).expect("assets validate");
            let sessions = Arc::new(SessionRegistry::new());
            let now = Instant::now();
            let credentials = sessions.issue_session(now).expect("browser session issues");
            let session = sessions
                .authenticate(&credentials.cookie_value, now)
                .expect("session authenticates");
            let cookie = format!("{SESSION_COOKIE_NAME}={}", credentials.cookie_value);
            let filesystem = Arc::new(FilesystemRegistry::new());
            let root = crate::test_support::local_tempdir();
            let directory = BrowseDirectory::open(root.path(), &Cancellation::new())
                .expect("repository root opens");
            let root_capability = filesystem
                .issue_root(
                    session.identity(),
                    RootAdmission::new(directory, "selected root".to_owned()),
                    now,
                )
                .expect("root capability issues")
                .token;
            let daemon = Arc::new(IndexDaemon::new());
            let state = app::AppState::new(
                assets,
                Arc::clone(&daemon) as Arc<dyn DaemonClient>,
                sessions,
                filesystem,
                Arc::new(IndexRegistry::new()),
                Arc::new(GraphRegistry::new()),
                Arc::new(SupportRegistry::new()),
            );
            Self {
                router: app::router(state, SecurityPolicy::loopback(TEST_PORT)),
                cookie,
                csrf: credentials.csrf_token,
                root_capability,
                daemon,
                _root: root,
            }
        }

        async fn request(
            &self,
            method: Method,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut request = Request::builder()
                .method(method.clone())
                .uri(uri)
                .header(HOST, format!("127.0.0.1:{TEST_PORT}"))
                .header("sec-fetch-site", "same-origin")
                .header(COOKIE, &self.cookie);
            if method == Method::POST {
                request = request
                    .header(CONTENT_TYPE, "application/json")
                    .header(ORIGIN, format!("http://127.0.0.1:{TEST_PORT}"))
                    .header(CSRF_HEADER_NAME, &self.csrf);
            }
            let encoded = body
                .map(|value| serde_json::to_vec(&value).expect("request serializes"))
                .unwrap_or_default();
            let response = self
                .router
                .clone()
                .oneshot(request.body(Body::from(encoded)).expect("request builds"))
                .await
                .expect("response returns");
            let status = response.status();
            let bytes = to_bytes(response.into_body(), TEST_RESPONSE_LIMIT)
                .await
                .expect("response body reads");
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, value)
        }
    }

    struct IndexDaemon {
        index_calls: AtomicUsize,
        status_calls: AtomicUsize,
    }

    impl IndexDaemon {
        const fn new() -> Self {
            Self {
                index_calls: AtomicUsize::new(0),
                status_calls: AtomicUsize::new(0),
            }
        }
    }

    impl DaemonClient for IndexDaemon {
        fn health<'a>(
            &'a self,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<Health, ClientError>> + Send + 'a>> {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_catalog_page<'a>(
            &'a self,
            _request: &'a RepositoryCatalogPageRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryCatalogPage, ClientError>> + Send + 'a>>
        {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_status<'a>(
            &'a self,
            _request: RepositoryStatusRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryStatus, ClientError>> + Send + 'a>>
        {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_index<'a>(
            &'a self,
            _root: &'a str,
            operation: OperationId,
            mode: RepositoryIndexMode,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryIndex, ClientError>> + Send + 'a>>
        {
            self.index_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(RepositoryIndex {
                    repository: rootlight_client::RepositoryId::from_bytes([7; 16]),
                    operation,
                    semantic_operation: None,
                    state: OperationState::Queued,
                    revision: 1,
                    mode,
                    parent_generation: None,
                    published_generation: None,
                    discovered_inputs: 0,
                    indexed_files: 0,
                    entities: 0,
                    elapsed_micros: 0,
                    estimated_disk_bytes: 0,
                    diagnostics: Vec::new(),
                })
            })
        }

        fn repository_operation_status<'a>(
            &'a self,
            operation: OperationId,
            action: RepositoryOperationAction,
            _wait_ms: Option<u32>,
            _after_revision: Option<u64>,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryOperationStatus, ClientError>> + Send + 'a>>
        {
            self.status_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let cancelled = action == RepositoryOperationAction::Cancel;
                Ok(RepositoryOperationStatus {
                    operation: rootlight_client::OperationStatus {
                        operation,
                        state: if cancelled {
                            OperationState::Cancelling
                        } else {
                            OperationState::Running
                        },
                        revision: if cancelled { 3 } else { 2 },
                        completed_units: 2,
                        total_units: 4,
                        error: None,
                        kind: rootlight_client::OperationKind::RepositoryIndex,
                        stage: OperationStage::Executing,
                        plan_hash: [8; 32],
                        detached: true,
                        cancellation_requested: cancelled,
                        deadline_unix_ms: None,
                        lease_expires_unix_ms: None,
                        recovery_class: RecoveryClass::NotApplicable,
                    },
                    published_generation: None,
                    semantic_operation: None,
                    started_unix_ms: 1,
                    peak_rss_bytes: 2,
                    written_bytes: 3,
                    files_examined: 4,
                    bytes_examined: 5,
                    index_stage: "indexing".to_owned(),
                    retry_after_ms: Some(100),
                    evidence: None,
                })
            })
        }
    }
}
