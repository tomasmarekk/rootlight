//! Authenticated browser API routing and verified SPA asset serving.

use std::{sync::Arc, time::Duration, time::Instant};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Extension, Path, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, COOKIE, SET_COOKIE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use rootlight_client::{
    ClientError, DaemonLifecycle, Health, HealthStatus, RequestTimeout, ResourcePressure,
};
use serde::Serialize;

use crate::{
    api,
    assets::{Asset, AssetInventory},
    daemon::DaemonClient,
    filesystem_registry::FilesystemRegistry,
    graph_registry::GraphRegistry,
    index_registry::IndexRegistry,
    security::{self, SecurityPolicy},
    session::{
        AuthenticatedSession, CSRF_HEADER_NAME, SESSION_COOKIE_NAME, SessionRegistry,
        idle_ttl_seconds,
    },
    source_registry::SourceCapabilityRegistry,
    support_registry::SupportRegistry,
};

const MAX_BROWSER_BODY_BYTES: usize = 16 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const GRAPH_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct AppState {
    assets: AssetInventory,
    daemon: Arc<dyn DaemonClient>,
    sessions: Arc<SessionRegistry>,
    filesystem: Arc<FilesystemRegistry>,
    indexes: Arc<IndexRegistry>,
    graphs: Arc<GraphRegistry>,
    sources: Arc<SourceCapabilityRegistry>,
    support: Arc<SupportRegistry>,
}

impl AppState {
    pub(crate) fn new(
        assets: AssetInventory,
        daemon: Arc<dyn DaemonClient>,
        sessions: Arc<SessionRegistry>,
        filesystem: Arc<FilesystemRegistry>,
        indexes: Arc<IndexRegistry>,
        graphs: Arc<GraphRegistry>,
        support: Arc<SupportRegistry>,
    ) -> Self {
        Self {
            assets,
            daemon,
            sessions,
            filesystem,
            indexes,
            graphs,
            sources: Arc::new(SourceCapabilityRegistry::new()),
            support,
        }
    }

    pub(crate) fn daemon(&self) -> &Arc<dyn DaemonClient> {
        &self.daemon
    }

    pub(crate) fn filesystem(&self) -> &Arc<FilesystemRegistry> {
        &self.filesystem
    }

    pub(crate) fn indexes(&self) -> &Arc<IndexRegistry> {
        &self.indexes
    }

    pub(crate) fn graphs(&self) -> &Arc<GraphRegistry> {
        &self.graphs
    }

    pub(crate) fn sources(&self) -> &Arc<SourceCapabilityRegistry> {
        &self.sources
    }

    pub(crate) fn support(&self) -> &Arc<SupportRegistry> {
        &self.support
    }

    fn reap_expired_session_resources(&self, now: Instant) {
        let expired = self.sessions.expire(now);
        self.filesystem.clear_sessions(&expired);
        self.filesystem.reap(now);
        self.indexes.clear_sessions(&expired);
        self.indexes.reap(now);
        self.graphs.clear_sessions(&expired);
        self.graphs.reap(now);
        self.sources.clear_sessions(&expired);
        self.sources.reap(now);
        self.support.clear_sessions(&expired);
        self.support.reap(now);
    }
}

