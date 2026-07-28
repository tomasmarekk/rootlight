//! Typed failures for process creation and ownership.

use std::io;

/// Failure while validating, starting, controlling, or reaping a process.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// The process command contains an invalid path, argument, or environment entry.
    #[error("invalid process command: {0}")]
    InvalidInput(String),
    /// A portable operating-system process operation failed.
    #[error("process operation {operation} failed")]
    Io {
        /// Stable operation label.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// A Windows process or Job Object operation failed.
    #[cfg(windows)]
    #[error("Windows process operation {operation} failed ({code:#010x}): {message}")]
    Windows {
        /// Stable operation label.
        operation: &'static str,
        /// HRESULT representation of the Win32 failure.
        code: u32,
        /// Source-redacted operating-system diagnostic.
        message: String,
    },
    /// The requested containment primitive does not exist on this platform.
    #[error("process containment is unsupported on this platform")]
    UnsupportedPlatform,
    /// A bounded process or Job Object wait reached its deadline.
    #[error("process operation {operation} reached its deadline")]
    Deadline {
        /// Stable operation label.
        operation: &'static str,
    },
}

impl ProcessError {
    pub(crate) fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }

    #[cfg(windows)]
    pub(crate) fn windows(operation: &'static str, source: windows::core::Error) -> Self {
        Self::Windows {
            operation,
            code: source.code().0.cast_unsigned(),
            message: source.to_string(),
        }
    }
}
