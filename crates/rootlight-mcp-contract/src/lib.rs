//! Strict MCP schema foundations for Rootlight's agent-facing boundary.

#![forbid(unsafe_code)]

pub mod accounting;
pub mod batch;
pub mod capability;
pub mod catalog;
pub mod change;
pub mod completeness;
pub mod context;
pub mod initialize;
pub mod intent;
pub mod json;
pub mod pagination;
pub mod repository;
pub mod vertical;

use std::collections::BTreeMap;

use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use rootlight_ir::CoverageStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use catalog::{ExposureProfile, McpTool};
pub use rootlight_error::{
    DetailKey, ErrorCode, NextAction, PublicError, PublicErrorBuildError, PublicValue, SafeLabel,
    error_definition,
};
pub use vertical::{
    CodeLocateInput, CodeLocateOutput, ContinuationCursor, GenerationSelector,
    OperationStatusInput, OperationStatusOutput, RepoIndexInput, RepoIndexOutput,
    RepositorySelector, SchemaVersion, SourceFreeMessage, SourceReadInput, SourceReadOutput,
    SymbolExplainInput, SymbolExplainOutputV1_1 as SymbolExplainOutput, ToolResponse, VerticalTool,
};

/// The MCP specification revision fixed by the compatibility contract.
pub const MCP_SPECIFICATION_DATE: &str = "2025-11-25";

/// The initial Rootlight MCP schema version.
pub const MCP_SCHEMA_VERSION: &str = "1.0";

/// The additive repository-operation tool schema version.
pub const MCP_OPERATION_SCHEMA_VERSION: &str = "1.3";

/// The additive operation-status tool schema version.
pub const MCP_OPERATION_STATUS_SCHEMA_VERSION: &str = "1.5";

/// The additive analysis-tool schema version.
pub const MCP_ANALYSIS_SCHEMA_VERSION: &str = "1.1";

/// The additive repository-status tool schema version.
pub const MCP_REPOSITORY_STATUS_SCHEMA_VERSION: &str = "1.2";

/// The repository catalog response schema version.
pub const REPO_LIST_SCHEMA_VERSION: &str = "2.0";

/// Remediation hint set exposed by current MCP contracts.
///
/// This MCP-owned schema type includes protocol-1.15 actions while retained
/// contracts continue deriving their exact historical schemas from
/// [`NextAction`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpNextAction {
    /// Correct one named input field.
    CorrectField {
        /// Stable name of the invalid field.
        field: DetailKey,
    },
    /// Retry after the bounded delay in the envelope.
    Retry,
    /// Select a compatible contract version.
    SelectSupportedVersion,
    /// Inspect the associated operation status.
    InspectOperation,
    /// Rebuild the affected repository generation.
    RebuildRepository,
    /// Collect a protected source-free support bundle.
    CollectSupportBundle,
    /// Restart enumeration from the beginning.
    RestartEnumeration,
    /// Update one checked configuration value.
    UpdateConfiguration {
        /// Canonical source-free configuration key.
        key: SafeLabel,
    },
    /// Delete an unneeded repository through catalog mutation.
    DeleteRepository,
}

/// Source-redacted failure envelope exposed by current MCP contracts.
///
/// Deserialization delegates invariant checking to [`PublicError`] before
/// materializing this schema-complete MCP representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpPublicError {
    code: ErrorCode,
    #[schemars(length(max = 1_024))]
    message: String,
    retryable: bool,
    #[schemars(range(max = 86_400_000))]
    retry_after_ms: Option<u64>,
    repository: Option<RepositoryId>,
    operation: Option<OperationId>,
    generation: Option<GenerationId>,
    #[schemars(length(max = 32))]
    details: BTreeMap<DetailKey, PublicValue>,
    #[schemars(length(max = 8))]
    next_actions: Vec<McpNextAction>,
}

impl McpPublicError {
    /// Returns the stable error family.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the source-free display template.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Reports whether an unchanged request may succeed when retried.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        self.retryable
    }

    /// Returns the optional bounded retry delay.
    #[must_use]
    pub const fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }

    /// Returns the associated repository identity, when present.
    #[must_use]
    pub const fn repository(&self) -> Option<RepositoryId> {
        self.repository
    }

    /// Returns the associated operation identity, when present.
    #[must_use]
    pub const fn operation(&self) -> Option<OperationId> {
        self.operation
    }

    /// Returns the associated generation identity, when present.
    #[must_use]
    pub const fn generation(&self) -> Option<GenerationId> {
        self.generation
    }

    /// Returns the bounded structured details.
    #[must_use]
    pub const fn details(&self) -> &BTreeMap<DetailKey, PublicValue> {
        &self.details
    }

    /// Returns the bounded remediation hints.
    #[must_use]
    pub fn next_actions(&self) -> &[McpNextAction] {
        &self.next_actions
    }
}

impl<'de> Deserialize<'de> for McpPublicError {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let error = PublicError::deserialize(deserializer)?;
        Self::try_from(&error).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<&PublicError> for McpPublicError {
    type Error = McpPublicErrorConversionError;

    fn try_from(error: &PublicError) -> Result<Self, Self::Error> {
        Ok(Self {
            code: error.code(),
            message: error.message().to_owned(),
            retryable: error.retryable(),
            retry_after_ms: error.retry_after_ms(),
            repository: error.repository(),
            operation: error.operation(),
            generation: error.generation(),
            details: error.details().clone(),
            next_actions: error
                .next_actions()
                .iter()
                .map(McpNextAction::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<&NextAction> for McpNextAction {
    type Error = McpPublicErrorConversionError;

    fn try_from(action: &NextAction) -> Result<Self, Self::Error> {
        match action {
            NextAction::CorrectField { field } => Ok(Self::CorrectField {
                field: field.clone(),
            }),
            NextAction::Retry => Ok(Self::Retry),
            NextAction::SelectSupportedVersion => Ok(Self::SelectSupportedVersion),
            NextAction::InspectOperation => Ok(Self::InspectOperation),
            NextAction::RebuildRepository => Ok(Self::RebuildRepository),
            NextAction::CollectSupportBundle => Ok(Self::CollectSupportBundle),
            NextAction::RestartEnumeration => Ok(Self::RestartEnumeration),
            NextAction::UpdateConfiguration { key } => {
                Ok(Self::UpdateConfiguration { key: key.clone() })
            }
            NextAction::DeleteRepository => Ok(Self::DeleteRepository),
            _ => Err(McpPublicErrorConversionError::UnsupportedNextAction),
        }
    }
}

/// Failure to represent a future shared action in the current MCP schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpPublicErrorConversionError {
    /// The shared error contains an action not yet admitted by the MCP contract.
    #[error("public error action is not supported by the MCP contract")]
    UnsupportedNextAction,
}

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
