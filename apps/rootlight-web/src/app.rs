//! Authenticated browser API routing and verified SPA asset serving.

use std::{sync::Arc, time::Duration, time::Instant};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, COOKIE, SET_COOKIE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rootlight_client::{DaemonLifecycle, Health, HealthStatus, RequestTimeout, ResourcePressure};
use serde::{Deserialize, Serialize};

use crate::{
    assets::{Asset, AssetInventory},
    daemon::HealthClient,
    security::{self, SecurityPolicy},
    session::{
        AuthenticatedSession, CSRF_HEADER_NAME, SESSION_COOKIE_NAME, SessionRegistry,
        idle_ttl_seconds,
    },
};

const MAX_BROWSER_BODY_BYTES: usize = 16 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct AppState {
    assets: AssetInventory,
    daemon: Arc<dyn HealthClient>,
    sessions: Arc<SessionRegistry>,
}

impl AppState {
    pub(crate) fn new(
        assets: AssetInventory,
        daemon: Arc<dyn HealthClient>,
        sessions: Arc<SessionRegistry>,
    ) -> Self {
        Self {
            assets,
            daemon,
            sessions,
        }
    }
}

pub(crate) fn router(state: AppState, policy: SecurityPolicy) -> Router {
    Router::new()
        .route("/api/v1/session/bootstrap", post(bootstrap_session))
        .route(
            "/api/v1/session",
            get(session_status).delete(logout_session),
        )
        .route("/api/v1/health", get(health))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapRequest {
    secret: String,
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
struct ApiError {
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

    const fn daemon_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "daemon_unavailable",
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

async fn bootstrap_session(
    State(state): State<AppState>,
    Json(request): Json<BootstrapRequest>,
) -> Result<Response, ApiError> {
    let credentials = state
        .sessions
        .consume_bootstrap(&request.secret, Instant::now())
        .ok_or_else(ApiError::unauthorized)?;
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict",
        credentials.cookie_value
    );
    let mut response = Json(SessionResponse {
        csrf_token: credentials.csrf_token,
        idle_ttl_seconds: credentials.idle_ttl_seconds,
    })
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| ApiError::unauthorized())?,
    );
    Ok(response)
}

async fn session_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionResponse>, ApiError> {
    let session = authenticate(&state.sessions, &headers)?;
    Ok(Json(SessionResponse {
        csrf_token: session.csrf_token(),
        idle_ttl_seconds: idle_ttl_seconds(),
    }))
}

async fn logout_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let session = authenticate(&state.sessions, &headers)?;
    require_csrf(&session, &headers)?;
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
    queued_operations: u32,
    running_operations: u32,
    journal_healthy: bool,
    catalog_status: &'static str,
    generation_status: &'static str,
    adapter_status: &'static str,
    watcher_status: &'static str,
    endpoint_status: &'static str,
    resource_pressure: &'static str,
}

async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<HealthResponse>, ApiError> {
    authenticate(&state.sessions, &headers)?;
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
        queued_operations: health.queued_operations,
        running_operations: health.running_operations,
        journal_healthy: health.journal_healthy,
        catalog_status: health_status_label(health.catalog_status),
        generation_status: health_status_label(health.generation_status),
        adapter_status: health_status_label(health.adapter_status),
        watcher_status: health_status_label(health.watcher_status),
        endpoint_status: health_status_label(health.endpoint_status),
        resource_pressure: resource_pressure_label(health.resource_pressure),
    }
}

async fn index(State(state): State<AppState>) -> Response {
    asset_response(state.assets.index())
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
        return asset_response(state.assets.index());
    }
    StatusCode::NOT_FOUND.into_response()
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

fn authenticate(
    sessions: &SessionRegistry,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, ApiError> {
    let cookie = session_cookie(headers).ok_or_else(ApiError::unauthorized)?;
    sessions
        .authenticate(cookie, Instant::now())
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
    use rootlight_client::ClientError;
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
    async fn router_enforces_bootstrap_session_csrf_and_security_headers() {
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
        let bootstrap = sessions
            .issue_bootstrap(Instant::now())
            .expect("bootstrap issues")
            .encoded()
            .to_owned();
        let state = AppState::new(assets, Arc::new(FakeDaemon), Arc::clone(&sessions));
        let app = router(state, SecurityPolicy::loopback(43_127));
        let bootstrap_body =
            serde_json::to_vec(&json!({ "secret": bootstrap })).expect("bootstrap body serializes");

        let hostile = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/session/bootstrap")
            .header(HOST, "localhost:43127")
            .header("origin", "http://localhost:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(bootstrap_body.clone()))
            .expect("hostile request builds");
        assert_eq!(
            app.clone()
                .oneshot(hostile)
                .await
                .expect("hostile response returns")
                .status(),
            StatusCode::FORBIDDEN
        );

        let bootstrap_request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/session/bootstrap")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(bootstrap_body.clone()))
            .expect("bootstrap request builds");
        let bootstrap_response = app
            .clone()
            .oneshot(bootstrap_request)
            .await
            .expect("bootstrap response returns");
        assert_eq!(bootstrap_response.status(), StatusCode::OK);
        let cookie = bootstrap_response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("session cookie returns")
            .to_owned();
        let response_body = to_bytes(bootstrap_response.into_body(), MAX_BROWSER_BODY_BYTES)
            .await
            .expect("bootstrap response body reads");
        let response_json: serde_json::Value =
            serde_json::from_slice(&response_body).expect("bootstrap response parses");
        let csrf = response_json["csrfToken"]
            .as_str()
            .expect("csrf token returns")
            .to_owned();

        let replay = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/session/bootstrap")
            .header(HOST, "127.0.0.1:43127")
            .header("origin", "http://127.0.0.1:43127")
            .header("sec-fetch-site", "same-origin")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(bootstrap_body))
            .expect("replay request builds");
        assert_eq!(
            app.clone()
                .oneshot(replay)
                .await
                .expect("replay response returns")
                .status(),
            StatusCode::UNAUTHORIZED
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

    impl HealthClient for FakeDaemon {
        fn health<'a>(
            &'a self,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<Health, ClientError>> + Send + 'a>> {
            Box::pin(async { Ok(test_health()) })
        }
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
