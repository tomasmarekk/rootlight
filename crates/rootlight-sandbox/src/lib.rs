//! Explicit process-ownership primitives for Rootlight security boundaries.
//!
//! The process facade never inherits ambient standard handles on Windows.
//! Callers choose every child stream and retain exact process authority through
//! an owned handle.

#![deny(unsafe_code)]

mod error;
mod platform;
mod process;

pub use error::ProcessError;
pub use process::{
    ChildProcess, ChildStderr, ChildStdin, ChildStdout, KillOnCloseJob, ProcessCommand, StdioMode,
};