pub(crate) fn router(state: AppState, policy: SecurityPolicy) -> Router {
    let filesystem_mutations = Router::new()
        .route(
            "/api/v1/filesystem/open-path",
            post(api::filesystem::open_path),
        )
        .route("/api/v1/filesystem/browse", post(api::filesystem::browse))
        .route(
            "/api/v1/filesystem/preflight-index",
            post(api::filesystem::preflight_index),
        )
        .route("/api/v1/projects/index", post(api::indexing::submit))
        .route(
            "/api/v1/operations/{operation_id}/cancel",
            post(api::indexing::cancel),
        )
        .route("/api/v1/graph/projections", post(api::graph::open))
        .route(
            "/api/v1/graph/projections/{projection_token}/next",
            post(api::graph::next),
        )
        .route(
            "/api/v1/graph/projections/{projection_token}",
            delete(api::graph::release),
        )
        .route("/api/v1/diagnostics/quick", post(api::diagnostics::quick))
        .route(
            "/api/v1/diagnostics/support-bundle",
            post(api::diagnostics::create_support_bundle),
        )
        .route(
            "/api/v1/projects/{repository_id}/relationships",
            post(api::evidence::relationships),
        )
        .route(
            "/api/v1/projects/{repository_id}/source",
            post(api::evidence::source),
        )
        .route(
            "/api/v1/projects/{repository_id}/change-impact",
            post(api::evidence::change_impact),
        )
        .route_layer(middleware::from_fn(require_mutation_csrf));
    let protected_api = Router::new()
        .route("/api/v1/session", delete(logout_session))
        .route("/api/v1/health", get(health))
        .route("/api/v1/filesystem/roots", get(api::filesystem::roots))
        .route(
            "/api/v1/diagnostics/support-bundles/{receipt}",
            get(api::diagnostics::download_support_bundle),
        )
        .route(
            "/api/v1/operations/{operation_id}",
            get(api::indexing::status),
        )
        .route("/api/v1/projects", get(api::projects::list))
        .route(
            "/api/v1/projects/{repository_id}",
            get(api::projects::detail),
        )
        .route(
            "/api/v1/projects/{repository_id}/nodes/{node_id}",
            get(api::evidence::node_detail),
        )
        .merge(filesystem_mutations)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/api/v1/session", get(session_status_or_create))
        .merge(protected_api)
        .route("/", get(index))
        .route("/{*path}", get(asset_or_route))
        .layer(DefaultBodyLimit::max(MAX_BROWSER_BODY_BYTES))
        .layer(middleware::from_fn(no_store_api))
        .layer(middleware::from_fn_with_state(
            policy.clone(),
            security::enforce,
        ))
        .with_state(state)
}

async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let now = Instant::now();
    state.reap_expired_session_resources(now);
    let session = authenticate_at(&state.sessions, request.headers(), now)?;
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}

async fn require_mutation_csrf(request: Request, next: Next) -> Result<Response, ApiError> {
    let session = request
        .extensions()
        .get::<AuthenticatedSession>()
        .ok_or_else(ApiError::unauthorized)?;
    require_csrf(session, request.headers())?;
    Ok(next.run(request).await)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResponse {
    csrf_token: String,
    idle_ttl_seconds: u64,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorEnvelope,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    code: &'static str,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
}

impl ApiError {
    const fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "session_required",
        }
    }

    const fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "csrf_required",
        }
    }

    const fn session_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "session_unavailable",
        }
    }

    pub(crate) const fn bad_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
        }
    }

    pub(crate) const fn invalid_filesystem_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_filesystem_request",
        }
    }

    pub(crate) const fn filesystem_capability_invalid() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "filesystem_capability_invalid",
        }
    }

    pub(crate) const fn filesystem_limit_reached() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "filesystem_limit_reached",
        }
    }

    pub(crate) const fn filesystem_unavailable() -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "filesystem_unavailable",
        }
    }

    pub(crate) const fn daemon_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "daemon_unavailable",
        }
    }

    pub(crate) const fn daemon_request_failed() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "daemon_request_failed",
        }
    }

    pub(crate) const fn invalid_index_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_index_request",
        }
    }

    pub(crate) const fn invalid_operation_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_operation_request",
        }
    }

    pub(crate) const fn index_request_conflict() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "index_request_conflict",
        }
    }

    pub(crate) const fn index_limit_reached() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "index_limit_reached",
        }
    }

    pub(crate) const fn operation_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "operation_not_found",
        }
    }

    pub(crate) const fn invalid_graph_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_graph_request",
        }
    }

    pub(crate) const fn graph_projection_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "graph_projection_not_found",
        }
    }

    pub(crate) const fn graph_projection_limit_reached() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "graph_projection_limit_reached",
        }
    }

    pub(crate) const fn graph_projection_conflict() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "graph_projection_conflict",
        }
    }

    pub(crate) const fn diagnostics_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "diagnostics_unavailable",
        }
    }

    pub(crate) const fn support_bundle_busy() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "support_bundle_busy",
        }
    }

    pub(crate) const fn support_bundle_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "support_bundle_not_found",
        }
    }

    pub(crate) const fn support_bundle_invalid() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "support_bundle_invalid",
        }
    }

    pub(crate) const fn invalid_node_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_node_request",
        }
    }

    pub(crate) const fn node_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "node_not_found",
        }
    }

    pub(crate) const fn invalid_relationships_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_relationships_request",
        }
    }

    pub(crate) const fn invalid_source_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_source_request",
        }
    }

    pub(crate) const fn source_capability_invalid() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "source_capability_invalid",
        }
    }

    pub(crate) const fn source_capability_limit_reached() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "source_capability_limit_reached",
        }
    }

    pub(crate) const fn invalid_change_impact_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_change_impact_request",
        }
    }

    pub(crate) const fn daemon_response_invalid() -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "daemon_response_invalid",
        }
    }

    pub(crate) fn from_daemon(error: &ClientError) -> Self {
        match error {
            ClientError::Ipc(_)
            | ClientError::RequestTimedOut
            | ClientError::DaemonUnavailable
            | ClientError::Runtime(_)
            | ClientError::DaemonExecutableMissing
            | ClientError::DaemonLaunchFailed => Self::daemon_unavailable(),
            ClientError::InvalidFirstSliceRequest
            | ClientError::InvalidRepositoryCatalogRequest
            | ClientError::InvalidSourceReference
            | ClientError::InvalidRequestTimeout => Self::bad_request(),
            ClientError::ProtocolFeatureUnavailable => Self {
                status: StatusCode::NOT_IMPLEMENTED,
                code: "capability_unavailable",
            },
            _ => Self::daemon_request_failed(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(ErrorBody {
                error: ErrorEnvelope { code: self.code },
            }),
        )
            .into_response()
    }
}

