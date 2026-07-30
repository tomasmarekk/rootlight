//! Explicit process-ownership primitives for Rootlight security boundaries.
//!
//! The native adapter facade stages an immutable executable, inherits only
//! explicit standard streams, and retains exact process-scope authority.

#![deny(unsafe_code)]

mod adapter;
mod error;
mod platform;
mod process;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use adapter::enter_isolated_adapter_launcher;
pub use adapter::{
    AdapterControl, AdapterControlEvidence, AdapterExecutableDigest, AdapterIsolationMechanism,
    AdapterIsolationPlatform, AdapterIsolationReport, AdapterMechanismEvidence,
    AdapterProcessCommand, AdapterSandboxLimits, AdapterStderr, AdapterStdin, AdapterStdout,
    AuthenticatedAdapterExecutable, IsolatedAdapterEntry, IsolatedAdapterProcess,
    MAX_ADAPTER_EXECUTABLE_BYTES, probe_windows_adapter_isolation, spawn_isolated_adapter,
};
pub use error::ProcessError;
pub use process::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, KillOnCloseJob, ProcessCommand, StdioMode,
};
