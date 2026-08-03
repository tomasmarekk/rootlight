//! Browser-safe filesystem discovery, navigation, and index preflight routes.
//!
//! Every navigation step is authorized by a session-owned VFS handle; paths
//! and filesystem error sources are deliberately absent from wire responses.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Extension, State},
};
use rootlight_cancel::Cancellation;
use rootlight_client::{HealthStatus, RequestTimeout};
use rootlight_vfs::{
    BrowseDirectory, BrowseDirectoryEntry, BrowseDirectorySnapshot, BrowsePageOffset,
    BrowsePageSize, MAX_BROWSE_CHILD_NAME_BYTES, MAX_BROWSE_PAGE_SIZE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    app::{ApiError, AppState},
    filesystem_registry::{
        BrowseNode, FilesystemRegistryError, IssuedBrowseCapability, MAX_BROWSE_DEPTH,
        ROOT_CAPABILITY_TTL_SECONDS, RootAdmission,
    },
    session::AuthenticatedSession,
};

const FILESYSTEM_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const PREFLIGHT_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DIRECT_PATH_BYTES: usize = 8 * 1_024;
const MAX_FILTER_BYTES: usize = 256;
const MAX_BROWSER_LABEL_UTF16_UNITS: usize = 256;
const MAX_BROWSER_DIRECTORY_NAME_UTF16_UNITS: usize = 1_024;
const MAX_PLATFORM_ROOTS: usize = 32;
const DEFAULT_PAGE_SIZE: usize = 64;
const PAGE_SCAN_SIZE: usize = MAX_BROWSE_PAGE_SIZE;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RootsResponse {
    schema: &'static str,
    roots: Vec<RootChoice>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RootChoice {
    label: String,
    browse_token: String,
    readable: bool,
    selectable: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OpenPathRequest {
    path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenPathResponse {
    schema: &'static str,
    label: String,
    browse_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BrowseRequest {
    browse_token: String,
    action: BrowseAction,
    #[serde(default = "default_page_size")]
    page_size: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BrowseAction {
    Current,
    Child { name: String },
    Parent,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowseResponse {
    schema: &'static str,
    browse_token: String,
    label: String,
    depth: usize,
    maximum_depth: usize,
    breadcrumbs: Vec<Breadcrumb>,
    directories: Vec<DirectoryChoice>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Breadcrumb {
    label: String,
    browse_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectoryChoice {
    name: String,
    kind: &'static str,
    readable: bool,
    selectable: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IndexMode {
    Auto,
    Structural,
    Deep,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreflightRequest {
    browse_token: String,
    mode: IndexMode,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreflightResponse {
    schema: &'static str,
    selectable: bool,
    normalized_display_label: String,
    daemon_accepting_operations: bool,
    selected_mode: IndexMode,
    supported_modes: [IndexMode; 3],
    adapter_isolation: &'static str,
    estimated_limitations: [&'static str; 1],
    warnings: Vec<&'static str>,
    root_capability: String,
    root_capability_expires_in_seconds: u64,
}

pub(crate) async fn roots(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<RootsResponse>, ApiError> {
    let candidates = platform_root_candidates();
    let opened = tokio::task::spawn_blocking(move || {
        let cancellation = filesystem_cancellation()?;
        let mut opened = Vec::new();
        for candidate in candidates {
            if let Ok(directory) = BrowseDirectory::open(&candidate.path, &cancellation) {
                opened.push((candidate.label, directory));
            }
        }
        Ok::<_, FilesystemRegistryError>(opened)
    })
    .await
    .map_err(|_| ApiError::filesystem_unavailable())?
    .map_err(map_registry_error)?;

    let now = Instant::now();
    let owner = session.identity();
    let mut roots = Vec::new();
    for (label, directory) in opened {
        let issued = state
            .filesystem()
            .issue_browse(owner, directory, None, label.clone(), None, now)
            .map_err(map_registry_error)?;
        roots.push(RootChoice {
            label,
            browse_token: issued.token,
            readable: true,
            selectable: true,
        });
    }
    roots.sort_by(|left, right| left.label.cmp(&right.label));
    roots.truncate(MAX_PLATFORM_ROOTS);
    Ok(Json(RootsResponse {
        schema: "rootlight.web-filesystem-roots/1",
        roots,
    }))
}

pub(crate) async fn open_path(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<OpenPathRequest>,
) -> Result<Json<OpenPathResponse>, ApiError> {
    let candidate = parse_direct_path(&request.path)?;
    let label = safe_path_label(&candidate);
    let directory = tokio::task::spawn_blocking(move || {
        let cancellation = filesystem_cancellation()?;
        BrowseDirectory::open(&candidate, &cancellation)
            .map_err(FilesystemRegistryError::from_browse)
    })
    .await
    .map_err(|_| ApiError::filesystem_unavailable())?
    .map_err(map_registry_error)?;
    let issued = state
        .filesystem()
        .issue_browse(
            session.identity(),
            directory,
            None,
            label.clone(),
            None,
            Instant::now(),
        )
        .map_err(map_registry_error)?;

    Ok(Json(OpenPathResponse {
        schema: "rootlight.web-filesystem-open-path/1",
        label,
        browse_token: issued.token,
    }))
}

pub(crate) async fn browse(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<BrowseRequest>,
) -> Result<Json<BrowseResponse>, ApiError> {
    let page_size = BrowsePageSize::new(request.page_size)
        .map_err(|_| ApiError::invalid_filesystem_request())?;
    let filter = parse_filter(request.filter)?;
    let filter_digest = filter_digest(filter.as_deref());
    let owner = session.identity();
    let now = Instant::now();
    let current = state
        .filesystem()
        .resolve_browse(owner, &request.browse_token, now)
        .map_err(map_registry_error)?;

    let issued = match request.action {
        BrowseAction::Current => IssuedBrowseCapability {
            token: current.token().to_owned(),
            node: current,
        },
        BrowseAction::Child { name } => {
            if request.cursor.is_some() {
                return Err(ApiError::invalid_filesystem_request());
            }
            validate_child_selector(&name)?;
            open_child(&state, owner, current, name, now).await?
        }
        BrowseAction::Parent => {
            if request.cursor.is_some() {
                return Err(ApiError::invalid_filesystem_request());
            }
            let parent = current
                .parent()
                .ok_or_else(ApiError::filesystem_capability_invalid)?;
            state
                .filesystem()
                .retain_browse(owner, parent, now)
                .map_err(map_registry_error)?
        }
    };

    let offset = match request.cursor.as_deref() {
        Some(cursor) => state
            .filesystem()
            .resolve_cursor(
                owner,
                cursor,
                &issued.node.token_digest(),
                &filter_digest,
                now,
            )
            .map_err(map_registry_error)?,
        None => 0,
    };
    let node = Arc::clone(&issued.node);
    let page = tokio::task::spawn_blocking(move || {
        let cancellation = filesystem_cancellation()?;
        let snapshot = node.snapshot(&cancellation)?;
        Ok::<_, FilesystemRegistryError>(render_page(
            &snapshot,
            filter.as_deref(),
            offset,
            page_size,
        ))
    })
    .await
    .map_err(|_| ApiError::filesystem_unavailable())?
    .map_err(map_registry_error)?;
    let next_cursor = page
        .next_offset
        .map(|next_offset| {
            state.filesystem().issue_cursor(
                owner,
                issued.node.token_digest(),
                filter_digest,
                next_offset,
                now,
            )
        })
        .transpose()
        .map_err(map_registry_error)?;

    Ok(Json(BrowseResponse {
        schema: "rootlight.web-filesystem-browse/1",
        browse_token: issued.token,
        label: issued.node.label().to_owned(),
        depth: issued.node.depth(),
        maximum_depth: MAX_BROWSE_DEPTH,
        breadcrumbs: breadcrumbs(&issued.node),
        directories: page.directories,
        next_cursor,
    }))
}

pub(crate) async fn preflight_index(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<PreflightRequest>,
) -> Result<Json<PreflightResponse>, ApiError> {
    let owner = session.identity();
    let node = state
        .filesystem()
        .resolve_browse(owner, &request.browse_token, Instant::now())
        .map_err(map_registry_error)?;
    let timeout = RequestTimeout::new(PREFLIGHT_HEALTH_TIMEOUT)
        .map_err(|_| ApiError::daemon_unavailable())?;
    let health = state
        .daemon()
        .health(timeout)
        .await
        .map_err(|error| ApiError::from_daemon(&error))?;

    let local_path = node.directory().local_path().to_path_buf();
    let display_label = node.label().to_owned();
    let directory = tokio::task::spawn_blocking(move || {
        let cancellation = filesystem_cancellation()?;
        BrowseDirectory::open(&local_path, &cancellation)
            .map_err(FilesystemRegistryError::from_browse)
    })
    .await
    .map_err(|_| ApiError::filesystem_unavailable())?
    .map_err(map_registry_error)?;
    let capability = state
        .filesystem()
        .issue_root(
            owner,
            RootAdmission::new(directory, display_label.clone()),
            Instant::now(),
        )
        .map_err(map_registry_error)?;
    let adapter_isolation = adapter_isolation_label(health.adapter_status);
    let mut warnings = Vec::new();
    if !health.accepting_operations {
        warnings.push("daemon_not_accepting_operations");
    }
    match health.adapter_status {
        HealthStatus::Degraded => warnings.push("adapter_isolation_degraded"),
        HealthStatus::Unavailable | HealthStatus::Failed | HealthStatus::NotConfigured => {
            warnings.push("adapter_isolation_unavailable");
        }
        HealthStatus::Healthy => {}
    }

    Ok(Json(PreflightResponse {
        schema: "rootlight.web-index-preflight/1",
        selectable: true,
        normalized_display_label: display_label,
        daemon_accepting_operations: health.accepting_operations,
        selected_mode: request.mode,
        supported_modes: [IndexMode::Auto, IndexMode::Structural, IndexMode::Deep],
        adapter_isolation,
        estimated_limitations: ["repository_contents_not_scanned"],
        warnings,
        root_capability: capability.token,
        root_capability_expires_in_seconds: ROOT_CAPABILITY_TTL_SECONDS,
    }))
}

async fn open_child(
    state: &AppState,
    owner: crate::session::SessionIdentity,
    parent: Arc<BrowseNode>,
    requested_name: String,
    now: Instant,
) -> Result<IssuedBrowseCapability, ApiError> {
    let parent_for_open = Arc::clone(&parent);
    let name_for_open = requested_name.clone();
    let display_label = truncate_utf16(&requested_name, MAX_BROWSER_LABEL_UTF16_UNITS);
    let (directory, snapshot) = tokio::task::spawn_blocking(move || {
        let cancellation = filesystem_cancellation()?;
        let parent_snapshot = parent_for_open.snapshot(&cancellation)?;
        let exact_name = exact_child_name(&parent_snapshot, &name_for_open)?;
        let directory = parent_for_open
            .directory()
            .open_child(&exact_name, &cancellation)
            .map_err(FilesystemRegistryError::from_browse)?;
        let snapshot = Arc::new(
            directory
                .snapshot(&cancellation)
                .map_err(FilesystemRegistryError::from_browse)?,
        );
        Ok::<_, FilesystemRegistryError>((directory, snapshot))
    })
    .await
    .map_err(|_| ApiError::filesystem_unavailable())?
    .map_err(map_registry_error)?;

    state
        .filesystem()
        .issue_browse(
            owner,
            directory,
            Some(snapshot),
            display_label,
            Some(parent),
            now,
        )
        .map_err(map_registry_error)
}

struct RenderedPage {
    directories: Vec<DirectoryChoice>,
    next_offset: Option<usize>,
}

fn render_page(
    snapshot: &BrowseDirectorySnapshot,
    filter: Option<&str>,
    offset: usize,
    page_size: BrowsePageSize,
) -> RenderedPage {
    let scan_size = BrowsePageSize::new(PAGE_SCAN_SIZE).expect("VFS maximum page size is valid");
    let mut scan_offset = 0usize;
    let mut filtered_offset = 0usize;
    let mut directories = Vec::with_capacity(page_size.get());
    let mut has_more = false;

    while scan_offset < snapshot.len() {
        let page = snapshot.page(
            BrowsePageOffset::new(scan_offset).expect("snapshot offset is VFS bounded"),
            scan_size,
        );
        for entry in page.entries() {
            if !matches_filter(entry, filter) {
                continue;
            }
            if filtered_offset >= offset {
                if directories.len() < page_size.get() {
                    directories.push(DirectoryChoice {
                        name: entry.display_name().to_owned(),
                        kind: "directory",
                        readable: true,
                        selectable: true,
                    });
                } else {
                    has_more = true;
                    break;
                }
            }
            filtered_offset = filtered_offset.saturating_add(1);
        }
        if has_more {
            break;
        }
        let Some(next) = page.next_offset() else {
            break;
        };
        scan_offset = next.get();
    }

    let next_offset = has_more.then_some(offset.saturating_add(directories.len()));
    RenderedPage {
        directories,
        next_offset,
    }
}

fn exact_child_name(
    snapshot: &BrowseDirectorySnapshot,
    requested_name: &str,
) -> Result<OsString, FilesystemRegistryError> {
    let scan_size = BrowsePageSize::new(PAGE_SCAN_SIZE).expect("VFS maximum page size is valid");
    let mut offset = 0usize;
    let mut matched = None;
    while offset < snapshot.len() {
        let page = snapshot.page(
            BrowsePageOffset::new(offset).expect("snapshot offset is VFS bounded"),
            scan_size,
        );
        for entry in page.entries() {
            if entry.display_name() == requested_name {
                if matched.is_some() {
                    return Err(FilesystemRegistryError::CapabilityInvalid);
                }
                matched = Some(entry.name().to_os_string());
            }
        }
        let Some(next) = page.next_offset() else {
            break;
        };
        offset = next.get();
    }
    matched.ok_or(FilesystemRegistryError::CapabilityInvalid)
}

fn breadcrumbs(node: &Arc<BrowseNode>) -> Vec<Breadcrumb> {
    let mut breadcrumbs = node
        .ancestors()
        .into_iter()
        .map(|ancestor| Breadcrumb {
            label: ancestor.label().to_owned(),
            browse_token: ancestor.token().to_owned(),
        })
        .collect::<Vec<_>>();
    breadcrumbs.push(Breadcrumb {
        label: node.label().to_owned(),
        browse_token: node.token().to_owned(),
    });
    breadcrumbs
}

fn matches_filter(entry: &BrowseDirectoryEntry, filter: Option<&str>) -> bool {
    entry.display_name().encode_utf16().count() <= MAX_BROWSER_DIRECTORY_NAME_UTF16_UNITS
        && filter.is_none_or(|filter| entry.display_name().to_lowercase().contains(filter))
}

fn parse_direct_path(candidate: &str) -> Result<PathBuf, ApiError> {
    if candidate.is_empty()
        || candidate.len() > MAX_DIRECT_PATH_BYTES
        || candidate.as_bytes().contains(&0)
    {
        return Err(ApiError::invalid_filesystem_request());
    }
    let path = PathBuf::from(candidate);
    if !path.is_absolute() {
        return Err(ApiError::invalid_filesystem_request());
    }
    Ok(path)
}

fn validate_child_selector(name: &str) -> Result<(), ApiError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_BROWSE_CHILD_NAME_BYTES
        || name.encode_utf16().count() > MAX_BROWSER_DIRECTORY_NAME_UTF16_UNITS
        || name.contains(['/', '\\', '\0'])
    {
        return Err(ApiError::invalid_filesystem_request());
    }
    Ok(())
}

fn parse_filter(filter: Option<String>) -> Result<Option<String>, ApiError> {
    match filter {
        Some(filter) if filter.len() > MAX_FILTER_BYTES || filter.contains('\0') => {
            Err(ApiError::invalid_filesystem_request())
        }
        Some(filter) if filter.is_empty() => Ok(None),
        Some(filter) => Ok(Some(filter.to_lowercase())),
        None => Ok(None),
    }
}

fn filter_digest(filter: Option<&str>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    match filter {
        Some(filter) => {
            hasher.update([1]);
            hasher.update(filter.as_bytes());
        }
        None => hasher.update([0]),
    }
    hasher.finalize().into()
}

fn safe_path_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(|name| truncate_utf16(name, MAX_BROWSER_LABEL_UTF16_UNITS))
        .unwrap_or_else(|| "Selected filesystem root".to_owned())
}

fn truncate_utf16(value: &str, maximum_units: usize) -> String {
    let mut units = 0usize;
    value
        .chars()
        .take_while(|character| {
            let width = character.len_utf16();
            let Some(next) = units.checked_add(width) else {
                return false;
            };
            if next > maximum_units {
                return false;
            }
            units = next;
            true
        })
        .collect()
}

fn filesystem_cancellation() -> Result<Cancellation, FilesystemRegistryError> {
    let deadline = Instant::now()
        .checked_add(FILESYSTEM_OPERATION_TIMEOUT)
        .ok_or(FilesystemRegistryError::ResourceUnavailable)?;
    Ok(Cancellation::with_deadline(deadline))
}

fn map_registry_error(error: FilesystemRegistryError) -> ApiError {
    match error {
        FilesystemRegistryError::InvalidRequest => ApiError::invalid_filesystem_request(),
        FilesystemRegistryError::CapabilityInvalid => ApiError::filesystem_capability_invalid(),
        FilesystemRegistryError::LimitReached => ApiError::filesystem_limit_reached(),
        FilesystemRegistryError::ResourceUnavailable => ApiError::filesystem_unavailable(),
    }
}

const fn adapter_isolation_label(status: HealthStatus) -> &'static str {
    match status {
        HealthStatus::Healthy => "available",
        HealthStatus::Degraded => "degraded",
        HealthStatus::Unavailable | HealthStatus::Failed => "unavailable",
        HealthStatus::NotConfigured => "not_configured",
    }
}

struct PlatformRootCandidate {
    label: String,
    path: PathBuf,
}

fn platform_root_candidates() -> Vec<PlatformRootCandidate> {
    let mut candidates = system_root_candidates();
    if let Some(user_dirs) = directories::UserDirs::new() {
        let home = user_dirs.home_dir().to_path_buf();
        if !candidates.iter().any(|candidate| candidate.path == home) {
            candidates.push(PlatformRootCandidate {
                label: "Home".to_owned(),
                path: home,
            });
        }
    }
    candidates.sort_by(|left, right| left.label.cmp(&right.label));
    candidates.truncate(MAX_PLATFORM_ROOTS);
    candidates
}

#[cfg(windows)]
fn system_root_candidates() -> Vec<PlatformRootCandidate> {
    (b'A'..=b'Z')
        .map(|letter| {
            let label = format!("{}:", char::from(letter));
            PlatformRootCandidate {
                path: PathBuf::from(format!("{label}\\")),
                label,
            }
        })
        .collect()
}

#[cfg(not(windows))]
fn system_root_candidates() -> Vec<PlatformRootCandidate> {
    vec![PlatformRootCandidate {
        label: "Filesystem".to_owned(),
        path: PathBuf::from("/"),
    }]
}

const fn default_page_size() -> usize {
    DEFAULT_PAGE_SIZE
}

#[cfg(test)]
mod tests {
    use std::{fs, future::Future, pin::Pin, sync::Arc, time::Instant};

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{
            Method, Request, StatusCode,
            header::{CONTENT_TYPE, COOKIE, HOST},
        },
    };
    use data_encoding::HEXLOWER;
    use rootlight_client::{
        ClientError, DaemonLifecycle, Health, RepositoryCatalogPage, RepositoryCatalogPageRequest,
        RepositoryStatus, RepositoryStatusRequest, RequestTimeout, ResourcePressure,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt as _;

    use super::*;
    use crate::{
        app,
        assets::AssetInventory,
        daemon::DaemonClient,
        filesystem_registry::FilesystemRegistry,
        graph_registry::GraphRegistry,
        index_registry::IndexRegistry,
        security::SecurityPolicy,
        session::{CSRF_HEADER_NAME, SESSION_COOKIE_NAME, SessionIdentity, SessionRegistry},
        support_registry::SupportRegistry,
    };

    const TEST_PORT: u16 = 43_131;
    const TEST_RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

    #[tokio::test]
    async fn direct_open_and_browse_are_path_redacted_and_snapshot_paged() {
        let fixture = TestApp::new();
        let repository = crate::test_support::local_tempdir();
        for name in ["zeta", "alpha", "middle"] {
            fs::create_dir(repository.path().join(name)).expect("fixture directory exists");
        }
        fs::write(repository.path().join("ignored.rs"), b"source").expect("fixture file exists");

        let (status, opened) = fixture
            .post(
                "/api/v1/filesystem/open-path",
                json!({ "path": repository.path().to_string_lossy() }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_no_raw_path(&opened, repository.path());
        let browse_token = opened["browseToken"]
            .as_str()
            .expect("browse token is returned");

        let (status, first_page) = fixture
            .post(
                "/api/v1/filesystem/browse",
                json!({
                    "browseToken": browse_token,
                    "action": { "type": "current" },
                    "pageSize": 1
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first_page["directories"][0]["name"], "alpha");
        let cursor = first_page["nextCursor"]
            .as_str()
            .expect("first page returns opaque cursor");
        assert_no_raw_path(&first_page, repository.path());

        fs::create_dir(repository.path().join("aardvark")).expect("post-snapshot directory exists");
        let (status, second_page) = fixture
            .post(
                "/api/v1/filesystem/browse",
                json!({
                    "browseToken": browse_token,
                    "action": { "type": "current" },
                    "pageSize": 1,
                    "cursor": cursor
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second_page["directories"][0]["name"], "middle");

        let (status, _) = fixture
            .post(
                "/api/v1/filesystem/browse",
                json!({
                    "browseToken": browse_token,
                    "action": { "type": "child", "name": "../escape" }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = fixture
            .post(
                "/api/v1/filesystem/open-path",
                json!({ "path": "relative/repository" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let missing = repository.path().join("secret-leak-marker");
        let (status, error) = fixture
            .post(
                "/api/v1/filesystem/open-path",
                json!({ "path": missing.to_string_lossy() }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!error.to_string().contains("secret-leak-marker"));
    }

    #[tokio::test]
    async fn preflight_does_not_enumerate_and_root_capability_is_retry_bound() {
        let fixture = TestApp::new();
        let repository = crate::test_support::local_tempdir();
        for index in 0..=rootlight_vfs::MAX_BROWSE_DIRECTORY_ENTRIES {
            fs::write(repository.path().join(format!("file-{index}")), [])
                .expect("bounded fixture file exists");
        }
        let (_, opened) = fixture
            .post(
                "/api/v1/filesystem/open-path",
                json!({ "path": repository.path().to_string_lossy() }),
            )
            .await;
        let browse_token = opened["browseToken"]
            .as_str()
            .expect("browse token is returned");

        let (status, preflight) = fixture
            .post(
                "/api/v1/filesystem/preflight-index",
                json!({ "browseToken": browse_token, "mode": "auto" }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(preflight["daemonAcceptingOperations"], true);
        assert_eq!(
            preflight["estimatedLimitations"][0],
            "repository_contents_not_scanned"
        );
        assert_no_raw_path(&preflight, repository.path());
        let root_capability = preflight["rootCapability"]
            .as_str()
            .expect("root capability is returned");
        let admission = fixture
            .filesystem
            .bind_root(fixture.identity, root_capability, [1; 32], Instant::now())
            .expect("root capability is bound");
        assert_eq!(admission.local_path(), repository.path());
        assert!(
            fixture
                .filesystem
                .bind_root(fixture.identity, root_capability, [1; 32], Instant::now())
                .is_ok()
        );
        assert!(
            fixture
                .filesystem
                .bind_root(fixture.identity, root_capability, [2; 32], Instant::now())
                .is_err()
        );

        let (status, _) = fixture
            .post(
                "/api/v1/filesystem/preflight-index",
                json!({ "browseToken": browse_token, "mode": "unsupported" }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn rendered_page_omits_files_and_linked_directories() {
        let repository = crate::test_support::local_tempdir();
        let outside = crate::test_support::local_tempdir();
        fs::create_dir(repository.path().join("ordinary")).expect("ordinary directory exists");
        fs::write(repository.path().join("source.rs"), b"source").expect("fixture file exists");
        let linked = create_directory_link(outside.path(), &repository.path().join("linked"));
        let directory = BrowseDirectory::open(repository.path(), &Cancellation::new())
            .expect("repository opens through VFS");
        let snapshot = directory
            .snapshot(&Cancellation::new())
            .expect("directory snapshot succeeds");
        let page = render_page(
            &snapshot,
            None,
            0,
            BrowsePageSize::new(16).expect("page size is valid"),
        );
        let names = page
            .directories
            .iter()
            .map(|directory| directory.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["ordinary"]);
        if linked {
            assert!(!names.contains(&"linked"));
        }
    }

    #[test]
    fn platform_roots_are_bounded_sorted_and_path_free_at_the_dto_boundary() {
        let candidates = platform_root_candidates();
        assert!(candidates.len() <= MAX_PLATFORM_ROOTS);
        assert!(
            candidates
                .windows(2)
                .all(|pair| pair[0].label <= pair[1].label)
        );
        for candidate in candidates {
            let dto = RootChoice {
                label: candidate.label,
                browse_token: "opaque".to_owned(),
                readable: true,
                selectable: true,
            };
            let value = serde_json::to_value(dto).expect("root DTO serializes");
            assert!(
                value
                    .as_object()
                    .is_some_and(|fields| !fields.contains_key("path"))
            );
        }
    }

    #[test]
    fn browser_labels_truncate_on_utf16_boundaries() {
        let label = "😀".repeat(MAX_BROWSER_LABEL_UTF16_UNITS);
        let truncated = truncate_utf16(&label, MAX_BROWSER_LABEL_UTF16_UNITS);

        assert_eq!(
            truncated.encode_utf16().count(),
            MAX_BROWSER_LABEL_UTF16_UNITS
        );
        assert_eq!(truncated.chars().count(), MAX_BROWSER_LABEL_UTF16_UNITS / 2);
    }

    struct TestApp {
        router: Router,
        cookie: String,
        csrf: String,
        identity: SessionIdentity,
        filesystem: Arc<FilesystemRegistry>,
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
            let bootstrap = sessions.issue_bootstrap(now).expect("bootstrap issues");
            let credentials = sessions
                .consume_bootstrap(bootstrap.encoded(), now)
                .expect("session issues");
            let identity = sessions
                .authenticate(&credentials.cookie_value, now)
                .expect("session authenticates")
                .identity();
            let cookie = format!("{SESSION_COOKIE_NAME}={}", credentials.cookie_value);
            let filesystem = Arc::new(FilesystemRegistry::new());
            let state = app::AppState::new(
                assets,
                Arc::new(FilesystemDaemon),
                sessions,
                Arc::clone(&filesystem),
                Arc::new(IndexRegistry::new()),
                Arc::new(GraphRegistry::new()),
                Arc::new(SupportRegistry::new()),
            );
            Self {
                router: app::router(state, SecurityPolicy::loopback(TEST_PORT)),
                cookie,
                csrf: credentials.csrf_token,
                identity,
                filesystem,
            }
        }

        async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
            let request = Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(HOST, format!("127.0.0.1:{TEST_PORT}"))
                .header("origin", format!("http://127.0.0.1:{TEST_PORT}"))
                .header("sec-fetch-site", "same-origin")
                .header(CONTENT_TYPE, "application/json")
                .header(COOKIE, &self.cookie)
                .header(CSRF_HEADER_NAME, &self.csrf)
                .body(Body::from(
                    serde_json::to_vec(&body).expect("request body serializes"),
                ))
                .expect("filesystem request builds");
            let response = self
                .router
                .clone()
                .oneshot(request)
                .await
                .expect("filesystem response returns");
            let status = response.status();
            let body = to_bytes(response.into_body(), TEST_RESPONSE_LIMIT)
                .await
                .expect("filesystem response body reads");
            let value = serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()));
            (status, value)
        }
    }

    struct FilesystemDaemon;

    impl DaemonClient for FilesystemDaemon {
        fn health<'a>(
            &'a self,
            _timeout: RequestTimeout,
        ) -> Pin<Box<dyn Future<Output = Result<Health, ClientError>> + Send + 'a>> {
            Box::pin(async {
                Ok(Health {
                    ready: true,
                    active_operations: 0,
                    admitted_operations: 0,
                    protocol_version: "1.9".to_owned(),
                    lifecycle: DaemonLifecycle::Ready,
                    accepting_operations: true,
                    active_connections: 1,
                    connection_limit: 128,
                    queued_operations: 0,
                    running_operations: 0,
                    operation_queue_limit: 256,
                    journal_healthy: true,
                    catalog_status: HealthStatus::Healthy,
                    catalog_schema_version: 2,
                    generation_status: HealthStatus::Healthy,
                    adapter_status: HealthStatus::Healthy,
                    watcher_status: HealthStatus::NotConfigured,
                    resource_pressure: ResourcePressure::Normal,
                    endpoint_status: HealthStatus::Healthy,
                    endpoint_schema_version: 2,
                })
            })
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
    }

    fn assert_no_raw_path(value: &Value, path: &Path) {
        let path = path.to_string_lossy();
        fn inspect(value: &Value, forbidden: &str) {
            match value {
                Value::Object(fields) => {
                    for (name, value) in fields {
                        assert!(
                            !name.to_ascii_lowercase().contains("path"),
                            "response contains path-shaped field {name}"
                        );
                        inspect(value, forbidden);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        inspect(value, forbidden);
                    }
                }
                Value::String(text) => {
                    assert!(
                        !text.contains(forbidden),
                        "response contains an absolute filesystem path"
                    );
                }
                Value::Null | Value::Bool(_) | Value::Number(_) => {}
            }
        }
        inspect(value, &path);
    }

    #[cfg(unix)]
    fn create_directory_link(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).expect("directory symlink is created");
        true
    }

    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) -> bool {
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1_314;

        match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => false,
            Err(error) => panic!("directory reparse point is created: {error}"),
        }
    }
}
