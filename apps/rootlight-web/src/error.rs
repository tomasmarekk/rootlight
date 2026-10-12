//! Source-free error taxonomy for the local web host boundary.

use rootlight_client::ClientError;
use rootlight_runtime::WebServiceStartupFailure;
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
    /// The coordinated daemon startup exceeded its readiness deadline.
    #[error("the Rootlight daemon did not become ready before its startup deadline")]
    DaemonStartTimedOut,
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

impl WebError {
    /// Classifies a daemon startup failure without retaining private error sources.
    #[must_use]
    pub fn from_daemon_start(error: ClientError) -> Self {
        match error {
            ClientError::DaemonStartTimedOut => Self::DaemonStartTimedOut,
            _ => Self::DaemonUnavailable,
        }
    }

    /// Returns the diagnosis permitted on the private service-child boundary.
    ///
    /// Ordinary web-host invocations must keep their regular failure status.
    #[must_use]
    pub const fn service_startup_failure(&self) -> Option<WebServiceStartupFailure> {
        match self {
            Self::DaemonStartTimedOut => Some(WebServiceStartupFailure::DaemonStartTimedOut),
            Self::DaemonUnavailable => Some(WebServiceStartupFailure::DaemonUnavailable),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_start_classification_preserves_timeout_without_private_sources() {
        let timeout = WebError::from_daemon_start(ClientError::DaemonStartTimedOut);
        let failure = timeout
            .service_startup_failure()
            .expect("timeout is classified");
        assert_eq!(failure.exit_code(), 80);
        assert_eq!(failure.diagnostic_reason(), "daemon_start_timeout");

        let generic = WebError::from_daemon_start(ClientError::LaunchIo(std::io::Error::other(
            "private-source-and-bootstrap-capability",
        )));
        assert_eq!(generic, WebError::DaemonUnavailable);
        assert_eq!(
            generic
                .service_startup_failure()
                .expect("daemon failure is classified")
                .exit_code(),
            81
        );
        assert!(
            !generic
                .to_string()
                .contains("private-source-and-bootstrap-capability")
        );
        assert!(std::error::Error::source(&generic).is_none());
    }
}
