//! Async thin-client boundary between browser handlers and the Rootlight daemon.

use std::{future::Future, pin::Pin, sync::Arc};

use rootlight_client::{
    Client, ClientError, ConnectPolicy, Health, RepositoryCatalogPage,
    RepositoryCatalogPageRequest, RepositoryStatus, RepositoryStatusRequest, RequestTimeout,
};
use rootlight_runtime::RuntimePaths;

use crate::error::WebError;

const WEB_CLIENT_INSTANCE_PREFIX: [u8; 8] = *b"rootweb1";

type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>>;

pub(crate) trait DaemonClient: Send + Sync {
    fn health<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, Health>;

    fn repository_catalog_page<'a>(
        &'a self,
        request: &'a RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryCatalogPage>;

    fn repository_status<'a>(
        &'a self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryStatus>;
}

impl DaemonClient for Client {
    fn health<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, Health> {
        Box::pin(self.health_async(timeout))
    }

    fn repository_catalog_page<'a>(
        &'a self,
        request: &'a RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryCatalogPage> {
        Box::pin(self.repository_catalog_page_async(request, timeout))
    }

    fn repository_status<'a>(
        &'a self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryStatus> {
        Box::pin(self.repository_status_with_options_async(request, timeout))
    }
}

/// Connects a web-session client to the authenticated local daemon.
///
/// The synchronous discovery/start protocol runs on Tokio's blocking pool so
/// it never occupies an async HTTP worker.
///
/// # Errors
///
/// Returns a source-free [`WebError`] when randomness, discovery, daemon
/// startup, or the blocking task boundary fails.
pub(crate) async fn connect(paths: RuntimePaths) -> Result<Arc<dyn DaemonClient>, WebError> {
    let mut client_instance_id = [0_u8; 16];
    client_instance_id[..WEB_CLIENT_INSTANCE_PREFIX.len()]
        .copy_from_slice(&WEB_CLIENT_INSTANCE_PREFIX);
    getrandom::fill(&mut client_instance_id[WEB_CLIENT_INSTANCE_PREFIX.len()..])
        .map_err(|_| WebError::RandomUnavailable)?;
    tokio::task::spawn_blocking(move || {
        Client::connect_or_start(&paths, client_instance_id, ConnectPolicy::StartIfMissing)
            .map(|client| Arc::new(client) as Arc<dyn DaemonClient>)
            .map_err(|_| WebError::DaemonUnavailable)
    })
    .await
    .map_err(|_| WebError::TaskFailed)?
}
