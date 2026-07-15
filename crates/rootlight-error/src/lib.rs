//! Stable, source-redacted errors for Rootlight's public boundaries.
//!
//! The full error envelope is implemented in TASK-01.3; this crate already
//! establishes the dependency boundary required by the workspace.

#![forbid(unsafe_code)]

/// Stable public error families shared by future clients and protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The caller supplied an invalid value.
    InvalidArgument,
    /// The requested contract major version is unsupported.
    ProtocolMismatch,
    /// The operation was cancelled before completion.
    Cancelled,
    /// An internal failure cannot be safely disclosed.
    Internal,
}
