//! Typed failures for process creation and ownership.

use std::io;

#[cfg(windows)]
const HRESULT_NOT_ENOUGH_MEMORY: u32 = 0x8007_0008;
#[cfg(windows)]
const HRESULT_OUT_OF_MEMORY: u32 = 0x8007_000e;
#[cfg(windows)]
const HRESULT_NO_SYSTEM_RESOURCES: u32 = 0x8007_05aa;
#[cfg(windows)]
const HRESULT_COMMITMENT_LIMIT: u32 = 0x8007_05af;

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
    /// A bounded native process resource has no safe capacity for more work.
    #[error("process resource {resource} is temporarily unavailable")]
    ResourceUnavailable {
        /// Stable source-free resource label.
        resource: &'static str,
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

    /// Returns whether the operating system could not reserve resources needed
    /// to create or control the process.
    #[must_use]
    pub fn is_resource_unavailable(&self) -> bool {
        match self {
            Self::Io { source, .. } => source.kind() == io::ErrorKind::OutOfMemory,
            #[cfg(windows)]
            Self::Windows { code, .. } => {
                matches!(
                    *code,
                    HRESULT_NOT_ENOUGH_MEMORY
                        | HRESULT_OUT_OF_MEMORY
                        | HRESULT_NO_SYSTEM_RESOURCES
                        | HRESULT_COMMITMENT_LIMIT
                )
            }
            Self::ResourceUnavailable { .. } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessError;

    #[test]
    fn only_operating_system_resource_failures_are_retryable() {
        let out_of_memory = ProcessError::io(
            "create process",
            std::io::Error::from(std::io::ErrorKind::OutOfMemory),
        );
        assert!(out_of_memory.is_resource_unavailable());
        assert!(
            !ProcessError::io(
                "create process",
                std::io::Error::from(std::io::ErrorKind::PermissionDenied)
            )
            .is_resource_unavailable()
        );

        #[cfg(windows)]
        {
            assert!(
                ProcessError::Windows {
                    operation: "create suspended isolated adapter",
                    code: 0x8007_05aa,
                    message: "source-redacted fixture".to_owned(),
                }
                .is_resource_unavailable()
            );
            assert!(
                !ProcessError::Windows {
                    operation: "create suspended isolated adapter",
                    code: 0x8007_0005,
                    message: "source-redacted fixture".to_owned(),
                }
                .is_resource_unavailable()
            );
        }

        assert!(
            ProcessError::ResourceUnavailable {
                resource: "adapter_appcontainer_profiles",
            }
            .is_resource_unavailable()
        );
    }
}
