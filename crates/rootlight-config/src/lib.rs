//! Versioned, immutable configuration contracts for Rootlight.
//!
//! TASK-01.3 adds layered resolution, hard-denial precedence, canonical
//! snapshots, and redacted diagnostics.

#![forbid(unsafe_code)]

/// The initial production configuration contract version.
pub const CONFIG_VERSION: &str = "1.0";
