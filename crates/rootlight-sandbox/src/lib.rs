//! Explicit process-ownership primitives for Rootlight security boundaries.
//!
//! The process facade never inherits ambient standard handles on Windows.
//! Callers choose every child stream and retain exact process authority through
//! an owned handle.

#![deny(unsafe_code)]

mod adapter;
mod error;
mod platform;
mod process;

pub use adapter::{
    AdapterControl, AdapterControlEvidence, AdapterIsolationMechanism, AdapterIsolationReport,
    AdapterMechanismEvidence, AdapterProcessCommand, AdapterSandboxLimits, AdapterStderr,
    AdapterStdin, AdapterStdout, IsolatedAdapterProcess, probe_windows_adapter_isolation,
    spawn_windows_isolated_adapter,
};
pub use error::ProcessError;
pub use process::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, KillOnCloseJob, ProcessCommand, StdioMode,
};
