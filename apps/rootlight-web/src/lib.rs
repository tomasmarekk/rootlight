//! Secure loopback BFF and static host for the Rootlight browser interface.

#![forbid(unsafe_code)]

mod api;
mod app;
mod assets;
mod browser;
mod config;
mod daemon;
mod error;
mod filesystem_registry;
mod graph_registry;
mod index_registry;
mod security;
mod session;
mod source_registry;
mod support_registry;

use std::{ffi::OsString, net::Ipv4Addr, sync::Arc, time::Duration};

use rootlight_client::RequestTimeout;
use tokio::net::TcpListener;

pub use error::WebError;

/// Runs one authenticated loopback web-host instance until process shutdown.
///
/// The server binds only IPv4 loopback, verifies the complete production asset
/// inventory before serving, and connects to domain data exclusively through
/// `rootlight-client`.
///
/// # Panics
///
/// Panics if polled without Tokio's network, signal, time, and task drivers.
///
/// # Errors
///
/// Returns [`WebError`] for invalid arguments, unavailable trusted assets,
/// runtime or daemon startup failure, loopback bind failure, or HTTP serving
/// failure.
pub async fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), WebError> {
    let config = config::WebConfig::parse(arguments)?;
    let asset_root = config.asset_root().to_path_buf();
    let assets = tokio::task::spawn_blocking(move || assets::AssetInventory::load(&asset_root))
        .await
        .map_err(|_| WebError::TaskFailed)??;
    let paths = config::runtime_paths()?;
    let daemon = daemon::connect(paths).await?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.listen_port()))
        .await
        .map_err(|_| WebError::ListenerUnavailable)?;
    let address = listener
        .local_addr()
        .map_err(|_| WebError::ListenerUnavailable)?;
    if !address.ip().is_loopback() {
        return Err(WebError::ListenerUnavailable);
    }
    let policy = security::SecurityPolicy::loopback(address.port());
    let sessions = Arc::new(session::SessionRegistry::new());
    let filesystem = Arc::new(filesystem_registry::FilesystemRegistry::new());
    let indexes = Arc::new(index_registry::IndexRegistry::new());
    let graphs = Arc::new(graph_registry::GraphRegistry::new());
    let support = Arc::new(support_registry::SupportRegistry::new());
    let url = format!("{}/", policy.origin());
    println!("Rootlight Web UI: {url}");
    if config.open_browser() {
        let _ = browser::open(&url);
    }
    let state = app::AppState::new(
        assets,
        daemon.client(),
        Arc::clone(&sessions),
        Arc::clone(&filesystem),
        Arc::clone(&indexes),
        Arc::clone(&graphs),
        Arc::clone(&support),
    );
    let router = app::router(state.clone(), policy);
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| WebError::ServerFailed);
    sessions.clear();
    filesystem.clear();
    indexes.clear();
    state.sources().clear();
    support.clear();
    if let Ok(timeout) = RequestTimeout::try_from(Duration::from_secs(2)) {
        for handle in graphs.clear() {
            let _ = daemon
                .client()
                .graph_projection_release(handle.projection(), timeout)
                .await;
        }
    }
    drop(state);
    let shutdown = daemon.shutdown().await;
    result?;
    shutdown
}

#[cfg(not(windows))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(windows)]
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::windows::ctrl_c().ok();
    let ctrl_break = tokio::signal::windows::ctrl_break().ok();
    tokio::select! {
        () = receive_ctrl_c(ctrl_c) => {}
        () = receive_ctrl_break(ctrl_break) => {}
    }
}

#[cfg(windows)]
async fn receive_ctrl_c(signal: Option<tokio::signal::windows::CtrlC>) {
    let Some(mut signal) = signal else {
        std::future::pending().await
    };
    let _ = signal.recv().await;
}

#[cfg(windows)]
async fn receive_ctrl_break(signal: Option<tokio::signal::windows::CtrlBreak>) {
    let Some(mut signal) = signal else {
        std::future::pending().await
    };
    let _ = signal.recv().await;
}

#[cfg(test)]
mod test_support {
    pub(crate) fn local_tempdir() -> tempfile::TempDir {
        let current = std::env::current_dir().expect("current directory is available");
        tempfile::tempdir_in(current).expect("local temporary directory is available")
    }
}
