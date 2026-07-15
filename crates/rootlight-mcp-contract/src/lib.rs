//! Strict MCP schema foundations for Rootlight's agent-facing boundary.
//!
//! This crate owns schemas only; it does not implement an MCP server, transport,
//! dispatcher, or tool behavior during P0.

#![forbid(unsafe_code)]

/// The MCP specification revision selected by ADR-015.
pub const MCP_SPECIFICATION_DATE: &str = "2025-11-25";

/// The initial Rootlight MCP schema version.
pub const MCP_SCHEMA_VERSION: &str = "1.0";