async fn session_status_or_create(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let now = Instant::now();
    state.reap_expired_session_resources(now);
    if let Some(session) =
        session_cookie(&headers).and_then(|cookie| state.sessions.authenticate(cookie, now))
    {
        return Ok(Json(SessionResponse {
            csrf_token: session.csrf_token(),
            idle_ttl_seconds: idle_ttl_seconds(),
        })
        .into_response());
    }
    let credentials = state
        .sessions
        .issue_session(now)
        .map_err(|_| ApiError::session_unavailable())?;
    let mut response = Json(SessionResponse {
        csrf_token: credentials.csrf_token,
        idle_ttl_seconds: credentials.idle_ttl_seconds,
    })
    .into_response();
    set_session_cookie(&mut response, &credentials.cookie_value)?;
    Ok(response)
}

async fn logout_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_csrf(&session, &headers)?;
    state.filesystem.clear_session(session.identity());
    state.indexes.clear_session(session.identity());
    state.sources.clear_session(session.identity());
    state.support.clear_session(session.identity());
    if let Ok(handles) = state.graphs.clear_session(session.identity())
        && let Ok(timeout) = RequestTimeout::try_from(GRAPH_RELEASE_TIMEOUT)
    {
        for handle in handles {
            let _ = state
                .daemon
                .graph_projection_release(handle.projection(), timeout)
                .await;
        }
    }
    state.sessions.logout(&session);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static(
            "rootlight_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    Ok(response)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    web_ready: bool,
    daemon_ready: bool,
    protocol_version: String,
    lifecycle: &'static str,
    accepting_operations: bool,
    active_operations: u32,
    admitted_operations: u32,
    queued_operations: u32,
    running_operations: u32,
    active_connections: u32,
    connection_limit: u32,
    operation_queue_limit: u32,
    journal_healthy: bool,
    catalog_schema_version: u32,
    endpoint_schema_version: u32,
    catalog_status: &'static str,
    generation_status: &'static str,
    adapter_status: &'static str,
    watcher_status: &'static str,
    endpoint_status: &'static str,
    resource_pressure: &'static str,
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    let timeout =
        RequestTimeout::new(HEALTH_TIMEOUT).map_err(|_| ApiError::daemon_unavailable())?;
    let health = state
        .daemon
        .health(timeout)
        .await
        .map_err(|_| ApiError::daemon_unavailable())?;
    Ok(Json(map_health(health)))
}

