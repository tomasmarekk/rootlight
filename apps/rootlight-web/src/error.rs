//! Source-free error taxonomy for the local web host boundary.

use thiserror::Error;

/// Stable failures exposed by the `rootlight-web` process.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebError {
    /// Command-line arguments violate the closed web-host grammar.
    #[error("invalid web host arguments")]
    InvalidArguments,
    /// Account-private Rootlight runtime paths are unavailable.
    #[error("local runtime paths are unavailable")]
    RuntimeUnavailable,
    /// The daemon could not be discovered, started, or authenticated.
    #[error("the Rootlight daemon is unavailable")]
    DaemonUnavailable,
    /// Cryptographic randomness required by a local session is unavailable.
    #[error("secure session randomness is unavailable")]
    RandomUnavailable,
    /// The immutable web asset inventory is missing, malformed, or corrupt.
    #[error("trusted web assets are unavailable")]
    AssetsUnavailable,
    /// The loopback listener could not be created.
    #[error("the local web listener is unavailable")]
    ListenerUnavailable,
    /// The HTTP server stopped unexpectedly.
    #[error("the local web server failed")]
    ServerFailed,
    /// The background task boundary failed before returning its result.
    #[error("local web startup failed")]
    TaskFailed,
}
