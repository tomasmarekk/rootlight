//! Versioned envelopes for code, markup and authored document entities.
//! Retained tools keep their exact historical schemas; new revisions preserve
//! source kinds instead of coercing markup into programming-language symbols.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vertical::{
    AnalysisSchemaVersion, CodeLocateData, ContinuationCursor, CoverageSummary, GenerationSummary,
    RequiredNullable, ResolvedRepository, ResponseWarning, SymbolExplainData, UsageSummary,
};

/// Contract revision for source-kind locate and expert queries.
pub const QUERY_VERSION: &str = "1.4";
/// Contract revision for source-kind explanations.
pub const EXPLAIN_VERSION: &str = "1.5";
/// Contract revision for change results containing source entities.
pub const CHANGE_VERSION: &str = "1.5";

/// Exact version of source-kind explanation responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ExplainVersion {
    /// Explanation contract 1.2.
    #[serde(rename = "1.2")]
    V1_2,
}

/// Exact version of markup-aware change responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChangeVersion {
    /// Change contract 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Checked domain error under one exact source-entity contract revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityErrorResponse<V> {
    /// Exact error contract version.
    pub schema_version: V,
    /// Source-free error and remediation.
    pub error: crate::PublicError,
}

/// Success or checked domain error sharing one exact contract revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum EntityToolResponse<T, V> {
    /// Tool-specific successful result.
    Success(T),
    /// Checked source-redacted domain error.
    Error(EntityErrorResponse<V>),
}

/// Generation-bound read result carrying an exact source-entity version marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityReadEnvelope<T, V> {
    /// Exact response contract version.
    pub schema_version: V,
    /// Resolved repository.
    pub repository: ResolvedRepository,
    /// Pinned generation and freshness.
    pub generation: GenerationSummary,
    /// Relevant coverage.
    pub coverage: CoverageSummary,
    /// Tool-specific result.
    pub data: T,
    /// Whether a hard or requested limit stopped completion.
    pub truncated: bool,
    /// Authoritative completeness and safe continuation semantics.
    pub completeness: crate::completeness::ResultCompleteness,
    /// Safe continuation cursor for pageable results.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting.
    pub usage: UsageSummary,
    /// Source-free warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
    /// Classification for all repository-derived content.
    pub trust: crate::TrustClassification,
}

/// Source-kind `code.locate` success or domain error.
pub type CodeLocateOutputV1_1 = EntityToolResponse<
    EntityReadEnvelope<CodeLocateData, AnalysisSchemaVersion>,
    AnalysisSchemaVersion,
>;
/// Source-kind `symbol.explain` success or domain error.
pub type SymbolExplainOutputV1_2 =
    EntityToolResponse<EntityReadEnvelope<SymbolExplainData, ExplainVersion>, ExplainVersion>;
/// Source-kind `query.advanced` success or domain error.
pub type QueryAdvancedOutputV1_1 = EntityToolResponse<
    EntityReadEnvelope<crate::context::QueryAdvancedData, AnalysisSchemaVersion>,
    AnalysisSchemaVersion,
>;
/// Markup-aware `change.impact` success or domain error.
pub type ChangeImpactOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<crate::change::ChangeImpactData, ChangeVersion>,
    ChangeVersion,
>;
/// Markup-aware `history.compare` success or domain error.
pub type HistoryCompareOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<crate::change::HistoryCompareData, ChangeVersion>,
    ChangeVersion,
>;

/// Exact version of database-aware locate and advanced-query responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DatabaseQueryVersion {
    /// Query contract 1.2.
    #[serde(rename = "1.2")]
    V1_2,
}

/// Exact version of database-aware explanation responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DatabaseExplainVersion {
    /// Explanation contract 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Database-aware `code.locate` success or domain error.
pub type CodeLocateOutputV1_2 = EntityToolResponse<
    EntityReadEnvelope<CodeLocateData, DatabaseQueryVersion>,
    DatabaseQueryVersion,
>;
/// Database-aware `query.advanced` success or domain error.
pub type QueryAdvancedOutputV1_2 = EntityToolResponse<
    EntityReadEnvelope<crate::context::QueryAdvancedData, DatabaseQueryVersion>,
    DatabaseQueryVersion,
>;
/// Database-aware `symbol.explain` success or domain error.
pub type SymbolExplainOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<SymbolExplainData, DatabaseExplainVersion>,
    DatabaseExplainVersion,
>;

/// Exact version of event-, error- and modifier-aware queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DeclarationQueryVersion {
    /// Query contract 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Exact version of declaration-aware explanations and change results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DeclarationVersion {
    /// Explanation and change contract 1.4.
    #[serde(rename = "1.4")]
    V1_4,
}

/// Declaration-aware `code.locate` success or domain error.
pub type CodeLocateOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<CodeLocateData, DeclarationQueryVersion>,
    DeclarationQueryVersion,
>;
/// Declaration-aware `query.advanced` success or domain error.
pub type QueryAdvancedOutputV1_3 = EntityToolResponse<
    EntityReadEnvelope<crate::context::QueryAdvancedData, DeclarationQueryVersion>,
    DeclarationQueryVersion,
>;
/// Declaration-aware `symbol.explain` success or domain error.
pub type SymbolExplainOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<SymbolExplainData, DeclarationVersion>,
    DeclarationVersion,
>;
/// Declaration-aware `change.impact` success or domain error.
pub type ChangeImpactOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<crate::change::ChangeImpactData, DeclarationVersion>,
    DeclarationVersion,
>;
/// Declaration-aware `history.compare` success or domain error.
pub type HistoryCompareOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<crate::change::HistoryCompareData, DeclarationVersion>,
    DeclarationVersion,
>;

/// Exact version of document-aware locate and advanced-query responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DocumentQueryVersion {
    /// Query contract 1.4.
    #[serde(rename = "1.4")]
    V1_4,
}

/// Exact version of document-aware explanations and change results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DocumentVersion {
    /// Explanation and change contract 1.5.
    #[serde(rename = "1.5")]
    V1_5,
}

/// Document-aware `code.locate` success or domain error.
pub type CodeLocateOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<CodeLocateData, DocumentQueryVersion>,
    DocumentQueryVersion,
>;
/// Document-aware `query.advanced` success or domain error.
pub type QueryAdvancedOutputV1_4 = EntityToolResponse<
    EntityReadEnvelope<crate::context::QueryAdvancedData, DocumentQueryVersion>,
    DocumentQueryVersion,
>;
/// Document-aware `symbol.explain` success or domain error.
pub type SymbolExplainOutputV1_5 =
    EntityToolResponse<EntityReadEnvelope<SymbolExplainData, DocumentVersion>, DocumentVersion>;
/// Document-aware `change.impact` success or domain error.
pub type ChangeImpactOutputV1_5 = EntityToolResponse<
    EntityReadEnvelope<crate::change::ChangeImpactData, DocumentVersion>,
    DocumentVersion,
>;
/// Document-aware `history.compare` success or domain error.
pub type HistoryCompareOutputV1_5 = EntityToolResponse<
    EntityReadEnvelope<crate::change::HistoryCompareData, DocumentVersion>,
    DocumentVersion,
>;