fn map_health(health: Health) -> HealthResponse {
    HealthResponse {
        web_ready: true,
        daemon_ready: health.ready,
        protocol_version: health.protocol_version,
        lifecycle: lifecycle_label(health.lifecycle),
        accepting_operations: health.accepting_operations,
        active_operations: health.active_operations,
        admitted_operations: health.admitted_operations,
        queued_operations: health.queued_operations,
        running_operations: health.running_operations,
        active_connections: health.active_connections,
        connection_limit: health.connection_limit,
        operation_queue_limit: health.operation_queue_limit,
        journal_healthy: health.journal_healthy,
        catalog_schema_version: health.catalog_schema_version,
        endpoint_schema_version: health.endpoint_schema_version,
        catalog_status: health_status_label(health.catalog_status),
        generation_status: health_status_label(health.generation_status),
        adapter_status: health_status_label(health.adapter_status),
        watcher_status: health_status_label(health.watcher_status),
        endpoint_status: health_status_label(health.endpoint_status),
        resource_pressure: resource_pressure_label(health.resource_pressure),
    }
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    session_asset_response(&state, state.assets.index(), &headers)
}

async fn asset_or_route(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(asset) = state.assets.get(&path) {
        return asset_response(asset);
    }
    if accepts_html(&headers)
        && !path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.contains('.'))
    {
        return session_asset_response(&state, state.assets.index(), &headers)
            .unwrap_or_else(IntoResponse::into_response);
    }
    StatusCode::NOT_FOUND.into_response()
}

fn session_asset_response(
    state: &AppState,
    asset: &Asset,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let now = Instant::now();
    state.reap_expired_session_resources(now);
    if session_cookie(headers)
        .is_some_and(|cookie| state.sessions.authenticate(cookie, now).is_some())
    {
        return Ok(asset_response(asset));
    }
    let credentials = state
        .sessions
        .issue_session(now)
        .map_err(|_| ApiError::session_unavailable())?;
    let mut response = asset_response(asset);
    set_session_cookie(&mut response, &credentials.cookie_value)?;
    Ok(response)
}

fn set_session_cookie(response: &mut Response, cookie_value: &str) -> Result<(), ApiError> {
    let cookie = format!("{SESSION_COOKIE_NAME}={cookie_value}; Path=/; HttpOnly; SameSite=Strict");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::session_unavailable())?,
    );
    Ok(())
}

fn asset_response(asset: &Asset) -> Response {
    let cache = if asset.immutable {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        HeaderValue::from_static("no-store")
    };
    let mut response = Response::new(Body::from(asset.bytes.clone()));
    response.headers_mut().insert(CACHE_CONTROL, cache);
    if let Ok(content_type) = HeaderValue::from_str(&asset.content_type) {
        response.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    response
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|media| media.trim().starts_with("text/html"))
        })
}

fn authenticate_at(
    sessions: &SessionRegistry,
    headers: &HeaderMap,
    now: Instant,
) -> Result<AuthenticatedSession, ApiError> {
    let cookie = session_cookie(headers).ok_or_else(ApiError::unauthorized)?;
    sessions
        .authenticate(cookie, now)
        .ok_or_else(ApiError::unauthorized)
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    let cookie = headers.get(COOKIE)?.to_str().ok()?;
    let mut matched = cookie.split(';').filter_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == SESSION_COOKIE_NAME).then_some(value)
    });
    let value = matched.next()?;
    if matched.next().is_some() || value.is_empty() {
        return None;
    }
    Some(value)
}

