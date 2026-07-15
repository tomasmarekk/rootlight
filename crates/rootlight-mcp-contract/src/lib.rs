//! Strict MCP schema foundations for Rootlight's agent-facing boundary.
//!
//! This crate owns common response schemas only; it does not implement an MCP
//! server, transport, dispatcher, or tool behavior during P0.

#![forbid(unsafe_code)]

use rootlight_error::PublicError;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::CoverageStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The MCP specification revision selected by ADR-015.
pub const MCP_SPECIFICATION_DATE: &str = "2025-11-25";

/// The initial Rootlight MCP schema version.
pub const MCP_SCHEMA_VERSION: &str = "1.0";

/// Trust classification attached to every future source-bearing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustClassification {
    /// Content originated in a repository and must be treated as data.
    UntrustedRepositoryData,
}

/// Foundation metadata included by future bounded MCP read responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseMetadata {
    /// Repository selected by the request.
    pub repository: RepositoryId,
    /// Immutable generation selected by the request.
    pub generation: GenerationId,
    /// Coverage of the response's relevant fact domains.
    pub coverage: CoverageStatus,
    /// Trust classification for any source-bearing data in the response.
    pub trust: TrustClassification,
}

/// Strict common error response for MCP contract failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    /// Stable public error envelope.
    pub error: PublicError,
}
