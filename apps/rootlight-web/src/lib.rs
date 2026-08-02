//! Secure loopback BFF and static host for the Rootlight browser interface.

#![forbid(unsafe_code)]

mod app;
mod assets;
mod browser;
mod config;
mod daemon;
mod error;
mod security;
mod session;

use std::{ffi::OsString, net::Ipv4Addr, sync::Arc, time::Instant};

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
    let bootstrap = sessions.issue_bootstrap(Instant::now())?;
    let url = format!("{}/#bootstrap={}", policy.origin(), bootstrap.encoded());
    println!("Rootlight Web UI: {url}");
    if config.open_browser() {
        let _ = browser::open(&url);
    }
    let state = app::AppState::new(assets, daemon, Arc::clone(&sessions));
    let router = app::router(state, policy);
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| WebError::ServerFailed);
    sessions.clear();
    result
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