fn require_csrf(session: &AuthenticatedSession, headers: &HeaderMap) -> Result<(), ApiError> {
    headers
        .get(CSRF_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .filter(|value| session.validate_csrf(value))
        .map(|_| ())
        .ok_or_else(ApiError::forbidden)
}

async fn no_store_api(request: Request, next: Next) -> Response {
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if api {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

const fn lifecycle_label(value: DaemonLifecycle) -> &'static str {
    match value {
        DaemonLifecycle::Starting => "starting",
        DaemonLifecycle::Ready => "ready",
        DaemonLifecycle::Draining => "draining",
        DaemonLifecycle::Faulted => "faulted",
        DaemonLifecycle::Stopped => "stopped",
    }
}

const fn health_status_label(value: HealthStatus) -> &'static str {
    match value {
        HealthStatus::Healthy => "healthy",
        HealthStatus::Degraded => "degraded",
        HealthStatus::Unavailable => "unavailable",
        HealthStatus::NotConfigured => "not_configured",
        HealthStatus::Failed => "failed",
    }
}

const fn resource_pressure_label(value: ResourcePressure) -> &'static str {
    match value {
        ResourcePressure::Normal => "normal",
        ResourcePressure::Elevated => "elevated",
        ResourcePressure::High => "high",
        ResourcePressure::Critical => "critical",
        ResourcePressure::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, future::Future, pin::Pin};

    use axum::{
        body::to_bytes,
        http::{Method, Request, header::HOST},
    };
    use data_encoding::HEXLOWER;
    use rootlight_client::{
        ClientError, GenerationId, RepositoryCatalogEntry, RepositoryCatalogFreshness,
        RepositoryCatalogPage, RepositoryCatalogPageRequest, RepositoryCatalogSnapshotId,
        RepositoryCatalogState, RepositoryCoverageEntry, RepositoryId, RepositoryStatus,
        RepositoryStatusRequest,
    };
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use tower::ServiceExt as _;

    #[test]
    fn cookie_parser_rejects_duplicate_session_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static(
                "other=value; rootlight_session=first; rootlight_session=second",
            ),
        );
        assert!(session_cookie(&headers).is_none());
    }

    #[test]
    fn health_mapping_preserves_fail_closed_states() {
        let mapped = map_health(test_health());
        assert!(!mapped.daemon_ready);
        assert_eq!(mapped.lifecycle, "faulted");
        assert_eq!(mapped.catalog_status, "failed");
        assert_eq!(mapped.generation_status, "unavailable");
        assert_eq!(mapped.adapter_status, "degraded");
        assert_eq!(mapped.watcher_status, "not_configured");
        assert_eq!(mapped.resource_pressure, "unknown");
    }

    #[tokio::test]
    async fn router_enforces_direct_session_csrf_and_security_headers() {
        let asset_root = TempDir::new().expect("asset root exists");
        let index = b"<!doctype html><html class=\"dark\"></html>";
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
        let state = AppState::new(
            assets,
            Arc::new(FakeDaemon),
            Arc::clone(&sessions),
            Arc::new(FilesystemRegistry::new()),
            Arc::new(IndexRegistry::new()),
            Arc::new(GraphRegistry::new()),
            Arc::new(SupportRegistry::new()),
        );
        let app = router(state, SecurityPolicy::loopback(43_127));

        let direct_navigation = Request::builder()
            .uri("/")
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "none")
            .header(ACCEPT, "text/html")
            .body(Body::empty())
            .expect("direct navigation request builds");
        let direct_response = app
            .clone()
            .oneshot(direct_navigation)
            .await
            .expect("direct navigation response returns");
        assert_eq!(direct_response.status(), StatusCode::OK);
        let direct_cookie = direct_response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("direct navigation creates a browser session")
            .to_owned();
        let direct_session = Request::builder()
            .uri("/api/v1/session")
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &direct_cookie)
            .body(Body::empty())
            .expect("direct browser session request builds");
        let direct_session_response = app
            .clone()
            .oneshot(direct_session)
            .await
            .expect("direct browser session response returns");
        assert_eq!(direct_session_response.status(), StatusCode::OK);
        let response_body = to_bytes(direct_session_response.into_body(), MAX_BROWSER_BODY_BYTES)
            .await
            .expect("direct session response body reads");
        let response_json: serde_json::Value =
            serde_json::from_slice(&response_body).expect("direct session response parses");
        let csrf = response_json["csrfToken"]
            .as_str()
            .expect("csrf token returns")
            .to_owned();
        let cookie = direct_cookie;

        let unauthenticated_malformed_query = Request::builder()
            .uri(format!("/api/v1/projects?snapshot={}", "x".repeat(2_000)))
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .expect("unauthenticated query request builds");
        assert_eq!(
            app.clone()
                .oneshot(unauthenticated_malformed_query)
                .await
                .expect("unauthenticated query response returns")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let hostile = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/filesystem/open-path")
            .header(HOST, "localhost:43127")
            .header("origin", "http://localhost:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .header(COOKIE, &cookie)
            .header(CSRF_HEADER_NAME, &csrf)
            .body(Body::from("{}"))
            .expect("hostile request builds");
        assert_eq!(
            app.clone()
                .oneshot(hostile)
                .await
                .expect("hostile response returns")
                .status(),
            StatusCode::FORBIDDEN
        );

        let unauthenticated_filesystem_mutation = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/filesystem/open-path")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .expect("unauthenticated filesystem request builds");
        assert_eq!(
            app.clone()
                .oneshot(unauthenticated_filesystem_mutation)
                .await
                .expect("unauthenticated filesystem response returns")
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let filesystem_mutation_without_csrf = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/filesystem/open-path")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .header(COOKIE, &cookie)
            .body(Body::from("{"))
            .expect("filesystem request without CSRF builds");
        assert_eq!(
            app.clone()
                .oneshot(filesystem_mutation_without_csrf)
                .await
                .expect("filesystem CSRF rejection returns")
                .status(),
            StatusCode::FORBIDDEN
        );

        let health_request = Request::builder()
            .uri("/api/v1/health")
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("health request builds");
        let health_response = app
            .clone()
            .oneshot(health_request)
            .await
            .expect("health response returns");
        assert_eq!(health_response.status(), StatusCode::OK);
        assert_eq!(
            health_response.headers().get(CACHE_CONTROL),
            Some(&HeaderValue::from_static("no-store"))
        );
        assert!(
            health_response
                .headers()
                .contains_key("content-security-policy")
        );

        let catalog_request = Request::builder()
            .uri("/api/v1/projects?page_size=1&state=ready")
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("catalog request builds");
        let catalog_response = app
            .clone()
            .oneshot(catalog_request)
            .await
            .expect("catalog response returns");
        assert_eq!(catalog_response.status(), StatusCode::OK);
        let catalog_body = to_bytes(catalog_response.into_body(), MAX_BROWSER_BODY_BYTES)
            .await
            .expect("catalog response body reads");
        let catalog_json: serde_json::Value =
            serde_json::from_slice(&catalog_body).expect("catalog response parses");
        assert_eq!(
            catalog_json["projects"][0]["repositoryId"],
            test_repository().to_string()
        );
        assert_eq!(catalog_json["totalCount"], "1");

        let oversized_catalog_request = Request::builder()
            .uri(format!("/api/v1/projects?query={}", "x".repeat(4_097)))
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("oversized catalog request builds");
        assert_eq!(
            app.clone()
                .oneshot(oversized_catalog_request)
                .await
                .expect("oversized catalog response returns")
                .status(),
            StatusCode::BAD_REQUEST
        );

        let detail_request = Request::builder()
            .uri(format!(
                "/api/v1/projects/{}?generation=active&coverage_detail=language",
                test_repository()
            ))
            .header(HOST, "127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("project detail request builds");
        let detail_response = app
            .clone()
            .oneshot(detail_request)
            .await
            .expect("project detail response returns");
        assert_eq!(detail_response.status(), StatusCode::OK);

        let cross_origin_health = Request::builder()
            .uri("/api/v1/health")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://localhost:43127")
            .header("sec-fetch-site", "same-site")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("cross-origin health request builds");
        assert_eq!(
            app.clone()
                .oneshot(cross_origin_health)
                .await
                .expect("cross-origin rejection returns")
                .status(),
            StatusCode::FORBIDDEN
        );

        let logout_without_csrf = Request::builder()
            .method(Method::DELETE)
            .uri("/api/v1/session")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .body(Body::empty())
            .expect("logout request builds");
        assert_eq!(
            app.clone()
                .oneshot(logout_without_csrf)
                .await
                .expect("logout rejection returns")
                .status(),
            StatusCode::FORBIDDEN
        );

        let logout = Request::builder()
            .method(Method::DELETE)
            .uri("/api/v1/session")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(COOKIE, &cookie)
            .header(CSRF_HEADER_NAME, csrf)
            .body(Body::empty())
            .expect("logout request builds");
        assert_eq!(
            app.oneshot(logout)
                .await
                .expect("logout response returns")
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    struct FakeDaemon;

    impl DaemonClient for FakeDaemon {
        fn health<'a>(
            &'a self,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<Health, ClientError>> + Send + 'a>> {
            Box::pin(async { Ok(test_health()) })
        }

        fn repository_catalog_page<'a>(
            &'a self,
            _request: &'a RepositoryCatalogPageRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryCatalogPage, ClientError>> + Send + 'a>>
        {
            Box::pin(async {
                Ok(RepositoryCatalogPage {
                    repositories: vec![RepositoryCatalogEntry {
                        repository_id: test_repository(),
                        display_name: "Rootlight".to_owned(),
                        alias: None,
                        active_generation: Some(test_generation()),
                        generation_count: 1,
                        state: RepositoryCatalogState::Ready,
                        languages: vec!["rust".to_owned()],
                        structural_freshness: RepositoryCatalogFreshness::Current,
                        semantic_freshness: RepositoryCatalogFreshness::Current,
                        coverage: Vec::new(),
                    }],
                    snapshot_id: RepositoryCatalogSnapshotId::from_bytes([9; 32]),
                    next_after: None,
                    total_count: Some(1),
                    truncated: false,
                    sort_version: rootlight_client::REPOSITORY_CATALOG_SORT_VERSION,
                })
            })
        }

        fn repository_status<'a>(
            &'a self,
            _request: RepositoryStatusRequest,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<RepositoryStatus, ClientError>> + Send + 'a>>
        {
            Box::pin(async {
                Ok(RepositoryStatus {
                    repository_id: test_repository(),
                    display_name: "Rootlight".to_owned(),
                    alias: None,
                    resolved_generation: test_generation(),
                    active_generation: test_generation(),
                    parent_generation: None,
                    active_parent_generation: None,
                    active_structural_freshness: "current".to_owned(),
                    active_semantic_freshness: "current".to_owned(),
                    structural_freshness: "current".to_owned(),
                    semantic_freshness: "current".to_owned(),
                    state: "ready".to_owned(),
                    publication_state: "published".to_owned(),
                    coverage: vec![RepositoryCoverageEntry {
                        language: "rust".to_owned(),
                        tier: "tier_b".to_owned(),
                        status: "complete".to_owned(),
                        discovered_files: 1,
                        indexed_files: 1,
                    }],
                    operations: Vec::new(),
                })
            })
        }
    }

    fn test_repository() -> RepositoryId {
        RepositoryId::from_bytes([7; 16])
    }

    fn test_generation() -> GenerationId {
        GenerationId::from_bytes([8; 20])
    }

    fn test_health() -> Health {
        Health {
            ready: false,
            active_operations: 2,
            admitted_operations: 1,
            protocol_version: "1.9".to_owned(),
            lifecycle: DaemonLifecycle::Faulted,
            accepting_operations: false,
            active_connections: 1,
            connection_limit: 128,
            queued_operations: 1,
            running_operations: 1,
            operation_queue_limit: 256,
            journal_healthy: false,
            catalog_status: HealthStatus::Failed,
            catalog_schema_version: 2,
            generation_status: HealthStatus::Unavailable,
            adapter_status: HealthStatus::Degraded,
            watcher_status: HealthStatus::NotConfigured,
            resource_pressure: ResourcePressure::Unknown,
            endpoint_status: HealthStatus::Failed,
            endpoint_schema_version: 2,
        }
    }
}
