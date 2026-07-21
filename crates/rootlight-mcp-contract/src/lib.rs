//! Strict MCP schema foundations for Rootlight's agent-facing boundary.

#![forbid(unsafe_code)]

pub mod accounting;
pub mod capability;
pub mod catalog;
pub mod change;
pub mod context;
pub mod intent;
pub mod pagination;
pub mod repository;
pub mod vertical;

use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::CoverageStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use catalog::{ExposureProfile, McpTool};
pub use rootlight_error::{
    DetailKey, ErrorCode, NextAction, PublicError, PublicErrorBuildError, SafeLabel,
};
pub use vertical::{
    CodeLocateInput, CodeLocateOutput, ContinuationCursor, GenerationSelector,
    OperationStatusInput, OperationStatusOutput, RepoIndexInput, RepoIndexOutput,
    RepositorySelector, SchemaVersion, SourceFreeMessage, SourceReadInput, SourceReadOutput,
    SymbolExplainInput, SymbolExplainOutput, ToolResponse, VerticalTool,
};

/// The MCP specification revision fixed by the compatibility contract.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    /// Tool error schema version.
    pub schema_version: vertical::SchemaVersion,
    /// Stable public error envelope.
    pub error: PublicError,
}
