//! Source definitions for the first MCP vertical-slice tool schemas.
//!
//! The schema generator derives checked public artifacts from these bounded
//! types; transport routing consumes only those generated artifacts.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_error::{PublicError, SafeLabel};
use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, OperationId, RepositoryId, SymbolId,
};
use rootlight_ir::{CoverageStatus, SourceRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ErrorResponse, McpPublicError, TrustClassification};

const MAX_SOURCE_FREE_MESSAGE_BYTES: usize = 1_024;
const MAX_CONTINUATION_CURSOR_BYTES: usize = 4_096;
const MAX_SOURCE_READ_BYTES: u64 = 524_288;

/// One tool exposed by the first secure MCP vertical slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerticalTool {
    /// Registers or rebuilds one repository.
    RepoIndex,
    /// Inspects repository state, generation, coverage, and operations.
    RepoStatus,
    /// Lists registered repositories.
    RepoList,
    /// Reads or cancels one operation.
    OperationStatus,
    /// Locates bounded structural or lexical matches.
    CodeLocate,
    /// Explains one or more stable symbols.
    SymbolExplain,
    /// Gets bounded typed relationships around symbols.
    SymbolRelationships,
    /// Traces bounded paths through relation graphs.
    FlowTrace,
    /// Maps changes to affected symbols, dependents, and risks.
    ChangeImpact,
    /// Ranks tests relevant to symbols or changes.
    TestsSelect,
    /// Produces a scoped architecture map.
    ArchitectureOverview,
    /// Finds dependency cycles in a relation projection.
    ArchitectureCycles,
    /// Reports bounded static reachability observations for human review.
    CodeDead,
    /// Compares two revisions or generations structurally.
    HistoryCompare,
    /// Produces an ordered change plan.
    PlanChange,
    /// Assembles task-specific evidence under a token budget.
    ContextPack,
    /// Reads generation-pinned source ranges.
    SourceRead,
    /// Executes a bounded expert query over the safe AST.
    QueryAdvanced,
    /// Executes up to sixteen read operations under one generation.
    QueryBatch,
}

impl VerticalTool {
    /// Complete deterministic first-slice tool catalog.
    pub const ALL: [Self; 19] = [
        Self::RepoIndex,
        Self::RepoStatus,
        Self::RepoList,
        Self::OperationStatus,
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SymbolRelationships,
        Self::FlowTrace,
        Self::ChangeImpact,
        Self::TestsSelect,
        Self::ArchitectureOverview,
        Self::ArchitectureCycles,
        Self::CodeDead,
        Self::HistoryCompare,
        Self::PlanChange,
        Self::ContextPack,
        Self::SourceRead,
        Self::QueryAdvanced,
        Self::QueryBatch,
    ];

    /// Stable tool name advertised through MCP.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RepoIndex => "repo.index",
            Self::RepoStatus => "repo.status",
            Self::RepoList => "repo.list",
            Self::OperationStatus => "operation.status",
            Self::CodeLocate => "code.locate",
            Self::SymbolExplain => "symbol.explain",
            Self::SymbolRelationships => "symbol.relationships",
            Self::FlowTrace => "flow.trace",
            Self::ChangeImpact => "change.impact",
            Self::TestsSelect => "tests.select",
            Self::ArchitectureOverview => "architecture.overview",
            Self::ArchitectureCycles => "architecture.cycles",
            Self::CodeDead => "code.dead",
            Self::HistoryCompare => "history.compare",
            Self::PlanChange => "plan.change",
            Self::ContextPack => "context.pack",
            Self::SourceRead => "source.read",
            Self::QueryAdvanced => "query.advanced",
            Self::QueryBatch => "query.batch",
        }
    }

    /// Public contract version advertised for this tool.
    #[must_use]
    pub const fn contract_version(self) -> &'static str {
        match self {
            Self::CodeLocate | Self::QueryAdvanced => crate::source_entity::QUERY_VERSION,
            Self::SymbolExplain => crate::source_entity::EXPLAIN_VERSION,
            Self::ChangeImpact | Self::HistoryCompare => crate::source_entity::CHANGE_VERSION,
            Self::RepoList => crate::REPO_LIST_SCHEMA_VERSION,
            Self::RepoIndex => crate::MCP_OPERATION_SCHEMA_VERSION,
            Self::OperationStatus => crate::MCP_OPERATION_STATUS_SCHEMA_VERSION,
            Self::RepoStatus => crate::MCP_REPOSITORY_STATUS_SCHEMA_VERSION,
            Self::SymbolRelationships
            | Self::TestsSelect
            | Self::ArchitectureOverview
            | Self::ArchitectureCycles
            | Self::CodeDead
            | Self::PlanChange
            | Self::ContextPack => crate::MCP_ANALYSIS_SCHEMA_VERSION,
            _ => crate::MCP_SCHEMA_VERSION,
        }
    }

    /// Checked JSON Schema 2020-12 input artifact for this tool.
    #[must_use]
    pub const fn input_schema_json(self) -> &'static str {
        match self {
            Self::RepoIndex => {
                include_str!("../../../schemas/generated/json/mcp-repo-index-input-1.3.schema.json")
            }
            Self::RepoStatus => include_str!(
                "../../../schemas/generated/json/mcp-repo-status-input-1.2.schema.json"
            ),
            Self::RepoList => {
                include_str!("../../../schemas/generated/json/mcp-repo-list-input-2.0.schema.json")
            }
            Self::OperationStatus => include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.6.schema.json"
            ),
            Self::CodeLocate => include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.4.schema.json"
            ),
            Self::SymbolExplain => include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.5.schema.json"
            ),
            Self::SymbolRelationships => include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-input-1.1.schema.json"
            ),
            Self::FlowTrace => {
                include_str!("../../../schemas/generated/json/mcp-flow-trace-input-1.0.schema.json")
            }
            Self::ChangeImpact => include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.5.schema.json"
            ),
            Self::TestsSelect => include_str!(
                "../../../schemas/generated/json/mcp-tests-select-input-1.1.schema.json"
            ),
            Self::ArchitectureOverview => include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-input-1.1.schema.json"
            ),
            Self::ArchitectureCycles => include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-input-1.1.schema.json"
            ),
            Self::CodeDead => {
                include_str!("../../../schemas/generated/json/mcp-code-dead-input-1.1.schema.json")
            }
            Self::HistoryCompare => include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.5.schema.json"
            ),
            Self::PlanChange => include_str!(
                "../../../schemas/generated/json/mcp-plan-change-input-1.1.schema.json"
            ),
            Self::ContextPack => include_str!(
                "../../../schemas/generated/json/mcp-context-pack-input-1.1.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-input-1.0.schema.json"
            ),
            Self::QueryAdvanced => include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.4.schema.json"
            ),
            Self::QueryBatch => include_str!(
                "../../../schemas/generated/json/mcp-query-batch-input-1.0.schema.json"
            ),
        }
    }

    /// Checked JSON Schema 2020-12 output artifact for this tool.
    #[must_use]
    pub const fn output_schema_json(self) -> &'static str {
        match self {
            Self::RepoIndex => include_str!(
                "../../../schemas/generated/json/mcp-repo-index-output-1.3.schema.json"
            ),
            Self::RepoStatus => include_str!(
                "../../../schemas/generated/json/mcp-repo-status-output-1.2.schema.json"
            ),
            Self::RepoList => {
                include_str!("../../../schemas/generated/json/mcp-repo-list-output-2.0.schema.json")
            }
            Self::OperationStatus => include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.6.schema.json"
            ),
            Self::CodeLocate => include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.4.schema.json"
            ),
            Self::SymbolExplain => include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.5.schema.json"
            ),
            Self::SymbolRelationships => include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-output-1.1.schema.json"
            ),
            Self::FlowTrace => include_str!(
                "../../../schemas/generated/json/mcp-flow-trace-output-1.0.schema.json"
            ),
            Self::ChangeImpact => include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.5.schema.json"
            ),
            Self::TestsSelect => include_str!(
                "../../../schemas/generated/json/mcp-tests-select-output-1.1.schema.json"
            ),
            Self::ArchitectureOverview => include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-output-1.1.schema.json"
            ),
            Self::ArchitectureCycles => include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-output-1.1.schema.json"
            ),
            Self::CodeDead => {
                include_str!("../../../schemas/generated/json/mcp-code-dead-output-1.1.schema.json")
            }
            Self::HistoryCompare => include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.5.schema.json"
            ),
            Self::PlanChange => include_str!(
                "../../../schemas/generated/json/mcp-plan-change-output-1.1.schema.json"
            ),
            Self::ContextPack => include_str!(
                "../../../schemas/generated/json/mcp-context-pack-output-1.1.schema.json"
            ),
            Self::SourceRead => include_str!(
                "../../../schemas/generated/json/mcp-source-read-output-1.0.schema.json"
            ),
            Self::QueryAdvanced => include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.4.schema.json"
            ),
            Self::QueryBatch => include_str!(
                "../../../schemas/generated/json/mcp-query-batch-output-1.0.schema.json"
            ),
        }
    }

    /// Previous additive-minor contract version retained for explicit callers.
    #[must_use]
    pub const fn previous_contract_version(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate | Self::QueryAdvanced => Some("1.3"),
            Self::SymbolExplain => Some("1.4"),
            Self::ChangeImpact | Self::HistoryCompare => Some("1.4"),
            Self::OperationStatus => Some("1.5"),
            Self::RepoIndex => Some("1.2"),
            Self::RepoStatus => Some("1.1"),
            Self::SymbolRelationships
            | Self::TestsSelect
            | Self::ArchitectureOverview
            | Self::ArchitectureCycles
            | Self::CodeDead
            | Self::PlanChange
            | Self::ContextPack => Some(crate::MCP_SCHEMA_VERSION),
            _ => None,
        }
    }

    /// Second retained additive-minor contract version, when required.
    #[must_use]
    pub const fn legacy_contract_version(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate | Self::QueryAdvanced => Some("1.2"),
            Self::SymbolExplain => Some("1.3"),
            Self::ChangeImpact | Self::HistoryCompare => Some("1.3"),
            Self::OperationStatus => Some("1.4"),
            Self::RepoIndex => Some("1.1"),
            Self::RepoStatus => Some(crate::MCP_SCHEMA_VERSION),
            _ => None,
        }
    }

    /// Third retained additive-minor contract version, when required.
    #[must_use]
    pub const fn second_legacy_contract_version(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate | Self::QueryAdvanced => Some("1.1"),
            Self::SymbolExplain => Some("1.2"),
            Self::ChangeImpact | Self::HistoryCompare => Some("1.2"),
            Self::OperationStatus => Some("1.3"),
            _ => None,
        }
    }

    /// Fourth retained additive-minor contract version, when required.
    #[must_use]
    pub const fn third_legacy_contract_version(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate | Self::QueryAdvanced => Some("1.0"),
            Self::SymbolExplain | Self::ChangeImpact | Self::HistoryCompare => Some("1.1"),
            Self::OperationStatus => Some("1.2"),
            _ => None,
        }
    }

    /// Fifth retained additive-minor contract version, when required.
    #[must_use]
    pub const fn fourth_legacy_contract_version(self) -> Option<&'static str> {
        match self {
            Self::SymbolExplain | Self::ChangeImpact | Self::HistoryCompare => Some("1.0"),
            Self::OperationStatus => Some("1.1"),
            _ => None,
        }
    }

    /// Initial additive-minor contract version retained for explicit callers.
    #[must_use]
    pub const fn initial_contract_version(self) -> Option<&'static str> {
        match self {
            Self::RepoIndex | Self::OperationStatus => Some(crate::MCP_SCHEMA_VERSION),
            _ => None,
        }
    }

    /// Previous additive-minor input schema retained for explicit callers.
    #[must_use]
    pub const fn previous_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.3.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.3.schema.json"
            )),
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-input-1.2.schema.json"
            )),
            Self::RepoStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-status-input-1.1.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.5.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.4.schema.json"
            )),
            Self::SymbolRelationships => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-input-1.0.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.4.schema.json"
            )),
            Self::TestsSelect => Some(include_str!(
                "../../../schemas/generated/json/mcp-tests-select-input-1.0.schema.json"
            )),
            Self::ArchitectureOverview => Some(include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-input-1.0.schema.json"
            )),
            Self::ArchitectureCycles => Some(include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-input-1.0.schema.json"
            )),
            Self::CodeDead => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-dead-input-1.0.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.4.schema.json"
            )),
            Self::PlanChange => Some(include_str!(
                "../../../schemas/generated/json/mcp-plan-change-input-1.0.schema.json"
            )),
            Self::ContextPack => Some(include_str!(
                "../../../schemas/generated/json/mcp-context-pack-input-1.0.schema.json"
            )),
            _ => None,
        }
    }

    /// Previous additive-minor output schema retained for explicit callers.
    #[must_use]
    pub const fn previous_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.3.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.3.schema.json"
            )),
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-output-1.2.schema.json"
            )),
            Self::RepoStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-status-output-1.1.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.5.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.4.schema.json"
            )),
            Self::SymbolRelationships => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-relationships-output-1.0.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.4.schema.json"
            )),
            Self::TestsSelect => Some(include_str!(
                "../../../schemas/generated/json/mcp-tests-select-output-1.0.schema.json"
            )),
            Self::ArchitectureOverview => Some(include_str!(
                "../../../schemas/generated/json/mcp-architecture-overview-output-1.0.schema.json"
            )),
            Self::ArchitectureCycles => Some(include_str!(
                "../../../schemas/generated/json/mcp-architecture-cycles-output-1.0.schema.json"
            )),
            Self::CodeDead => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-dead-output-1.0.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.4.schema.json"
            )),
            Self::PlanChange => Some(include_str!(
                "../../../schemas/generated/json/mcp-plan-change-output-1.0.schema.json"
            )),
            Self::ContextPack => Some(include_str!(
                "../../../schemas/generated/json/mcp-context-pack-output-1.0.schema.json"
            )),
            _ => None,
        }
    }

    /// Second retained additive-minor input schema, when required.
    #[must_use]
    pub const fn legacy_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.2.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.2.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.3.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.3.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.3.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.4.schema.json"
            )),
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-input-1.1.schema.json"
            )),
            Self::RepoStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-status-input-1.0.schema.json"
            )),
            _ => None,
        }
    }

    /// Second retained additive-minor output schema, when required.
    #[must_use]
    pub const fn legacy_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.2.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.2.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.3.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.3.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.3.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.4.schema.json"
            )),
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-output-1.1.schema.json"
            )),
            Self::RepoStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-status-output-1.0.schema.json"
            )),
            _ => None,
        }
    }

    /// Third retained additive-minor input schema, when required.
    #[must_use]
    pub const fn second_legacy_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.1.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.1.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.2.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.2.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.2.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.3.schema.json"
            )),
            _ => None,
        }
    }

    /// Third retained additive-minor output schema, when required.
    #[must_use]
    pub const fn second_legacy_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.1.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.1.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.2.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.2.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.2.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.3.schema.json"
            )),
            _ => None,
        }
    }

    /// Fourth retained additive-minor input schema, when required.
    #[must_use]
    pub const fn third_legacy_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-input-1.0.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-input-1.0.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.1.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.1.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.1.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.2.schema.json"
            )),
            _ => None,
        }
    }

    /// Fourth retained additive-minor output schema, when required.
    #[must_use]
    pub const fn third_legacy_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::CodeLocate => Some(include_str!(
                "../../../schemas/generated/json/mcp-code-locate-output-1.0.schema.json"
            )),
            Self::QueryAdvanced => Some(include_str!(
                "../../../schemas/generated/json/mcp-query-advanced-output-1.0.schema.json"
            )),
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.1.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.1.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.1.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.2.schema.json"
            )),
            _ => None,
        }
    }

    /// Fifth retained additive-minor input schema, when required.
    #[must_use]
    pub const fn fourth_legacy_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-input-1.0.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-input-1.0.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-input-1.0.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.1.schema.json"
            )),
            _ => None,
        }
    }

    /// Fifth retained additive-minor output schema, when required.
    #[must_use]
    pub const fn fourth_legacy_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::SymbolExplain => Some(include_str!(
                "../../../schemas/generated/json/mcp-symbol-explain-output-1.0.schema.json"
            )),
            Self::ChangeImpact => Some(include_str!(
                "../../../schemas/generated/json/mcp-change-impact-output-1.0.schema.json"
            )),
            Self::HistoryCompare => Some(include_str!(
                "../../../schemas/generated/json/mcp-history-compare-output-1.0.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.1.schema.json"
            )),
            _ => None,
        }
    }

    /// Initial additive-minor input schema, when required.
    #[must_use]
    pub const fn initial_input_schema_json(self) -> Option<&'static str> {
        match self {
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-input-1.0.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-input-1.0.schema.json"
            )),
            _ => None,
        }
    }

    /// Initial additive-minor output schema, when required.
    #[must_use]
    pub const fn initial_output_schema_json(self) -> Option<&'static str> {
        match self {
            Self::RepoIndex => Some(include_str!(
                "../../../schemas/generated/json/mcp-repo-index-output-1.0.schema.json"
            )),
            Self::OperationStatus => Some(include_str!(
                "../../../schemas/generated/json/mcp-operation-status-output-1.0.schema.json"
            )),
            _ => None,
        }
    }
}

/// Version marker carried by every first-slice response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SchemaVersion {
    /// Tool contract version 1.0.
    #[serde(rename = "1.0")]
    V1_0,
}

/// Version marker carried by additive repository-operation responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum OperationSchemaVersion {
    /// Tool contract version 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Version marker carried by repository-operation responses retained at 1.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationSchemaVersion")]
pub enum OperationSchemaVersionV1_2 {
    /// Tool contract version 1.2.
    #[serde(rename = "1.2")]
    V1_2,
}

/// Version marker carried by additive repository-operation responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationSchemaVersion")]
pub enum OperationSchemaVersionV1_1 {
    /// Tool contract version 1.1.
    #[serde(rename = "1.1")]
    V1_1,
}

/// Version marker carried by additive operation-status responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum OperationStatusSchemaVersion {
    /// Tool contract version 1.6.
    #[serde(rename = "1.6")]
    V1_6,
}

/// Version marker carried by operation-status responses retained at 1.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationStatusSchemaVersion")]
pub enum OperationStatusSchemaVersionV1_5 {
    /// Tool contract version 1.5.
    #[serde(rename = "1.5")]
    V1_5,
}

/// Version marker carried by operation-status responses retained at 1.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationStatusSchemaVersion")]
pub enum OperationStatusSchemaVersionV1_4 {
    /// Tool contract version 1.4.
    #[serde(rename = "1.4")]
    V1_4,
}

/// Version marker carried by operation-status responses retained at 1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationStatusSchemaVersion")]
pub enum OperationStatusSchemaVersionV1_3 {
    /// Tool contract version 1.3.
    #[serde(rename = "1.3")]
    V1_3,
}

/// Version marker carried by additive operation-status responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(rename = "OperationStatusSchemaVersion")]
pub enum OperationStatusSchemaVersionV1_2 {
    /// Tool contract version 1.2.
    #[serde(rename = "1.2")]
    V1_2,
}

/// Version marker carried by additive analysis responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AnalysisSchemaVersion {
    /// Tool contract version 1.1.
    #[serde(rename = "1.1")]
    V1_1,
}

/// Checked error response for analysis schema 1.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisErrorResponse {
    /// Tool error schema version.
    pub schema_version: AnalysisSchemaVersion,
    /// Stable source-redacted error.
    pub error: PublicError,
}

/// Success-or-error response for analysis schema 1.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AnalysisToolResponse<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(AnalysisErrorResponse),
}

/// Checked error response for repository-operation schema 1.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationErrorResponse")]
pub struct OperationErrorResponseV1_1 {
    /// Tool error schema version.
    pub schema_version: OperationSchemaVersionV1_1,
    /// Stable source-redacted error.
    pub error: PublicError,
}

/// Success-or-error response for repository-operation schema 1.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "OperationToolResponse")]
pub enum OperationToolResponseV1_1<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationErrorResponseV1_1),
}

/// Checked error response for repository-operation schema 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationErrorResponse {
    /// Tool error schema version.
    pub schema_version: OperationSchemaVersion,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for repository-operation schema 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OperationToolResponse<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationErrorResponse),
}

/// Checked error response for repository-operation schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationErrorResponse")]
pub struct OperationErrorResponseV1_2 {
    /// Tool error schema version.
    pub schema_version: OperationSchemaVersionV1_2,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for repository-operation schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "OperationToolResponse")]
pub enum OperationToolResponseV1_2<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationErrorResponseV1_2),
}

/// Checked error response for operation-status schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusErrorResponseV1_2 {
    /// Tool error schema version.
    pub schema_version: OperationStatusSchemaVersionV1_2,
    /// Stable source-redacted error.
    pub error: PublicError,
}

/// Success-or-error response for operation-status schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OperationStatusToolResponseV1_2<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationStatusErrorResponseV1_2),
}

/// A checked success-or-error result accepted by one tool output schema.
///
/// Successful variants preserve each tool's documented response shape.
/// Expected domain failures use the same versioned [`ErrorResponse`] and
/// checked [`PublicError`] contract for every tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolResponse<T> {
    /// A tool-specific successful response.
    Success(T),
    /// A versioned source-redacted domain error.
    Error(ErrorResponse),
}

/// A property that must be present and may contain JSON `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RequiredNullable<T>(pub Option<T>);

/// A bounded opaque continuation cursor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ContinuationCursor(#[schemars(length(min = 1, max = 4096))] String);

impl ContinuationCursor {
    /// Parses a nonempty cursor within the 4096-byte wire limit.
    ///
    /// # Errors
    ///
    /// Returns [`McpContractError::InvalidContinuationCursor`] when the value
    /// is empty or exceeds the byte limit.
    pub fn parse(value: &str) -> Result<Self, McpContractError> {
        if value.is_empty() || value.len() > MAX_CONTINUATION_CURSOR_BYTES {
            return Err(McpContractError::InvalidContinuationCursor);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the opaque cursor text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContinuationCursor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A checked Rootlight-generated message that cannot contain paths or source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct SourceFreeMessage(
    #[schemars(length(min = 1, max = 1024), regex(pattern = r"^[a-z0-9 -]+$"))] String,
);

impl SourceFreeMessage {
    /// Parses a bounded lowercase source-free message template.
    ///
    /// # Errors
    ///
    /// Returns [`McpContractError::InvalidSourceFreeMessage`] when the value
    /// is empty, oversized, or outside the safe character allow-list.
    pub fn parse(value: &str) -> Result<Self, McpContractError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_SOURCE_FREE_MESSAGE_BYTES
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b' ' | b'-')
            });
        if !valid {
            return Err(McpContractError::InvalidSourceFreeMessage);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the checked source-free message.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SourceFreeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Semantic validation failures in the MCP wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpContractError {
    /// A continuation cursor is empty or exceeds its byte ceiling.
    #[error("invalid continuation cursor")]
    InvalidContinuationCursor,
    /// A source-free message violates its bounded template policy.
    #[error("invalid source-free message")]
    InvalidSourceFreeMessage,
    /// A direct file range is inverted.
    #[error("invalid source file range")]
    InvalidFileRange,
    /// A source chunk does not match its reference, encoding, or range.
    #[error("invalid source chunk")]
    InvalidSourceChunk,
    /// Source chunk bytes do not match the declared aggregate.
    #[error("invalid source byte total")]
    InvalidSourceByteTotal,
}

/// Repository selector accepted by first-slice read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RepositorySelector {
    /// Select by stable repository identifier.
    ById(RepositoryIdSelector),
    /// Select by a configured local alias.
    #[schemars(skip)]
    ByAlias(RepositoryAliasSelector),
}

/// Stable repository-ID selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIdSelector {
    /// Repository identity.
    pub repository_id: RepositoryId,
}

/// Registered repository-alias selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAliasSelector {
    /// Registered alias, resolved to exactly one repository.
    #[schemars(length(min = 1, max = 256))]
    pub alias: String,
}

/// Generation selector shared by first-slice read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum GenerationSelector {
    /// Resolve the currently active immutable generation.
    Active(ActiveGeneration),
    /// Pin an explicit immutable generation.
    Explicit(GenerationId),
}

/// Active-generation keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActiveGeneration {
    /// Select the active generation.
    #[serde(rename = "active")]
    Active,
}

/// Requested response representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProfile {
    /// Smallest complete correctness-bearing response.
    Compact,
    /// More explanatory response within the same hard budgets.
    Standard,
    /// Maximum bounded provenance and evidence.
    Evidence,
}

/// Optional response limits that can only reduce server hard limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseBudget {
    /// Maximum returned result objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: Option<u16>,
    /// Maximum estimated output tokens.
    ///
    /// The schema ceiling is the largest aggregate budget any tool accepts
    /// (`query.batch` allows up to 16000); the server clamps each request to
    /// the calling tool's own hard limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 100, max = 16_000))]
    pub max_tokens: Option<u16>,
    /// Maximum source bytes before JSON escaping.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Maximum relationship or traversal facts examined.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100_000))]
    pub max_traversal_facts: Option<u32>,
    /// Maximum plan depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 16))]
    pub max_depth: Option<u8>,
    /// Maximum independently returned paths.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_paths: Option<u16>,
    /// Cooperative request deadline in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 10, max = 30_000))]
    pub timeout_ms: Option<u32>,
    /// Requested evidence detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_level: Option<ProvenanceLevel>,
}

/// Scope accepted by repository indexing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum IndexScope {
    /// Index the complete repository policy scope.
    Repository(RepositoryScope),
    /// Index selected repository-relative paths.
    Paths(PathScope),
    /// Index selected package identities.
    Packages(PackageScope),
    /// Index selected build-target identities without executing builds.
    BuildTargets(BuildTargetScope),
}

/// Whole-repository scope marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepositoryScope {
    /// Must be the repository scope keyword.
    pub repository: RepositoryScopeValue,
}

/// Whole-repository scope keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryScopeValue {
    /// Complete repository policy scope.
    Whole,
}

/// Repository-relative path scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathScope {
    /// Distinct repository-relative paths.
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 8192)))]
    pub paths: BTreeSet<String>,
}

/// Package scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PackageScope {
    /// Distinct package identities.
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 512)))]
    pub packages: BTreeSet<String>,
}

/// Build-target scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildTargetScope {
    /// Distinct build-target identities.
    #[schemars(length(min = 1, max = 256), inner(length(min = 1, max = 512)))]
    pub build_targets: BTreeSet<String>,
}

/// Indexing mode requested by `repo.index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexMode {
    /// Select the strongest available safe mode.
    Auto,
    /// Build the structural tier only.
    Structural,
    /// Request available deep tiers.
    Deep,
    /// Rebuild from a clean generation.
    Rebuild,
}

/// Requested or observed language support tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum AnalysisTier {
    /// Compiler-quality or equivalent semantic evidence.
    A,
    /// High-confidence partial semantic evidence.
    B,
    /// Generic structural extraction.
    C,
    /// Lexical-only support.
    D,
}

/// Strict input for `repo.index`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexInput {
    /// Canonicalizable local root for first registration.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub root: Option<String>,
    /// Existing repository identity to update.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub repository_id: Option<RepositoryId>,
    /// Optional indexing scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub scope: Option<IndexScope>,
    /// Requested indexing mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<IndexMode>,
    /// Per-language maximum requested tier.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub requested_tiers: Option<BTreeMap<String, AnalysisTier>>,
    /// Validated operation-scoped configuration override.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(skip)]
    pub configuration_patch: Option<BTreeMap<String, Value>>,
    /// Maximum time to wait for publication or a terminal state.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Requests permission to continue after client disconnect.
    ///
    /// When `true`, the daemon owns the durable operation independently of the
    /// requesting transport. Status and a successfully published generation
    /// remain recoverable after daemon restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detached: Option<bool>,
}

/// Summary of one admitted indexing plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexPlanSummary {
    /// Normalized scope class.
    pub scope: IndexPlanScope,
    /// Selected analysis mode.
    pub mode: IndexMode,
    /// Admitted providers in deterministic order.
    #[schemars(length(max = 64), inner(length(min = 1, max = 128)))]
    pub providers: Vec<String>,
    /// Parent generation, when an active generation existed.
    pub parent_generation: RequiredNullable<GenerationId>,
    /// Estimated staging and publication bytes.
    pub estimated_disk_bytes: u64,
}

/// Compact normalized scope class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexPlanScope {
    /// Whole repository.
    Repository,
    /// Selected paths.
    Paths,
    /// Selected packages.
    Packages,
    /// Selected build targets.
    BuildTargets,
}

/// Durable operation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    /// Accepted but not started.
    Queued,
    /// Actively running.
    Running,
    /// Published an immutable generation.
    Published,
    /// Failed without publishing partial state.
    Failed,
    /// Cancelled without publishing partial state.
    Cancelled,
    /// Waiting for explicit build or environment context.
    WaitingForContext,
}

/// Source-free diagnostic attached to an operational response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    /// Stable diagnostic code.
    pub code: SafeLabel,
    /// Static or Rootlight-generated source-free message.
    pub message: SourceFreeMessage,
}

/// `repo.index` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexDataV1_1 {
    /// Registered repository identity.
    pub repository_id: RepositoryId,
    /// Process-local operation identity.
    pub operation_id: OperationId,
    /// Separately journaled semantic refinement created by Auto mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Plan admitted by policy and resource checks.
    pub accepted_plan: IndexPlanSummary,
    /// Current operation state.
    pub state: OperationState,
    /// Generation published before the attached call returns, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Source-free validation and capability notes.
    #[schemars(length(max = 100))]
    pub diagnostics: Vec<Diagnostic>,
}

/// Strict output for `repo.index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexSuccessV1_1 {
    /// Tool response schema version.
    pub schema_version: OperationSchemaVersionV1_1,
    /// Operational result.
    pub data: RepoIndexDataV1_1,
}

/// Checked success-or-error output for `repo.index`.
pub type RepoIndexOutputV1_1 = OperationToolResponseV1_1<RepoIndexSuccessV1_1>;

/// Strict output for `repo.index` schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexSuccessV1_2 {
    /// Tool response schema version.
    pub schema_version: OperationSchemaVersionV1_2,
    /// Operational result.
    pub data: RepoIndexDataV1_1,
}

/// Checked success-or-error output for `repo.index` schema 1.2.
pub type RepoIndexOutputV1_2 = OperationToolResponseV1_2<RepoIndexSuccessV1_2>;

/// Strict output for `repo.index` schema 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexSuccessV1_3 {
    /// Tool response schema version.
    pub schema_version: OperationSchemaVersion,
    /// Operational result.
    pub data: RepoIndexDataV1_1,
}

/// Checked success-or-error output for `repo.index` schema 1.3.
pub type RepoIndexOutputV1_3 = OperationToolResponse<RepoIndexSuccessV1_3>;

/// `repo.index` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexData {
    /// Registered repository identity.
    pub repository_id: RepositoryId,
    /// Process-local operation identity.
    pub operation_id: OperationId,
    /// Plan admitted by policy and resource checks.
    pub accepted_plan: IndexPlanSummary,
    /// Current operation state.
    pub state: OperationState,
    /// Generation published before the attached call returns, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Source-free validation and capability notes.
    #[schemars(length(max = 100))]
    pub diagnostics: Vec<Diagnostic>,
}

/// Strict output for `repo.index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoIndexSuccess {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operational result.
    pub data: RepoIndexData,
}

/// Checked `repo.index` output retained for explicit 1.0 callers.
pub type RepoIndexOutputV1_0 = ToolResponse<RepoIndexSuccess>;

/// Current checked success-or-error output for `repo.index`.
pub type RepoIndexOutput = RepoIndexOutputV1_3;

/// Action accepted by `operation.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationAction {
    /// Read current state.
    Get,
    /// Request cooperative cancellation.
    Cancel,
}

/// Strict input for `operation.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusInput {
    /// Operation handle returned by the creating tool.
    pub operation_id: OperationId,
    /// Read or request cancellation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<OperationAction>,
    /// Maximum long-poll duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 30_000))]
    pub wait_ms: Option<u32>,
    /// Return immediately only after this journal revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_revision: Option<u64>,
}

/// Progress units reported by an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationProgress {
    /// Completed units.
    pub completed_units: u64,
    /// Known total units, when measurable.
    pub total_units: RequiredNullable<u64>,
}

/// Bounded resource counters for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationResourcesV1_1 {
    /// Peak resident bytes observed so far.
    pub peak_rss_bytes: u64,
    /// Durable bytes written so far.
    pub written_bytes: u64,
    /// Files examined so far.
    pub files_examined: u64,
    /// Source bytes examined so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_examined: Option<u64>,
    /// Existing artifact or generation bytes referenced by this operation.
    pub referenced_bytes: u64,
    /// Bytes confirmed by operation-owned durable writer boundaries.
    pub newly_written_bytes: u64,
    /// Conservative generation-memory reservation, separate from process RSS.
    pub reserved_memory_bytes: u64,
    /// Retained generation-memory charge, separate from process RSS.
    pub owned_memory_bytes: u64,
}

/// Construction strategy retained through operation-status schema 1.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "OperationBuildStrategy")]
pub enum OperationBuildStrategyV1_3 {
    /// No committed parent generation was available.
    Initial,
    /// Declared dependencies selected bounded artifact reuse.
    DependencyDirected,
    /// Missing dependency evidence required a repository-wide rebuild.
    ConservativeRepositoryRebuild,
    /// An identical retained generation was reactivated without rebuilding it.
    RetainedGeneration,
}

/// Construction strategy retained with a successful repository operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationBuildStrategy {
    /// No committed parent generation was available.
    Initial,
    /// Declared dependencies selected bounded artifact reuse.
    DependencyDirected,
    /// Missing dependency evidence required a repository-wide rebuild.
    ConservativeRepositoryRebuild,
    /// An identical retained generation was reactivated without rebuilding it.
    RetainedGeneration,
    /// The caller requested construction from a clean repository snapshot.
    CleanRebuild,
}

/// Source-free reason fine-grained invalidation expanded to a full rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationFallbackReason {
    /// A changed input had no declared dependent edge.
    MissingDependencyDeclaration,
    /// Fixed-point dependency traversal reached its configured work ceiling.
    ClosureWorkExceeded,
}

/// Fact domain named by one invalidation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationInvalidationFactDomain {
    /// Parsed syntax and syntax diagnostics.
    Syntax,
    /// Exported declarations, signatures, visibility, and imports.
    PublicSurface,
    /// Local implementation bodies and body-dependent occurrences.
    Body,
    /// Import, type, call, hierarchy, and candidate resolution.
    Resolution,
    /// Generation-aligned lexical and derived search facts.
    Search,
    /// Bounded derived graph projections and intent-plan aids.
    DerivedGraph,
    /// Test identities and explicit test relationships.
    Tests,
    /// Routes, RPC, messaging, database, and foreign service links.
    Services,
    /// Bounded Git, lineage, ownership, and co-change facts.
    History,
}

/// Semantic classification of one invalidation input transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationInvalidationChangeClass {
    /// No complete input value changed.
    NoChange,
    /// A stable input was newly added.
    Added,
    /// Only an implementation-body summary changed.
    BodyOnly,
    /// A declaration, import, route, test, or exported surface changed.
    Surface,
    /// Build targets, compiler options, or dependency context changed.
    BuildContext,
    /// Canonical path or containment semantics changed.
    Move,
    /// A stable input was removed.
    Delete,
    /// Rootlight analysis or derived-index configuration changed.
    Configuration,
    /// Grammar, adapter, or resolver producer version changed.
    ProviderChange,
}

/// Typed generation input named by an invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "subject", rename_all = "snake_case")]
pub enum OperationInvalidationInput {
    /// Actual bytes of one file.
    FileContent(String),
    /// Canonical path semantics of one file.
    FilePath(String),
    /// Exported surface of one analysis unit.
    PublicSurface(String),
    /// Body summary of one analysis unit.
    BodySummary(String),
    /// Import set of one analysis unit.
    ImportSet(String),
    /// Build-target identity and membership.
    BuildTarget(String),
    /// Compiler and macro option context.
    CompilerOptions(String),
    /// Dependency or lockfile resolution.
    DependencyVersion(String),
    /// Parser grammar identity.
    GrammarVersion(String),
    /// Adapter producer identity.
    AdapterVersion(String),
    /// Global resolver revision.
    ResolverVersion,
    /// Global analysis-configuration revision.
    ConfigurationRevision,
    /// Global search revision.
    SearchRevision,
    /// Derived plan or projection identity.
    DerivedPlan(String),
}

/// Scoped fact-domain target named by an invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationInvalidationFactTarget {
    /// Stable analysis-unit fact identity.
    pub unit: String,
    /// Invalidated fact domain.
    pub domain: OperationInvalidationFactDomain,
}

/// Source-free target named by one invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OperationInvalidationTarget {
    /// A changed generation input.
    Input(OperationInvalidationInput),
    /// A scoped fact-domain node.
    Fact(OperationInvalidationFactTarget),
    /// A reusable immutable artifact identity.
    Artifact(String),
    /// The complete invalidation plan.
    Plan,
}

/// Stable action recorded by one invalidation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationInvalidationAction {
    /// A complete generation input value changed.
    Changed,
    /// A fact node entered the fixed-point invalidation closure.
    Invalidated,
    /// An immutable artifact can be retained.
    Reused,
    /// An immutable artifact must be rebuilt.
    Rebuilt,
    /// Fine-grained reuse escalated to a repository rebuild.
    ConservativeFallback,
}

/// Stable source-free explanation for one invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OperationInvalidationReason {
    /// A complete input transitioned under this conservative class.
    InputTransition(OperationInvalidationChangeClass),
    /// A declared pass dependency propagated invalidation.
    DependencyPass(String),
    /// A changed input had no declared dependent edge.
    MissingDependencyDeclaration,
    /// The configured fixed-point edge-visit budget was exhausted.
    ClosureWorkExceeded,
    /// One artifact output node is invalidated.
    ArtifactOutputInvalidated,
    /// One artifact's complete dependency fingerprint changed.
    ArtifactDependencyChanged(OperationInvalidationInput),
    /// Every artifact dependency and output remains reusable.
    CompleteDependencyMatch,
    /// A conservative repository fallback rebuilds this target.
    ConservativeRepositoryRebuild(OperationFallbackReason),
}

/// One deterministic source-free invalidation decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationInvalidationTraceEntry {
    /// Input, fact node, artifact, or whole-plan target.
    pub target: OperationInvalidationTarget,
    /// Decision applied to the target.
    pub action: OperationInvalidationAction,
    /// Stable reason for the decision.
    pub reason: OperationInvalidationReason,
    /// Direct predecessor that propagated invalidation, when present.
    pub via: Option<OperationInvalidationTarget>,
}

/// Bounded prefix of the complete source-free invalidation trace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationInvalidationTraceV1_1 {
    /// Incremental trace schema version.
    #[schemars(length(min = 1, max = 32))]
    pub version: String,
    /// Canonically ordered trace decisions retained in this response.
    #[schemars(length(max = 8))]
    pub entries: Vec<OperationInvalidationTraceEntry>,
    /// Complete number of decisions in the durable trace.
    pub total_entries: u64,
    /// Whether every durable decision fits in this response.
    pub complete: bool,
}

/// Durable source-free invalidation, reuse, and rebuild evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationIncrementalEvidenceV1_1 {
    /// Construction strategy used by this operation.
    pub build_strategy: OperationBuildStrategyV1_3,
    /// Explicit reason dependency-directed reuse was abandoned, when applicable.
    pub fallback_reason: RequiredNullable<OperationFallbackReason>,
    /// Analysis units selected by the invalidation closure.
    pub invalidated_units: u64,
    /// Typed generation inputs changed from the parent snapshot.
    pub changed_inputs: u64,
    /// Authoritative file transitions observed by reconciliation.
    pub changed_files: u64,
    /// Source files whose immutable artifacts were reused.
    pub reused_files: u64,
    /// Source files lowered into fresh generation-bound facts.
    pub rebuilt_files: u64,
    /// Generation-bound normalized facts reused without rewriting.
    pub reused_facts: u64,
    /// Normalized facts rebuilt for the published generation.
    pub rebuilt_facts: u64,
    /// Bounded source-free explanation of invalidation and reuse decisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidation_trace: Option<OperationInvalidationTraceV1_1>,
}

/// One operation journal view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDetailV1_1 {
    /// Operation kind.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Durable state.
    pub state: OperationState,
    /// Current source-free stage name.
    #[schemars(length(min = 1, max = 128))]
    pub stage: String,
    /// Bounded progress counters.
    pub progress: OperationProgress,
    /// Monotonic journal revision.
    pub revision: u64,
    /// RFC 3339 UTC creation time.
    #[schemars(length(min = 20, max = 35))]
    pub started_at: String,
    /// Bounded resource summary.
    pub resources: OperationResourcesV1_1,
}

/// `operation.status` result data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusDataV1_1 {
    /// Current operation view.
    pub operation: OperationDetailV1_1,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidenceV1_1>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<PublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for `operation.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSuccessV1_1 {
    /// Tool response schema version.
    pub schema_version: OperationSchemaVersionV1_1,
    /// Operation result.
    pub data: OperationStatusDataV1_1,
}

/// Checked success-or-error output for `operation.status`.
pub type OperationStatusOutputV1_1 = OperationToolResponseV1_1<OperationStatusSuccessV1_1>;

/// Bounded resource counters for operation-status schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationResourcesV1_2 {
    /// Peak resident bytes observed so far.
    pub peak_rss_bytes: u64,
    /// Durable bytes written so far.
    pub written_bytes: u64,
    /// Files examined so far.
    pub files_examined: u64,
    /// Source bytes examined so far.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_examined: Option<u64>,
    /// Existing artifact or generation bytes referenced by this operation.
    pub referenced_bytes: u64,
    /// Bytes confirmed by operation-owned durable writer boundaries.
    pub newly_written_bytes: u64,
    /// Conservative generation-memory reservation, separate from process RSS.
    pub reserved_memory_bytes: u64,
    /// Retained generation-memory charge, separate from process RSS.
    pub owned_memory_bytes: u64,
    /// Durable bytes retained for the resulting immutable generation.
    pub retained_durable_bytes: u64,
}

/// One operation journal view for operation-status schema 1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDetailV1_2 {
    /// Operation kind.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Durable state.
    pub state: OperationState,
    /// Current source-free stage name.
    #[schemars(length(min = 1, max = 128))]
    pub stage: String,
    /// Bounded progress counters.
    pub progress: OperationProgress,
    /// Monotonic journal revision.
    pub revision: u64,
    /// RFC 3339 UTC creation time.
    #[schemars(length(min = 20, max = 35))]
    pub started_at: String,
    /// Bounded resource summary.
    pub resources: OperationResourcesV1_2,
}

/// `operation.status` result data for schema 1.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusDataV1_2 {
    /// Current operation view.
    pub operation: OperationDetailV1_2,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidenceV1_1>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<PublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for operation-status schema 1.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSuccessV1_2 {
    /// Tool response schema version.
    pub schema_version: OperationStatusSchemaVersionV1_2,
    /// Operation result.
    pub data: OperationStatusDataV1_2,
}

/// Checked `operation.status` output for schema 1.2.
pub type OperationStatusOutputV1_2 = OperationStatusToolResponseV1_2<OperationStatusSuccessV1_2>;

/// Whether one incremental fact scope was rebuilt or reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationFactWorkDisposition {
    /// The operation constructed fresh normalized records for this scope.
    Rebuild,
    /// Verified generation-neutral records were rebound without lowering again.
    Reuse,
}

/// Source-free reason attributed to one fact-work group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationFactWorkCause {
    /// No committed parent generation existed.
    InitialGeneration,
    /// The dependency closure selected this rebuild scope.
    DependencyClosure,
    /// The dependency graph proved this scope reusable.
    CompleteDependencyMatch,
    /// Missing dependency evidence required conservative rebuilding.
    ConservativeFallback,
    /// Generation ownership required fresh lowering.
    GenerationBoundLowering,
    /// Repository-wide resolution completed the records.
    Resolution,
    /// The caller explicitly requested a complete rebuild.
    UserRequestedCleanRebuild,
}

/// Logical domain selected by incremental planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationPlannedFactDomain {
    /// Parsed syntax facts.
    Syntax,
    /// Public symbol surface facts.
    PublicSurface,
    /// Function or method body facts.
    Body,
    /// Name and type resolution facts.
    Resolution,
    /// Search projection facts.
    Search,
    /// Derived graph facts.
    DerivedGraph,
    /// Test facts.
    Tests,
    /// Service and route facts.
    Services,
    /// History-derived facts.
    History,
}

/// Normalized IR record domain completed by incremental construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationNormalizedFactDomain {
    /// File records.
    Files,
    /// Entity records.
    Entities,
    /// Occurrence records.
    Occurrences,
    /// Relationship records.
    Relations,
    /// Provenance records.
    Provenance,
    /// Source-mapping records.
    SourceMappings,
    /// Diagnostic records.
    Diagnostics,
    /// Extension records.
    Extensions,
}

/// Exact count and bounded canonical sample of affected file identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationAffectedFileIds {
    /// Exact number of distinct affected files.
    pub total: u64,
    /// Canonical ascending identity sample.
    #[schemars(length(max = 4))]
    pub samples: Vec<FileId>,
    /// Whether the sample includes every affected file.
    pub complete: bool,
}

/// Exact count and bounded canonical sample of affected analysis units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationAffectedAnalysisUnitIds {
    /// Exact number of distinct affected analysis units.
    pub total: u64,
    /// Canonical ascending identity sample.
    #[schemars(length(max = 4))]
    pub samples: Vec<FactId>,
    /// Whether the sample includes every affected analysis unit.
    pub complete: bool,
}

/// One source-free logical work scope selected before fact construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationPlannedFactWorkGroup {
    /// Whether the planner selected rebuild or reuse.
    pub disposition: OperationFactWorkDisposition,
    /// Logical incremental fact domain.
    pub domain: OperationPlannedFactDomain,
    /// Source-free provider pass.
    #[schemars(length(min = 1, max = 128))]
    pub provider_pass: String,
    /// Dependency or construction cause.
    pub cause: OperationFactWorkCause,
    /// Exact count and bounded identities for affected files.
    pub affected_files: OperationAffectedFileIds,
    /// Exact count and bounded identities for affected analysis units.
    pub affected_analysis_units: OperationAffectedAnalysisUnitIds,
}

/// One normalized IR partition retained or rebuilt by the operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationNormalizedFactWorkGroup {
    /// Whether normalized records were rebuilt or reused.
    pub disposition: OperationFactWorkDisposition,
    /// Normalized IR record domain.
    pub domain: OperationNormalizedFactDomain,
    /// Source-free provider pass.
    #[schemars(length(min = 1, max = 128))]
    pub provider_pass: String,
    /// Construction or verified-reuse cause.
    pub cause: OperationFactWorkCause,
    /// Exact normalized record count.
    pub fact_count: u64,
    /// Exact count and bounded identities for affected files.
    pub affected_files: OperationAffectedFileIds,
    /// Exact count and bounded identities for affected analysis units.
    pub affected_analysis_units: OperationAffectedAnalysisUnitIds,
}

/// Bounded planned fact-work groups and collection completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationPlannedFactWorkCollection {
    /// Canonically ordered retained groups.
    #[schemars(length(max = 32))]
    pub groups: Vec<OperationPlannedFactWorkGroup>,
    /// Exact total number of groups before response bounding.
    pub total_groups: u64,
    /// Whether every group is retained.
    pub complete: bool,
}

/// Bounded normalized fact-work groups and collection completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationNormalizedFactWorkCollection {
    /// Canonically ordered retained groups.
    #[schemars(length(max = 32))]
    pub groups: Vec<OperationNormalizedFactWorkGroup>,
    /// Exact total number of groups before response bounding.
    pub total_groups: u64,
    /// Whether every group is retained.
    pub complete: bool,
}

/// Bounded grouped evidence for planned and normalized incremental fact work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationIncrementalFactWorkEvidence {
    /// Planned logical work groups.
    pub planned: OperationPlannedFactWorkCollection,
    /// Completed normalized record groups.
    pub normalized: OperationNormalizedFactWorkCollection,
}

/// Durable source-free invalidation and grouped fact-work evidence retained at schema 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationIncrementalEvidence")]
pub struct OperationIncrementalEvidenceV1_3 {
    /// Construction strategy used by this operation.
    pub build_strategy: OperationBuildStrategyV1_3,
    /// Explicit reason dependency-directed reuse was abandoned, when applicable.
    pub fallback_reason: RequiredNullable<OperationFallbackReason>,
    /// Analysis units selected by the invalidation closure.
    pub invalidated_units: u64,
    /// Typed generation inputs changed from the parent snapshot.
    pub changed_inputs: u64,
    /// Authoritative file transitions observed by reconciliation.
    pub changed_files: u64,
    /// Source files whose immutable artifacts were reused.
    pub reused_files: u64,
    /// Source files lowered into fresh generation-bound facts.
    pub rebuilt_files: u64,
    /// Generation-bound normalized facts reused without rewriting.
    pub reused_facts: u64,
    /// Normalized facts rebuilt for the published generation.
    pub rebuilt_facts: u64,
    /// Bounded source-free explanation of invalidation and reuse decisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidation_trace: Option<OperationInvalidationTraceV1_1>,
    /// Bounded grouped planned and completed normalized fact work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fact_work: Option<OperationIncrementalFactWorkEvidence>,
}

/// `operation.status` result data for schema 1.3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusData")]
pub struct OperationStatusDataV1_3 {
    /// Current operation view.
    pub operation: OperationDetailV1_2,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidenceV1_3>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<McpPublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for operation-status schema 1.3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusSuccess")]
pub struct OperationStatusSuccessV1_3 {
    /// Tool response schema version.
    pub schema_version: OperationStatusSchemaVersionV1_3,
    /// Operation result.
    pub data: OperationStatusDataV1_3,
}

/// Checked error response for operation-status schema 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusErrorResponse")]
pub struct OperationStatusErrorResponseV1_3 {
    /// Tool error schema version.
    pub schema_version: OperationStatusSchemaVersionV1_3,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for operation-status schema 1.3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "OperationStatusToolResponse")]
pub enum OperationStatusToolResponseV1_3<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationStatusErrorResponseV1_3),
}

/// Checked `operation.status` output retained for explicit 1.3 callers.
pub type OperationStatusOutputV1_3 = OperationStatusToolResponseV1_3<OperationStatusSuccessV1_3>;

/// Durable source-free invalidation and grouped fact-work evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationIncrementalEvidence {
    /// Construction strategy used by this operation.
    pub build_strategy: OperationBuildStrategy,
    /// Explicit reason dependency-directed reuse was abandoned, when applicable.
    pub fallback_reason: RequiredNullable<OperationFallbackReason>,
    /// Analysis units selected by the invalidation closure.
    pub invalidated_units: u64,
    /// Typed generation inputs changed from the parent snapshot.
    pub changed_inputs: u64,
    /// Authoritative file transitions observed by reconciliation.
    pub changed_files: u64,
    /// Source files whose immutable artifacts were reused.
    pub reused_files: u64,
    /// Source files lowered into fresh generation-bound facts.
    pub rebuilt_files: u64,
    /// Generation-bound normalized facts reused without rewriting.
    pub reused_facts: u64,
    /// Normalized facts rebuilt for the published generation.
    pub rebuilt_facts: u64,
    /// Bounded source-free explanation of invalidation and reuse decisions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidation_trace: Option<OperationInvalidationTraceV1_1>,
    /// Bounded grouped planned and completed normalized fact work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fact_work: Option<OperationIncrementalFactWorkEvidence>,
}

/// `operation.status` result data for schema 1.4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusData")]
pub struct OperationStatusDataV1_4 {
    /// Current operation view.
    pub operation: OperationDetailV1_2,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidence>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<McpPublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for operation-status schema 1.4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusSuccess")]
pub struct OperationStatusSuccessV1_4 {
    /// Tool response schema version.
    pub schema_version: OperationStatusSchemaVersionV1_4,
    /// Operation result.
    pub data: OperationStatusDataV1_4,
}

/// Checked error response for operation-status schema 1.4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusErrorResponse")]
pub struct OperationStatusErrorResponseV1_4 {
    /// Tool error schema version.
    pub schema_version: OperationStatusSchemaVersionV1_4,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for operation-status schema 1.4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "OperationStatusToolResponse")]
pub enum OperationStatusToolResponseV1_4<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationStatusErrorResponseV1_4),
}

/// Checked `operation.status` output retained for explicit 1.4 callers.
pub type OperationStatusOutputV1_4 = OperationStatusToolResponseV1_4<OperationStatusSuccessV1_4>;

/// One typed source-free dependency key selected before fact construction.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", content = "subject", rename_all = "snake_case")]
pub enum OperationPlanningDependencyKey {
    /// Actual bytes of one file.
    FileContent(FileId),
    /// Canonical path semantics of one file.
    FilePath(FileId),
    /// Exported surface of one analysis unit.
    PublicSurface(FactId),
    /// Body summary of one analysis unit.
    BodySummary(FactId),
    /// Import set of one analysis unit.
    ImportSet(FactId),
    /// Build-target membership.
    BuildTarget(FactId),
    /// Compiler and macro options.
    CompilerOptions(FactId),
    /// One dependency or lockfile resolution.
    DependencyVersion(FactId),
    /// Parser grammar identity.
    GrammarVersion(FactId),
    /// Adapter producer identity.
    AdapterVersion(FactId),
    /// Global resolver revision.
    ResolverVersion,
    /// Global analysis-configuration revision.
    ConfigurationRevision,
    /// Global search revision.
    SearchRevision,
    /// Derived plan or projection identity.
    DerivedPlan(FactId),
}

/// Bounded canonical identities for dependency keys that selected a closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationPlanningDependencyKeys {
    /// Exact number of selected dependency keys.
    pub total: u64,
    /// Canonical ascending identity sample.
    #[schemars(length(max = 32))]
    pub samples: Vec<OperationPlanningDependencyKey>,
    /// Whether the sample contains every selected key.
    pub complete: bool,
}

/// Source-free estimates selected before repository fact construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationPlanning {
    /// Construction strategy selected by incremental planning.
    pub build_strategy: OperationBuildStrategy,
    /// Explicit reason dependency-directed planning was abandoned.
    pub fallback_reason: RequiredNullable<OperationFallbackReason>,
    /// Conservative upper bound for analysis units selected by the closure.
    pub estimated_analysis_units: u64,
    /// Conservative upper bound for source files selected by the closure.
    pub estimated_files: u64,
    /// Conservative upper bound for normalized facts selected for rebuilding.
    pub estimated_facts: u64,
    /// Deterministic producer-defined upper bound for logical planning cost.
    pub estimated_cost_units: u64,
    /// Conservative upper bound for operation-owned durable bytes.
    pub estimated_durable_bytes: u64,
    /// Exact count and bounded identities for dependency keys selecting the closure.
    pub dependency_keys: OperationPlanningDependencyKeys,
}

/// `operation.status` result data for schema 1.5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusData")]
pub struct OperationStatusDataV1_5 {
    /// Current operation view.
    pub operation: OperationDetailV1_2,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Bounded source-free planning visible before terminal publication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planning: Option<OperationPlanning>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidence>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<McpPublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for operation-status schema 1.5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusSuccess")]
pub struct OperationStatusSuccessV1_5 {
    /// Tool response schema version.
    pub schema_version: OperationStatusSchemaVersionV1_5,
    /// Operation result.
    pub data: OperationStatusDataV1_5,
}

/// Checked error response for operation-status schema 1.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusErrorResponse")]
pub struct OperationStatusErrorResponseV1_5 {
    /// Tool error schema version.
    pub schema_version: OperationStatusSchemaVersionV1_5,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for operation-status schema 1.5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "OperationStatusToolResponse")]
pub enum OperationStatusToolResponseV1_5<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationStatusErrorResponseV1_5),
}

/// Checked `operation.status` output retained for explicit 1.5 callers.
pub type OperationStatusOutputV1_5 = OperationStatusToolResponseV1_5<OperationStatusSuccessV1_5>;

/// One operation journal view for operation-status schema 1.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDetailV1_6 {
    /// Operation kind.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Durable state.
    pub state: OperationState,
    /// Current source-free stage name.
    #[schemars(length(min = 1, max = 128))]
    pub stage: String,
    /// Bounded progress counters.
    pub progress: OperationProgress,
    /// Monotonic journal revision.
    pub revision: u64,
    /// RFC 3339 UTC creation time.
    #[schemars(length(min = 20, max = 35))]
    pub started_at: String,
    /// Durable absolute deadline in Unix milliseconds, when one exists.
    pub deadline_unix_ms: RequiredNullable<u64>,
    /// Bounded resource summary.
    pub resources: OperationResourcesV1_2,
}

/// `operation.status` result data for schema 1.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusData {
    /// Current operation view.
    pub operation: OperationDetailV1_6,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Separately journaled semantic refinement created by a successful Auto operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_operation_id: Option<OperationId>,
    /// Current source-free repository-index stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 128))]
    pub index_stage: Option<String>,
    /// Bounded source-free planning visible before terminal publication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planning: Option<OperationPlanning>,
    /// Final invalidation, reuse, and rebuild evidence, when published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incremental: Option<OperationIncrementalEvidence>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<McpPublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for operation-status schema 1.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSuccess {
    /// Tool response schema version.
    pub schema_version: OperationStatusSchemaVersion,
    /// Operation result.
    pub data: OperationStatusData,
}

/// Checked error response for operation-status schema 1.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusErrorResponse {
    /// Tool error schema version.
    pub schema_version: OperationStatusSchemaVersion,
    /// Stable source-redacted error including current remediation actions.
    pub error: McpPublicError,
}

/// Success-or-error response for operation-status schema 1.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OperationStatusToolResponse<T> {
    /// Tool-specific successful response.
    Success(T),
    /// Checked source-redacted domain error.
    Error(OperationStatusErrorResponse),
}

/// Bounded resource counters for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationResources {
    /// Peak resident bytes observed so far.
    pub peak_rss_bytes: u64,
    /// Durable bytes written so far.
    pub written_bytes: u64,
    /// Files examined so far.
    pub files_examined: u64,
}

/// One operation journal view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperationDetail {
    /// Operation kind.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Durable state.
    pub state: OperationState,
    /// Current source-free stage name.
    #[schemars(length(min = 1, max = 128))]
    pub stage: String,
    /// Bounded progress counters.
    pub progress: OperationProgress,
    /// Monotonic journal revision.
    pub revision: u64,
    /// RFC 3339 UTC creation time.
    #[schemars(length(min = 20, max = 35))]
    pub started_at: String,
    /// Bounded resource summary.
    pub resources: OperationResources,
}

/// `operation.status` result data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusData")]
pub struct OperationStatusDataV1_0 {
    /// Current operation view.
    pub operation: OperationDetail,
    /// Generation published by the operation, if any.
    pub published_generation: RequiredNullable<GenerationId>,
    /// Terminal public error, if any.
    pub error: RequiredNullable<PublicError>,
    /// Recommended delay before polling again.
    pub retry_after_ms: RequiredNullable<u32>,
}

/// Strict output for `operation.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OperationStatusSuccess")]
pub struct OperationStatusSuccessV1_0 {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Operation result.
    pub data: OperationStatusDataV1_0,
}

/// Checked `operation.status` output retained for explicit 1.0 callers.
pub type OperationStatusOutputV1_0 = ToolResponse<OperationStatusSuccessV1_0>;

/// Current checked success-or-error output for `operation.status`.
pub type OperationStatusOutput = OperationStatusToolResponse<OperationStatusSuccess>;

/// Common scope selector for read tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ScopeSelector {
    /// Limit to repository-relative paths.
    Paths(PathScope),
    /// Limit to packages.
    Packages(PackageScope),
    /// Limit to build targets.
    BuildTargets(BuildTargetScope),
    /// Limit to stable symbols.
    Symbols(SymbolScope),
}

/// Stable-symbol scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolScope {
    /// Distinct symbol identities.
    #[schemars(length(min = 1, max = 64))]
    pub symbols: BTreeSet<SymbolId>,
}

/// Entity classes supported by the first locate slice.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// Source file.
    File,
    /// Namespace or module.
    Module,
    /// Type declaration.
    Type,
    /// Function declaration.
    Function,
    /// Method declaration.
    Method,
    /// Field declaration.
    Field,
    /// Constant declaration.
    Constant,
    /// Variable declaration.
    Variable,
    /// Configuration record.
    Configuration,
    /// Service route or endpoint.
    Route,
    /// A symbol whose definition is outside the indexed repository.
    ExternalSymbol,
    /// Authored stylesheet rule.
    StyleRule,
    /// Authored stylesheet animation declaration.
    Keyframes,
    /// Written markup element, not a constructed DOM node.
    MarkupElement,
    /// Written markup attribute, not a runtime object property.
    MarkupAttribute,
    /// Authored database object, not an assertion about a live catalog.
    DatabaseObject,
    /// Declared source event, not a runtime emission.
    Event,
    /// Named source error declaration, not an analysis diagnostic.
    ErrorDeclaration,
    /// Declared callable modifier, not a visibility keyword.
    Modifier,
    /// Authored document heading or section, not a code module.
    DocumentSection,
    /// Authored named document link definition, not a runtime reference.
    LinkDefinition,
}

/// Locate retrieval mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Exact identifier match.
    Exact,
    /// Indexed lexical retrieval.
    Lexical,
    /// Structural filtering.
    Structural,
    /// Documentation retrieval.
    Docs,
    /// Repository-relative path retrieval.
    Path,
    /// Optional local semantic extension.
    Semantic,
}

/// Strict input for `code.locate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLocateInput {
    /// Repository to query.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Identifier, path, text, or concept query.
    #[schemars(length(min = 1, max = 2048))]
    pub query: String,
    /// Entity kinds to retain.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    pub kinds: Option<BTreeSet<EntityKind>>,
    /// Optional structural scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeSelector>,
    /// Language identity filters.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32), inner(length(min = 1, max = 64)))]
    pub languages: Option<BTreeSet<String>>,
    /// Retrieval modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 6))]
    pub search_modes: Option<BTreeSet<SearchMode>>,
    /// Structural seed symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 16))]
    pub related_to: Option<BTreeSet<SymbolId>>,
    /// Minimum relation confidence from 0 through 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 1000))]
    pub min_confidence: Option<u16>,
    /// Maximum returned matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: Option<u16>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Opaque generation-bound continuation cursor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ContinuationCursor>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Resolved repository metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRepository {
    /// Stable repository identity.
    pub repository_id: RepositoryId,
    /// Rootlight-owned display label.
    #[schemars(length(min = 1, max = 256))]
    pub display_name: String,
}

/// Freshness of one immutable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Matches the currently observed repository snapshot.
    Current,
    /// Queryable but a newer generation exists.
    Superseded,
    /// Queryable with a known stale source snapshot.
    Stale,
}

/// Generation metadata carried by each read response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GenerationSummary {
    /// Pinned generation.
    pub generation_id: GenerationId,
    /// Parent generation, if any.
    pub parent_generation: RequiredNullable<GenerationId>,
    /// Structural freshness.
    pub structural_freshness: Freshness,
    /// Semantic freshness.
    pub semantic_freshness: Freshness,
}

/// Per-language coverage summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LanguageCoverage {
    /// Language identity.
    #[schemars(length(min = 1, max = 64))]
    pub language: String,
    /// Observed support tier.
    pub tier: AnalysisTier,
    /// Coverage state for this language.
    pub status: CoverageStatus,
}

/// Coverage metadata relevant to one query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageSummary {
    /// Aggregate query-domain coverage.
    pub status: CoverageStatus,
    /// Deterministically ordered language coverage.
    #[schemars(length(max = 64))]
    pub languages: Vec<LanguageCoverage>,
    /// Inputs skipped by policy, limits, or capability.
    pub skipped_inputs: u64,
}

/// Cache classification for usage reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CacheStatus {
    /// No cache entry was used.
    Miss,
    /// A verified generation-bound cache entry was used.
    Hit,
    /// The operator does not use a cache.
    NotApplicable,
}

/// Bounded counters returned by each read tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageSummary {
    /// Storage rows examined.
    pub rows: u64,
    /// Relationship edges examined.
    pub edges: u64,
    /// Raw source bytes returned.
    pub source_bytes: u64,
    /// Encoded structured result bytes.
    pub json_bytes: u64,
    /// Deterministic token estimate.
    pub estimated_tokens: u64,
    /// Cooperative wall time in milliseconds.
    pub wall_time_ms: u64,
    /// Cache outcome.
    pub cache_status: CacheStatus,
    /// Source-free trace identity.
    #[schemars(length(min = 1, max = 128))]
    pub trace_id: String,
}

/// Source-free response warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResponseWarning {
    /// Stable warning code.
    pub code: SafeLabel,
    /// Rootlight-generated source-free explanation with optional bounded
    /// aggregate scope such as a canonical language and affected-file count.
    pub message: SourceFreeMessage,
}

/// Common strict response envelope for generation-pinned reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadEnvelope<T> {
    /// Tool response schema version.
    pub schema_version: SchemaVersion,
    /// Resolved repository.
    pub repository: ResolvedRepository,
    /// Pinned generation and freshness.
    pub generation: GenerationSummary,
    /// Relevant coverage.
    pub coverage: CoverageSummary,
    /// Tool-specific result.
    pub data: T,
    /// Whether any hard or requested limit stopped completion.
    pub truncated: bool,
    /// Authoritative execution completeness and safe continuation semantics.
    pub completeness: crate::completeness::ResultCompleteness,
    /// Safe continuation cursor, when the result is pageable.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting.
    pub usage: UsageSummary,
    /// Source-free warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
    /// Response-level classification for all repository-derived content.
    pub trust: TrustClassification,
}

/// Common strict response envelope for additive analysis-tool responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnalysisReadEnvelope<T> {
    /// Tool response schema version.
    pub schema_version: AnalysisSchemaVersion,
    /// Resolved repository.
    pub repository: ResolvedRepository,
    /// Pinned generation and freshness.
    pub generation: GenerationSummary,
    /// Relevant coverage.
    pub coverage: CoverageSummary,
    /// Tool-specific result.
    pub data: T,
    /// Whether any hard or requested limit stopped completion.
    pub truncated: bool,
    /// Authoritative execution completeness and safe continuation semantics.
    pub completeness: crate::completeness::ResultCompleteness,
    /// Safe continuation cursor, when the result is pageable.
    pub next_cursor: RequiredNullable<ContinuationCursor>,
    /// Runtime resource accounting.
    pub usage: UsageSummary,
    /// Source-free warnings.
    #[schemars(length(max = 100))]
    pub warnings: Vec<ResponseWarning>,
    /// Response-level classification for all repository-derived content.
    pub trust: TrustClassification,
}

/// Why a locate item ranked in the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocateReason {
    /// Exact identifier match.
    #[serde(rename = "identifier_match")]
    Identifier,
    /// Indexed lexical match.
    #[serde(rename = "lexical_match")]
    Lexical,
    /// Documentation match.
    #[serde(rename = "docs_match")]
    Docs,
    /// Path match.
    #[serde(rename = "path_match")]
    Path,
    /// Structural relation match.
    #[serde(rename = "structural_match")]
    Structural,
}

/// One bounded locate result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocatedItem {
    /// Stable symbol identity when the item is a symbol.
    pub symbol_id: Option<SymbolId>,
    /// Stable file identity when available.
    pub file_id: Option<FileId>,
    /// Entity kind.
    pub kind: EntityKind,
    /// Repository-controlled display name; always untrusted data.
    #[schemars(length(min = 1, max = 1024))]
    pub display_name: String,
    /// Compact repository-controlled signature; always untrusted data.
    #[schemars(length(max = 4096))]
    pub signature: Option<String>,
    /// Repository-relative display path; always untrusted data.
    #[schemars(length(min = 1, max = 8192))]
    pub path: String,
    /// Deterministic integer score from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub score: u16,
    /// Deterministically ordered ranking evidence.
    #[schemars(length(min = 1, max = 16))]
    pub why: Vec<LocateReason>,
    /// Exact source evidence, when available.
    pub source_ref: Option<SourceRef>,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// Server interpretation of a locate query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryInterpretation {
    /// Normalized query tokens.
    #[schemars(length(max = 128), inner(length(min = 1, max = 256)))]
    pub tokens: Vec<String>,
    /// Applied search modes.
    #[schemars(length(max = 6))]
    pub modes: BTreeSet<SearchMode>,
    /// Whether optional semantic retrieval was available.
    pub semantic_available: bool,
}

/// Tool suggested as a bounded next action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolSuggestion {
    /// Suggested first-slice read tool.
    pub tool: SuggestedTool,
    /// Stable symbols to carry forward.
    #[schemars(length(max = 16))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Exact source references to carry forward.
    #[schemars(length(max = 32))]
    pub source_refs: Vec<SourceRef>,
}

/// Read tools that may be suggested by the first slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SuggestedTool {
    /// Explain stable symbols.
    #[serde(rename = "symbol.explain")]
    SymbolExplain,
    /// Read exact source evidence.
    #[serde(rename = "source.read")]
    SourceRead,
    /// Refine a locate request.
    #[serde(rename = "code.locate")]
    CodeLocate,
}

/// `code.locate` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLocateData {
    /// Deterministically ranked matches.
    #[schemars(length(max = 200))]
    pub matches: Vec<LocatedItem>,
    /// Deterministic request interpretation.
    pub query_interpretation: QueryInterpretation,
    /// Bounded next-action suggestions.
    #[schemars(length(max = 16))]
    pub suggested_next: Vec<ToolSuggestion>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `code.locate`.
pub type CodeLocateOutput = ToolResponse<ReadEnvelope<CodeLocateData>>;

/// Sections accepted by `symbol.explain`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExplainSection {
    /// Compact signature.
    Signature,
    /// Documentation evidence.
    Docs,
    /// Containment evidence.
    Containment,
    /// Type evidence.
    Types,
    /// Outbound and inbound call summary.
    CallsSummary,
    /// Reference summary.
    ReferencesSummary,
    /// History evidence.
    History,
    /// Ownership evidence.
    Ownership,
    /// Diagnostics.
    Diagnostics,
    /// Small source preview.
    SourcePreview,
}

/// Provenance detail requested by a read tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceLevel {
    /// Omit provenance detail beyond mandatory source identity.
    None,
    /// Return compact provider and confidence evidence.
    Compact,
    /// Return maximum bounded provenance evidence.
    Full,
}

/// Strict input for `symbol.explain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplainInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Stable unambiguous symbols.
    #[schemars(length(min = 1, max = 16))]
    pub symbol_ids: BTreeSet<SymbolId>,
    /// Requested sections.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 10))]
    pub sections: Option<BTreeSet<ExplainSection>>,
    /// Per-relation evidence sample ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 25))]
    pub relation_sample_limit: Option<u8>,
    /// Source preview lines per symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 40))]
    pub source_preview_lines: Option<u8>,
    /// Provenance detail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_provenance: Option<ProvenanceLevel>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Compact relation counts for one symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelationSummary {
    /// Exact outbound calls.
    pub outbound_exact: u64,
    /// Candidate outbound calls.
    pub outbound_candidates: u64,
    /// Exact inbound calls.
    pub inbound_exact: u64,
    /// Candidate inbound calls.
    pub inbound_candidates: u64,
    /// Exact references.
    pub references_exact: u64,
}

/// Compact provenance item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSummary {
    /// Provider identity.
    #[schemars(length(min = 1, max = 128))]
    pub provider: String,
    /// Evidence class.
    #[schemars(length(min = 1, max = 128))]
    pub evidence: String,
    /// Confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Grammar, compiler, or frontend version when full provenance was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub frontend_version: Option<String>,
    /// Deterministic resolver rule when full provenance was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 256))]
    pub rule: Option<String>,
}

/// Compact provenance item retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceSummaryV1_0 {
    /// Provider identity.
    #[schemars(length(min = 1, max = 128))]
    pub provider: String,
    /// Evidence class.
    #[schemars(length(min = 1, max = 128))]
    pub evidence: String,
    /// Confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
}

/// One bounded typed relation sample for a symbol explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolRelationSample {
    /// Stable relation predicate label.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
    /// Direction relative to the explained symbol.
    #[schemars(length(min = 1, max = 16))]
    pub direction: String,
    /// Related symbol when the endpoint is an indexed entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SymbolId>,
    /// Direct immutable evidence for this relation.
    #[schemars(length(max = 8))]
    pub source_refs: Vec<SourceRef>,
    /// Fixed-point confidence from 0 through 1000.
    #[schemars(range(max = 1000))]
    pub confidence: u16,
}

/// One bounded symbol explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplanation {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind.
    pub kind: EntityKind,
    /// Repository-controlled display name.
    #[schemars(length(min = 1, max = 1024))]
    pub display_name: String,
    /// Repository-controlled qualified display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub qualified_name: Option<String>,
    /// Repository-controlled signature.
    #[schemars(length(max = 4096))]
    pub signature: Option<String>,
    /// Exact definition evidence.
    pub definition: SourceRef,
    /// Compact relation counts.
    pub relations: RelationSummary,
    /// Semantic container label when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 4096))]
    pub container: Option<String>,
    /// Bounded typed relation evidence requested by the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 250))]
    pub relation_samples: Vec<SymbolRelationSample>,
    /// Bounded untrusted source preview when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = 65_536))]
    pub source_preview: Option<String>,
    /// Bounded provenance.
    #[schemars(length(max = 64))]
    pub provenance: Vec<ProvenanceSummary>,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source-free uncertainty notes.
    #[schemars(length(max = 32))]
    pub uncertainty: Vec<ResponseWarning>,
    /// Explicit gaps for requested sections not supported by available evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = 10))]
    pub section_gaps: Vec<ResponseWarning>,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// Symbol explanation retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplanationV1_0 {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
    /// Entity kind.
    pub kind: EntityKind,
    /// Repository-controlled display name.
    #[schemars(length(min = 1, max = 1024))]
    pub display_name: String,
    /// Repository-controlled signature.
    #[schemars(length(max = 4096))]
    pub signature: Option<String>,
    /// Exact definition evidence.
    pub definition: SourceRef,
    /// Compact relation counts.
    pub relations: RelationSummary,
    /// Bounded provenance.
    #[schemars(length(max = 64))]
    pub provenance: Vec<ProvenanceSummaryV1_0>,
    /// Aggregate confidence from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    pub confidence: u16,
    /// Source-free uncertainty notes.
    #[schemars(length(max = 32))]
    pub uncertainty: Vec<ResponseWarning>,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

/// Reserved detail handle for a future pageable contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DetailHandle {
    /// Opaque generation-bound handle.
    #[schemars(length(min = 1, max = 4096))]
    pub handle: String,
    /// Detail class exposed by the handle.
    #[schemars(length(min = 1, max = 128))]
    pub kind: String,
}

/// `symbol.explain` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplainData {
    /// Explanations in request identity order.
    #[schemars(length(max = 16))]
    pub symbols: Vec<SymbolExplanation>,
    /// Requested identities absent from the pinned generation.
    #[schemars(length(max = 16))]
    pub unresolved_ids: Vec<SymbolId>,
    /// Reserved handles; empty while this tool uses explicit truncation.
    #[schemars(length(max = 64))]
    pub detail_handles: Vec<DetailHandle>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// `symbol.explain` result data retained for explicit 1.0 callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolExplainDataV1_0 {
    /// Explanations in request identity order.
    #[schemars(length(max = 16))]
    pub symbols: Vec<SymbolExplanationV1_0>,
    /// Requested identities absent from the pinned generation.
    #[schemars(length(max = 16))]
    pub unresolved_ids: Vec<SymbolId>,
    /// Reserved handles; empty while this tool uses explicit truncation.
    #[schemars(length(max = 64))]
    pub detail_handles: Vec<DetailHandle>,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

/// Checked success-or-error output for `symbol.explain`.
pub type SymbolExplainOutput = ToolResponse<ReadEnvelope<SymbolExplainData>>;

/// Checked `symbol.explain` output retained for explicit 1.0 callers.
pub type SymbolExplainOutputV1_0 = ToolResponse<ReadEnvelope<SymbolExplainDataV1_0>>;

/// Current checked `symbol.explain` output.
pub type SymbolExplainOutputV1_1 = AnalysisToolResponse<AnalysisReadEnvelope<SymbolExplainData>>;

/// One source selector accepted by `source.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SourceReadSelector {
    /// Select an exact source reference.
    Reference(SourceReferenceSelector),
    /// Select a symbol definition.
    Symbol(SymbolDefinitionSelector),
    /// Select a verified byte range in one indexed file.
    #[schemars(skip)]
    FileRange(FileRangeSelector),
}

/// Exact source-reference selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReferenceSelector {
    /// Generation-bound source reference.
    pub source_ref: SourceRef,
}

/// Symbol-definition selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymbolDefinitionSelector {
    /// Stable symbol identity.
    pub symbol_id: SymbolId,
}

/// Explicit indexed-file range selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileRangeSelector {
    /// Stable indexed file identity.
    pub file_id: FileId,
    /// Inclusive start byte.
    pub start_byte: u64,
    /// Exclusive end byte.
    pub end_byte: u64,
}

impl<'de> Deserialize<'de> for FileRangeSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireFileRangeSelector {
            file_id: FileId,
            start_byte: u64,
            end_byte: u64,
        }

        let wire = WireFileRangeSelector::deserialize(deserializer)?;
        if wire.start_byte > wire.end_byte {
            return Err(serde::de::Error::custom(McpContractError::InvalidFileRange));
        }
        Ok(Self {
            file_id: wire.file_id,
            start_byte: wire.start_byte,
            end_byte: wire.end_byte,
        })
    }
}

/// Requested source encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncodingRequest {
    /// Return exact UTF-8 when the complete verified file is valid UTF-8.
    Utf8LosslessWhenValid,
    /// Return explicit base64 for a small non-UTF-8 read.
    BytesBase64,
}

/// Strict input for `source.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadInput {
    /// Owning repository.
    pub repository: RepositorySelector,
    /// Immutable generation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<GenerationSelector>,
    /// Exact source selectors.
    #[schemars(length(min = 1, max = 32))]
    pub references: Vec<SourceReadSelector>,
    /// Leading context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_before: Option<u8>,
    /// Trailing context lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = 50))]
    pub context_lines_after: Option<u8>,
    /// Merge overlapping verified ranges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_overlaps: Option<bool>,
    /// Aggregate raw source-byte ceiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 524_288))]
    pub max_source_bytes: Option<u32>,
    /// Include one-based line numbers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_line_numbers: Option<bool>,
    /// Requested encoding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<SourceEncodingRequest>,
    /// Optional lower response limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<ResponseBudget>,
    /// Requested representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_profile: Option<ResponseProfile>,
    /// Return the bounded plan without executing retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<bool>,
}

/// Encoding used by one source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncoding {
    /// Exact UTF-8.
    Utf8,
    /// Base64-encoded exact bytes.
    Base64,
}

/// One exact verified source chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceChunk {
    /// Selected generation-bound reference.
    pub source_ref: SourceRef,
    /// Repository-relative display path.
    #[schemars(length(min = 1, max = 8192))]
    pub path: String,
    /// Inclusive byte start.
    pub start_byte: u64,
    /// Exclusive byte end.
    pub end_byte: u64,
    /// One-based first included line when line projection is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    /// One-based last included line when line projection is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    /// Exact UTF-8 or base64 text.
    #[schemars(length(max = 699_052))]
    pub content: String,
    /// Content representation.
    pub encoding: SourceEncoding,
    /// Complete-file content identity.
    pub content_hash: ContentHash,
    /// Indexed language identity.
    #[schemars(length(min = 1, max = 256))]
    pub language: String,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Mandatory repository-data trust marker.
    pub trust: TrustClassification,
}

impl SourceChunk {
    fn represented_source_bytes(&self) -> Result<u64, McpContractError> {
        represented_source_bytes(&self.content, self.encoding)
            .ok_or(McpContractError::InvalidSourceChunk)
    }
}

impl<'de> Deserialize<'de> for SourceChunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSourceChunk {
            source_ref: SourceRef,
            path: String,
            start_byte: u64,
            end_byte: u64,
            start_line: Option<u64>,
            end_line: Option<u64>,
            content: String,
            encoding: SourceEncoding,
            content_hash: ContentHash,
            language: String,
            generated: bool,
            trust: TrustClassification,
        }

        let wire = WireSourceChunk::deserialize(deserializer)?;
        let span = wire.source_ref.span();
        let represented_bytes = represented_source_bytes(&wire.content, wire.encoding)
            .ok_or_else(|| serde::de::Error::custom(McpContractError::InvalidSourceChunk))?;
        let span_bytes = wire
            .end_byte
            .checked_sub(wire.start_byte)
            .ok_or_else(|| serde::de::Error::custom(McpContractError::InvalidSourceChunk))?;
        let lines_are_valid = match (wire.start_line, wire.end_line) {
            (Some(start), Some(end)) => start > 0 && start <= end,
            (None, None) => true,
            _ => false,
        };
        let line_hint_matches = wire.source_ref.line_hint().is_none_or(|line_hint| {
            wire.start_line == Some(line_hint.start_line())
                && wire.end_line == Some(line_hint.end_line())
        });
        if !lines_are_valid
            || represented_bytes != span_bytes
            || span.start_byte() != wire.start_byte
            || span.end_byte() != wire.end_byte
            || wire.source_ref.content_hash() != wire.content_hash
            || !line_hint_matches
        {
            return Err(serde::de::Error::custom(
                McpContractError::InvalidSourceChunk,
            ));
        }

        Ok(Self {
            source_ref: wire.source_ref,
            path: wire.path,
            start_byte: wire.start_byte,
            end_byte: wire.end_byte,
            start_line: wire.start_line,
            end_line: wire.end_line,
            content: wire.content,
            encoding: wire.encoding,
            content_hash: wire.content_hash,
            language: wire.language,
            generated: wire.generated,
            trust: wire.trust,
        })
    }
}

fn represented_source_bytes(content: &str, encoding: SourceEncoding) -> Option<u64> {
    match encoding {
        SourceEncoding::Utf8 => u64::try_from(content.len()).ok(),
        SourceEncoding::Base64 => canonical_base64_decoded_len(content),
    }
}

fn canonical_base64_decoded_len(content: &str) -> Option<u64> {
    let bytes = content.as_bytes();
    if bytes.is_empty() {
        return Some(0);
    }
    if !bytes.len().is_multiple_of(4) {
        return None;
    }

    let padding = if bytes.ends_with(b"==") {
        2usize
    } else if bytes.ends_with(b"=") {
        1usize
    } else {
        0usize
    };
    let data_len = bytes.len().checked_sub(padding)?;
    if bytes[..data_len]
        .iter()
        .any(|byte| base64_value(*byte).is_none())
        || bytes[data_len..].iter().any(|byte| *byte != b'=')
    {
        return None;
    }

    if padding == 1 {
        let last = *bytes.get(data_len.checked_sub(1)?)?;
        if base64_value(last)? & 0b11 != 0 {
            return None;
        }
    } else if padding == 2 {
        let last = *bytes.get(data_len.checked_sub(1)?)?;
        if base64_value(last)? & 0b1111 != 0 {
            return None;
        }
    }

    let quartets = u64::try_from(bytes.len() / 4).ok()?;
    quartets
        .checked_mul(3)?
        .checked_sub(u64::try_from(padding).ok()?)
}

const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// A source selector that no longer resolves in the pinned snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StaleSourceReference {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free reason code.
    pub reason: SafeLabel,
}

/// One source-read elision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceElision {
    /// Zero-based request selector index.
    #[schemars(range(max = 31))]
    pub selector_index: u8,
    /// Source-free elision reason.
    pub reason: SafeLabel,
    /// Raw bytes omitted.
    pub omitted_bytes: u64,
}

/// `source.read` result data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceReadData {
    /// Verified chunks in request order.
    #[schemars(length(max = 32))]
    pub chunks: Vec<SourceChunk>,
    /// Selectors invalid for the pinned generation or source snapshot.
    #[schemars(length(max = 32))]
    pub stale_references: Vec<StaleSourceReference>,
    /// Merged, truncated, or unavailable ranges.
    #[schemars(length(max = 64))]
    pub elisions: Vec<SourceElision>,
    /// Raw bytes returned before JSON escaping.
    #[schemars(range(max = 524_288))]
    pub total_source_bytes: u32,
    /// Bounded source-free plan present when explain was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<crate::context::PlanExplanation>,
}

impl<'de> Deserialize<'de> for SourceReadData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireSourceReadData {
            chunks: Vec<SourceChunk>,
            stale_references: Vec<StaleSourceReference>,
            elisions: Vec<SourceElision>,
            total_source_bytes: u32,
            #[serde(default)]
            explanation: Option<crate::context::PlanExplanation>,
        }

        let wire = WireSourceReadData::deserialize(deserializer)?;
        let mut observed = 0u64;
        for chunk in &wire.chunks {
            observed = observed
                .checked_add(
                    chunk
                        .represented_source_bytes()
                        .map_err(serde::de::Error::custom)?,
                )
                .ok_or_else(|| {
                    serde::de::Error::custom(McpContractError::InvalidSourceByteTotal)
                })?;
        }
        if observed != u64::from(wire.total_source_bytes) || observed > MAX_SOURCE_READ_BYTES {
            return Err(serde::de::Error::custom(
                McpContractError::InvalidSourceByteTotal,
            ));
        }

        Ok(Self {
            chunks: wire.chunks,
            stale_references: wire.stale_references,
            elisions: wire.elisions,
            total_source_bytes: wire.total_source_bytes,
            explanation: wire.explanation,
        })
    }
}

/// Checked success-or-error output for `source.read`.
pub type SourceReadOutput = ToolResponse<ReadEnvelope<SourceReadData>>;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::{
        CodeLocateInput, CodeLocateOutput, OperationStatusInput, OperationStatusOutput,
        OperationStatusOutputV1_0, OperationStatusOutputV1_1, OperationStatusOutputV1_2,
        OperationStatusOutputV1_3, OperationStatusOutputV1_4, OperationStatusOutputV1_5,
        RepoIndexInput, RepoIndexOutput, RepoIndexOutputV1_0, RepoIndexOutputV1_1,
        RepoIndexOutputV1_2, SourceReadInput, SourceReadOutput, SymbolExplainInput,
        SymbolExplainOutputV1_0, SymbolExplainOutputV1_1, VerticalTool,
    };
    use crate::change::{
        ChangeImpactInput, ChangeImpactOutputV1_0, ChangeImpactOutputV1_1, HistoryCompareInputV1_0,
        HistoryCompareOutputV1_0, PlanChangeInputV1_0, PlanChangeOutputV1_0, TestsSelectInput,
        TestsSelectOutputV1_0, TestsSelectOutputV1_1,
    };
    use crate::context::{
        ContextPackInput, ContextPackOutput, QueryAdvancedInput, QueryAdvancedOutput,
        QueryBatchInput, QueryBatchOutput,
    };
    use crate::intent::{
        ArchitectureCyclesInput, ArchitectureCyclesOutputV1_0, ArchitectureOverviewInput,
        ArchitectureOverviewOutputV1_0, CodeDeadInput, CodeDeadOutputV1_0, FlowTraceInput,
        FlowTraceOutput, SymbolRelationshipsInput, SymbolRelationshipsOutputV1_0,
    };
    use crate::repository::{
        RepoListInput, RepoListOutput, RepoStatusInput, RepoStatusOutput, RepoStatusOutputV1_0,
        RepoStatusOutputV1_1,
    };

    #[test]
    fn embedded_vertical_schemas_are_unique_strict_draft_2020_12_objects() {
        let mut names = BTreeSet::new();
        let mut identifiers = BTreeSet::new();

        for tool in VerticalTool::ALL {
            assert!(names.insert(tool.name()));
            for schema_text in [tool.input_schema_json(), tool.output_schema_json()] {
                let schema: Value =
                    serde_json::from_str(schema_text).expect("checked schema is valid JSON");
                jsonschema::draft202012::new(&schema)
                    .expect("checked schema compiles as JSON Schema 2020-12");
                assert_eq!(
                    schema["$schema"],
                    "https://json-schema.org/draft/2020-12/schema"
                );
                assert_eq!(schema["type"], "object");
                assert!(
                    schema["additionalProperties"] == false
                        || schema["unevaluatedProperties"] == false
                );
                assert_every_object_declares_additional_properties(&schema);
                let identifier = schema["$id"]
                    .as_str()
                    .expect("tool schema has a stable identifier")
                    .to_owned();
                assert!(identifiers.insert(identifier));
            }
        }
    }

    fn assert_every_object_declares_additional_properties(value: &Value) {
        match value {
            Value::Array(values) => {
                for value in values {
                    assert_every_object_declares_additional_properties(value);
                }
            }
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("object") {
                    assert!(
                        object.contains_key("additionalProperties")
                            || object.contains_key("unevaluatedProperties"),
                        "object schema is missing a closed-properties contract: {object:?}"
                    );
                }
                for value in object.values() {
                    assert_every_object_declares_additional_properties(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    #[test]
    fn repo_index_schema_advertises_only_executable_registration_fields() {
        let schema: Value = serde_json::from_str(VerticalTool::RepoIndex.input_schema_json())
            .expect("checked schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("checked schema compiles");
        assert!(validator.is_valid(&json!({
            "root": "C:/fixture",
            "mode": "deep",
            "wait_ms": 1000,
            "detached": true
        })));
        assert!(validator.is_valid(&json!({
            "root": "C:/fixture",
            "mode": "rebuild"
        })));
        assert!(!validator.is_valid(&json!({})));
        assert!(!validator.is_valid(&json!({
            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        })));
        for unsupported in [
            json!({"root": "C:/fixture", "scope": {"repository": "whole"}}),
            json!({"root": "C:/fixture", "requested_tiers": {"rust": "A"}}),
            json!({"root": "C:/fixture", "configuration_patch": {}}),
        ] {
            assert!(!validator.is_valid(&unsupported));
        }
        assert!(!validator.is_valid(&json!({"root": null})));
    }

    #[test]
    fn clean_rebuild_values_exist_only_in_current_contracts() {
        let rebuild_input = json!({"root": "C:/fixture", "mode": "rebuild"});
        let current_repo: Value = serde_json::from_str(VerticalTool::RepoIndex.input_schema_json())
            .expect("current repo.index schema is valid JSON");
        assert!(
            jsonschema::draft202012::new(&current_repo)
                .expect("current repo.index schema compiles")
                .is_valid(&rebuild_input)
        );
        for schema in [
            VerticalTool::RepoIndex
                .previous_input_schema_json()
                .expect("repo.index retains schema 1.2"),
            VerticalTool::RepoIndex
                .legacy_input_schema_json()
                .expect("repo.index retains schema 1.1"),
            VerticalTool::RepoIndex
                .initial_input_schema_json()
                .expect("repo.index retains schema 1.0"),
        ] {
            let schema: Value =
                serde_json::from_str(schema).expect("retained repo.index schema is valid JSON");
            assert!(
                !jsonschema::draft202012::new(&schema)
                    .expect("retained repo.index schema compiles")
                    .is_valid(&rebuild_input)
            );
        }

        let current_status: Value =
            serde_json::from_str(VerticalTool::OperationStatus.output_schema_json())
                .expect("current operation.status schema is valid JSON");
        assert!(schema_enum_contains(
            &current_status,
            "OperationBuildStrategy",
            "clean_rebuild"
        ));
        let previous_status: Value = serde_json::from_str(
            VerticalTool::OperationStatus
                .previous_output_schema_json()
                .expect("operation.status retains schema 1.5"),
        )
        .expect("retained operation.status 1.5 schema is valid");
        assert!(schema_enum_contains(
            &previous_status,
            "OperationBuildStrategy",
            "clean_rebuild"
        ));
        let legacy_status: Value = serde_json::from_str(
            VerticalTool::OperationStatus
                .legacy_output_schema_json()
                .expect("operation.status retains schema 1.4"),
        )
        .expect("retained operation.status 1.4 schema is valid");
        assert!(schema_enum_contains(
            &legacy_status,
            "OperationBuildStrategy",
            "clean_rebuild"
        ));
        for schema in [
            VerticalTool::OperationStatus
                .second_legacy_output_schema_json()
                .expect("operation.status retains schema 1.3"),
            VerticalTool::OperationStatus
                .third_legacy_output_schema_json()
                .expect("operation.status retains schema 1.2"),
            VerticalTool::OperationStatus
                .fourth_legacy_output_schema_json()
                .expect("operation.status retains schema 1.1"),
            VerticalTool::OperationStatus
                .initial_output_schema_json()
                .expect("operation.status retains schema 1.0"),
        ] {
            let schema: Value =
                serde_json::from_str(schema).expect("retained operation.status schema is valid");
            assert!(!schema_enum_contains(
                &schema,
                "OperationBuildStrategy",
                "clean_rebuild"
            ));
        }
    }

    #[test]
    fn operation_deadline_exists_only_in_the_current_contract() {
        let current = json!({
            "schema_version": "1.6",
            "data": {
                "operation": {
                    "kind": "recovery",
                    "state": "running",
                    "stage": "active_generation",
                    "progress": {
                        "completed_units": 1,
                        "total_units": 2
                    },
                    "revision": 7,
                    "started_at": "2026-07-18T00:00:00Z",
                    "deadline_unix_ms": 1800000000000u64,
                    "resources": {
                        "peak_rss_bytes": 1024,
                        "written_bytes": 0,
                        "files_examined": 3,
                        "bytes_examined": 2048,
                        "referenced_bytes": 0,
                        "newly_written_bytes": 0,
                        "reserved_memory_bytes": 0,
                        "owned_memory_bytes": 0,
                        "retained_durable_bytes": 0
                    }
                },
                "published_generation": null,
                "error": null,
                "retry_after_ms": 1000
            }
        });
        serde_json::from_value::<OperationStatusOutput>(current.clone())
            .expect("current operation deadline decodes");
        assert_schema_fixture(
            VerticalTool::OperationStatus.output_schema_json(),
            &current,
            "operation.status 1.6 deadline",
        );

        let mut missing = current.clone();
        missing["data"]["operation"]
            .as_object_mut()
            .expect("operation is an object")
            .remove("deadline_unix_ms");
        let current_schema: Value =
            serde_json::from_str(VerticalTool::OperationStatus.output_schema_json())
                .expect("current operation schema is valid");
        assert!(
            !jsonschema::draft202012::new(&current_schema)
                .expect("current operation schema compiles")
                .is_valid(&missing)
        );

        let mut retained = current;
        retained["schema_version"] = json!("1.5");
        retained["data"]["operation"]
            .as_object_mut()
            .expect("operation is an object")
            .remove("deadline_unix_ms");
        serde_json::from_value::<OperationStatusOutputV1_5>(retained.clone())
            .expect("retained operation without deadline decodes");
        retained["data"]["operation"]["deadline_unix_ms"] = json!(1800000000000u64);
        assert!(serde_json::from_value::<OperationStatusOutputV1_5>(retained).is_err());
    }

    fn schema_enum_contains(schema: &Value, definition: &str, expected: &str) -> bool {
        schema["$defs"][definition]["oneOf"]
            .as_array()
            .is_some_and(|variants| {
                variants
                    .iter()
                    .any(|variant| variant["const"].as_str() == Some(expected))
            })
    }

    #[test]
    fn source_read_schema_advertises_exact_and_executable_symbol_selectors_only() {
        let schema: Value = serde_json::from_str(VerticalTool::SourceRead.input_schema_json())
            .expect("checked schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("checked schema compiles");
        let exact = retained_tool_input("source.read");
        assert!(validator.is_valid(&exact));

        let repository = exact["repository"].clone();
        let symbol = retained_tool_input("symbol.explain")["symbol_ids"][0].clone();
        assert!(validator.is_valid(&json!({
            "repository": repository,
            "references": [{"symbol_id": symbol}]
        })));
        assert!(!validator.is_valid(&json!({
            "repository": {"alias": "fixture"},
            "references": exact["references"].clone()
        })));
        assert!(!validator.is_valid(&json!({
            "repository": exact["repository"].clone(),
            "references": [{
                "file_id": "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq",
                "start_byte": 0,
                "end_byte": 10
            }]
        })));
    }

    #[test]
    fn no_tool_input_schema_advertises_an_unserved_repository_alias() {
        for tool in VerticalTool::ALL {
            assert!(
                !tool.input_schema_json().contains("RepositoryAliasSelector"),
                "{} must not advertise repository aliases",
                tool.name()
            );
        }
    }

    #[test]
    fn optional_input_fields_reject_explicit_null_and_unknown_properties() {
        let schema: Value = serde_json::from_str(VerticalTool::CodeLocate.input_schema_json())
            .expect("checked schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("checked schema compiles");
        let required = json!({
            "repository": {
                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
            },
            "query": "publish"
        });
        assert!(validator.is_valid(&required));

        let mut explicit_null = required.clone();
        explicit_null["max_results"] = Value::Null;
        assert!(!validator.is_valid(&explicit_null));

        let mut unknown = required;
        unknown["host_path"] = json!("must not be accepted");
        assert!(!validator.is_valid(&unknown));
    }

    #[test]
    fn retained_tool_contracts_round_trip_through_rust_and_json_schema() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        let tools = fixture["tools"]
            .as_array()
            .expect("retained tool contracts contain a tool array");
        assert_eq!(
            tools.len(),
            VerticalTool::ALL.len(),
            "fixture must retain one example for every public tool"
        );
        let mut retained = BTreeSet::new();

        for fixture in tools {
            let name = fixture["tool"].as_str().expect("tool name is a string");
            assert!(retained.insert(name), "duplicate retained example: {name}");
            let input = fixture["input"].clone();
            let output = fixture["output"].clone();
            match name {
                "repo.index" => {
                    assert_round_trip_with_schema::<RepoIndexInput>(
                        VerticalTool::RepoIndex,
                        &input,
                        VerticalTool::RepoIndex
                            .initial_input_schema_json()
                            .expect("repo.index retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<RepoIndexOutputV1_0>(
                        VerticalTool::RepoIndex,
                        &output,
                        VerticalTool::RepoIndex
                            .initial_output_schema_json()
                            .expect("repo.index retains its 1.0 output schema"),
                    );
                }
                "repo.status" => {
                    assert_round_trip_with_schema::<RepoStatusInput>(
                        VerticalTool::RepoStatus,
                        &input,
                        VerticalTool::RepoStatus
                            .legacy_input_schema_json()
                            .expect("repo.status retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<RepoStatusOutputV1_0>(
                        VerticalTool::RepoStatus,
                        &output,
                        VerticalTool::RepoStatus
                            .legacy_output_schema_json()
                            .expect("repo.status retains its 1.0 output schema"),
                    );
                }
                "repo.list" => {
                    assert_round_trip::<RepoListInput>(VerticalTool::RepoList, &input, true);
                    assert_schema_fixture(
                        include_str!(
                            "../../../schemas/generated/json/mcp-repo-list-output-1.0.schema.json"
                        ),
                        &output,
                        "repo.list 1.0",
                    );
                }
                "operation.status" => {
                    assert_round_trip_with_schema::<OperationStatusInput>(
                        VerticalTool::OperationStatus,
                        &input,
                        VerticalTool::OperationStatus
                            .initial_input_schema_json()
                            .expect("operation.status retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<OperationStatusOutputV1_0>(
                        VerticalTool::OperationStatus,
                        &output,
                        VerticalTool::OperationStatus
                            .initial_output_schema_json()
                            .expect("operation.status retains its 1.0 output schema"),
                    );
                }
                "code.locate" => {
                    assert_round_trip_with_schema::<CodeLocateInput>(
                        VerticalTool::CodeLocate,
                        &input,
                        VerticalTool::CodeLocate
                            .third_legacy_input_schema_json()
                            .expect("retained 1.0 input"),
                    );
                    assert_round_trip_with_schema::<CodeLocateOutput>(
                        VerticalTool::CodeLocate,
                        &output,
                        VerticalTool::CodeLocate
                            .third_legacy_output_schema_json()
                            .expect("retained 1.0 output"),
                    );
                }
                "symbol.explain" => {
                    assert_round_trip_with_schema::<SymbolExplainInput>(
                        VerticalTool::SymbolExplain,
                        &input,
                        VerticalTool::SymbolExplain
                            .fourth_legacy_input_schema_json()
                            .expect("symbol.explain retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<SymbolExplainOutputV1_0>(
                        VerticalTool::SymbolExplain,
                        &output,
                        VerticalTool::SymbolExplain
                            .fourth_legacy_output_schema_json()
                            .expect("symbol.explain retains its 1.0 output schema"),
                    );
                }
                "source.read" => {
                    assert_round_trip::<SourceReadInput>(VerticalTool::SourceRead, &input, true);
                    assert_round_trip::<SourceReadOutput>(VerticalTool::SourceRead, &output, false);
                }
                "symbol.relationships" => {
                    assert_round_trip_with_schema::<SymbolRelationshipsInput>(
                        VerticalTool::SymbolRelationships,
                        &input,
                        VerticalTool::SymbolRelationships
                            .previous_input_schema_json()
                            .expect("symbol.relationships retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<SymbolRelationshipsOutputV1_0>(
                        VerticalTool::SymbolRelationships,
                        &output,
                        VerticalTool::SymbolRelationships
                            .previous_output_schema_json()
                            .expect("symbol.relationships retains its 1.0 output schema"),
                    );
                }
                "flow.trace" => {
                    assert_round_trip::<FlowTraceInput>(VerticalTool::FlowTrace, &input, true);
                    assert_round_trip::<FlowTraceOutput>(VerticalTool::FlowTrace, &output, false);
                }
                "architecture.overview" => {
                    assert_round_trip_with_schema::<ArchitectureOverviewInput>(
                        VerticalTool::ArchitectureOverview,
                        &input,
                        VerticalTool::ArchitectureOverview
                            .previous_input_schema_json()
                            .expect("architecture.overview retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<ArchitectureOverviewOutputV1_0>(
                        VerticalTool::ArchitectureOverview,
                        &output,
                        VerticalTool::ArchitectureOverview
                            .previous_output_schema_json()
                            .expect("architecture.overview retains its 1.0 output schema"),
                    );
                }
                "architecture.cycles" => {
                    assert_round_trip_with_schema::<ArchitectureCyclesInput>(
                        VerticalTool::ArchitectureCycles,
                        &input,
                        VerticalTool::ArchitectureCycles
                            .previous_input_schema_json()
                            .expect("architecture.cycles retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<ArchitectureCyclesOutputV1_0>(
                        VerticalTool::ArchitectureCycles,
                        &output,
                        VerticalTool::ArchitectureCycles
                            .previous_output_schema_json()
                            .expect("architecture.cycles retains its 1.0 output schema"),
                    );
                }
                "code.dead" => {
                    assert_round_trip_with_schema::<CodeDeadInput>(
                        VerticalTool::CodeDead,
                        &input,
                        VerticalTool::CodeDead
                            .previous_input_schema_json()
                            .expect("code.dead retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<CodeDeadOutputV1_0>(
                        VerticalTool::CodeDead,
                        &output,
                        VerticalTool::CodeDead
                            .previous_output_schema_json()
                            .expect("code.dead retains its 1.0 output schema"),
                    );
                }
                "change.impact" => {
                    assert_round_trip_with_schema::<ChangeImpactInput>(
                        VerticalTool::ChangeImpact,
                        &input,
                        VerticalTool::ChangeImpact
                            .fourth_legacy_input_schema_json()
                            .expect("change.impact retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<ChangeImpactOutputV1_0>(
                        VerticalTool::ChangeImpact,
                        &output,
                        VerticalTool::ChangeImpact
                            .fourth_legacy_output_schema_json()
                            .expect("change.impact retains its 1.0 output schema"),
                    );
                }
                "tests.select" => {
                    assert_round_trip_with_schema::<TestsSelectInput>(
                        VerticalTool::TestsSelect,
                        &input,
                        VerticalTool::TestsSelect
                            .previous_input_schema_json()
                            .expect("tests.select retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<TestsSelectOutputV1_0>(
                        VerticalTool::TestsSelect,
                        &output,
                        VerticalTool::TestsSelect
                            .previous_output_schema_json()
                            .expect("tests.select retains its 1.0 output schema"),
                    );
                }
                "history.compare" => {
                    assert_round_trip_with_schema::<HistoryCompareInputV1_0>(
                        VerticalTool::HistoryCompare,
                        &input,
                        VerticalTool::HistoryCompare
                            .fourth_legacy_input_schema_json()
                            .expect("history.compare retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<HistoryCompareOutputV1_0>(
                        VerticalTool::HistoryCompare,
                        &output,
                        VerticalTool::HistoryCompare
                            .fourth_legacy_output_schema_json()
                            .expect("history.compare retains its 1.0 output schema"),
                    );
                }
                "plan.change" => {
                    assert_round_trip_with_schema::<PlanChangeInputV1_0>(
                        VerticalTool::PlanChange,
                        &input,
                        VerticalTool::PlanChange
                            .previous_input_schema_json()
                            .expect("plan.change retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<PlanChangeOutputV1_0>(
                        VerticalTool::PlanChange,
                        &output,
                        VerticalTool::PlanChange
                            .previous_output_schema_json()
                            .expect("plan.change retains its 1.0 output schema"),
                    );
                }
                "context.pack" => {
                    assert_round_trip_with_schema::<ContextPackInput>(
                        VerticalTool::ContextPack,
                        &input,
                        VerticalTool::ContextPack
                            .previous_input_schema_json()
                            .expect("context.pack retains its 1.0 input schema"),
                    );
                    assert_round_trip_with_schema::<ContextPackOutput>(
                        VerticalTool::ContextPack,
                        &output,
                        VerticalTool::ContextPack
                            .previous_output_schema_json()
                            .expect("context.pack retains its 1.0 output schema"),
                    );
                }
                "query.advanced" => {
                    assert_round_trip_with_schema::<QueryAdvancedInput>(
                        VerticalTool::QueryAdvanced,
                        &input,
                        VerticalTool::QueryAdvanced
                            .third_legacy_input_schema_json()
                            .expect("retained 1.0 input"),
                    );
                    assert_round_trip_with_schema::<QueryAdvancedOutput>(
                        VerticalTool::QueryAdvanced,
                        &output,
                        VerticalTool::QueryAdvanced
                            .third_legacy_output_schema_json()
                            .expect("retained 1.0 output"),
                    );
                }
                "query.batch" => {
                    assert_round_trip::<QueryBatchInput>(VerticalTool::QueryBatch, &input, true);
                    assert_round_trip::<QueryBatchOutput>(VerticalTool::QueryBatch, &output, false);
                }
                other => panic!("unexpected retained tool contract {other}"),
            }
        }
        assert_eq!(
            retained,
            VerticalTool::ALL
                .into_iter()
                .map(VerticalTool::name)
                .collect(),
            "retained examples must match the complete public catalog"
        );

        let current_repo_list: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/2.0/repo-list-contract.json"
        ))
        .expect("current repo.list contract is valid JSON");
        assert_eq!(current_repo_list["tool"], "repo.list");
        assert_round_trip::<RepoListInput>(
            VerticalTool::RepoList,
            &current_repo_list["input"],
            true,
        );
        assert_round_trip::<RepoListOutput>(
            VerticalTool::RepoList,
            &current_repo_list["output"],
            false,
        );
    }

    fn assert_round_trip<T>(tool: VerticalTool, fixture: &Value, input: bool)
    where
        T: DeserializeOwned + Serialize + PartialEq + Debug,
    {
        let schema_text = if input {
            tool.input_schema_json()
        } else {
            tool.output_schema_json()
        };
        assert_round_trip_with_schema::<T>(tool, fixture, schema_text);
    }

    fn assert_round_trip_with_schema<T>(tool: VerticalTool, fixture: &Value, schema_text: &str)
    where
        T: DeserializeOwned + Serialize + PartialEq + Debug,
    {
        let decoded: T = serde_json::from_value(fixture.clone()).unwrap_or_else(|error| {
            panic!("{} fixture decodes through Rust: {error}", tool.name())
        });
        let encoded = serde_json::to_value(&decoded).expect("typed fixture serializes");
        assert_eq!(
            &encoded,
            fixture,
            "absent optional fields remain absent for {}",
            tool.name()
        );
        let schema: Value = serde_json::from_str(schema_text).expect("tool schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("tool schema compiles");
        assert!(
            validator.is_valid(&encoded),
            "{} fixture passes its generated schema",
            tool.name()
        );
        let round_tripped: T =
            serde_json::from_value(encoded).expect("serialized fixture decodes through Rust");
        assert_eq!(round_tripped, decoded);
    }

    fn assert_schema_fixture(schema_text: &str, fixture: &Value, label: &str) {
        let schema: Value = serde_json::from_str(schema_text).expect("tool schema is valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("tool schema compiles");
        assert!(
            validator.is_valid(fixture),
            "{label} fixture passes its generated schema"
        );
    }

    #[test]
    fn tool_contract_versions_are_explicit_per_tool() {
        assert_eq!(
            VerticalTool::RepoList.contract_version(),
            crate::REPO_LIST_SCHEMA_VERSION
        );
        assert_eq!(
            VerticalTool::RepoIndex.contract_version(),
            crate::MCP_OPERATION_SCHEMA_VERSION
        );
        assert_eq!(
            VerticalTool::RepoStatus.contract_version(),
            crate::MCP_REPOSITORY_STATUS_SCHEMA_VERSION
        );
        assert_eq!(
            VerticalTool::OperationStatus.contract_version(),
            crate::MCP_OPERATION_STATUS_SCHEMA_VERSION
        );
        for tool in [
            VerticalTool::SymbolRelationships,
            VerticalTool::TestsSelect,
            VerticalTool::ArchitectureOverview,
            VerticalTool::ArchitectureCycles,
            VerticalTool::CodeDead,
            VerticalTool::PlanChange,
            VerticalTool::ContextPack,
        ] {
            assert_eq!(tool.contract_version(), crate::MCP_ANALYSIS_SCHEMA_VERSION);
        }
        for tool in [
            VerticalTool::SymbolExplain,
            VerticalTool::ChangeImpact,
            VerticalTool::HistoryCompare,
        ] {
            assert_eq!(tool.contract_version(), "1.5");
            assert_eq!(tool.previous_contract_version(), Some("1.4"));
            assert_eq!(tool.legacy_contract_version(), Some("1.3"));
            assert_eq!(tool.second_legacy_contract_version(), Some("1.2"));
            assert_eq!(tool.third_legacy_contract_version(), Some("1.1"));
            assert_eq!(tool.fourth_legacy_contract_version(), Some("1.0"));
        }
        for tool in [VerticalTool::CodeLocate, VerticalTool::QueryAdvanced] {
            assert_eq!(tool.contract_version(), "1.4");
            assert_eq!(tool.previous_contract_version(), Some("1.3"));
            assert_eq!(tool.legacy_contract_version(), Some("1.2"));
            assert_eq!(tool.second_legacy_contract_version(), Some("1.1"));
            assert_eq!(tool.third_legacy_contract_version(), Some("1.0"));
            assert_eq!(tool.fourth_legacy_contract_version(), None);
        }
        for tool in VerticalTool::ALL {
            if !matches!(
                tool,
                VerticalTool::RepoList
                    | VerticalTool::RepoIndex
                    | VerticalTool::RepoStatus
                    | VerticalTool::OperationStatus
                    | VerticalTool::SymbolExplain
                    | VerticalTool::SymbolRelationships
                    | VerticalTool::ChangeImpact
                    | VerticalTool::TestsSelect
                    | VerticalTool::ArchitectureOverview
                    | VerticalTool::ArchitectureCycles
                    | VerticalTool::CodeDead
                    | VerticalTool::HistoryCompare
                    | VerticalTool::PlanChange
                    | VerticalTool::ContextPack
                    | VerticalTool::CodeLocate
                    | VerticalTool::QueryAdvanced
            ) {
                assert_eq!(tool.contract_version(), crate::MCP_SCHEMA_VERSION);
            }
        }
    }

    #[test]
    fn repo_status_retains_exact_one_one_and_one_zero_contracts() {
        let tool = VerticalTool::RepoStatus;
        assert_eq!(tool.contract_version(), "1.2");
        assert_eq!(tool.previous_contract_version(), Some("1.1"));
        assert_eq!(tool.legacy_contract_version(), Some("1.0"));

        let mut current = retained_tool_output("repo.status");
        current["schema_version"] = json!("1.2");
        current["data"]["retained_durable_bytes"] = json!(768);
        current["data"]["logical_snapshot"] = json!({
            "schema_version": "1.0",
            "hash": "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
        });
        let current_schema: Value =
            serde_json::from_str(tool.output_schema_json()).expect("current schema is valid JSON");
        let current_validator =
            jsonschema::draft202012::new(&current_schema).expect("current schema compiles");
        assert!(current_validator.is_valid(&current));
        serde_json::from_value::<RepoStatusOutput>(current.clone())
            .expect("current output decodes through the 1.2 contract");

        let mut missing_identity = current.clone();
        missing_identity["data"]
            .as_object_mut()
            .expect("repo.status data is an object")
            .remove("logical_snapshot");
        assert!(!current_validator.is_valid(&missing_identity));

        let mut previous = current;
        previous["schema_version"] = json!("1.1");
        previous["data"]
            .as_object_mut()
            .expect("repo.status data is an object")
            .remove("logical_snapshot");
        let previous_schema: Value = serde_json::from_str(
            tool.previous_output_schema_json()
                .expect("repo.status retains its 1.1 output schema"),
        )
        .expect("previous schema is valid JSON");
        let previous_validator =
            jsonschema::draft202012::new(&previous_schema).expect("previous schema compiles");
        assert!(previous_validator.is_valid(&previous));
        serde_json::from_value::<RepoStatusOutputV1_1>(previous.clone())
            .expect("previous output decodes through the 1.1 contract");

        previous["data"]["logical_snapshot"] = Value::Null;
        assert!(!previous_validator.is_valid(&previous));

        let legacy = retained_tool_output("repo.status");
        let legacy_schema: Value = serde_json::from_str(
            tool.legacy_output_schema_json()
                .expect("repo.status retains its 1.0 output schema"),
        )
        .expect("legacy schema is valid JSON");
        let legacy_validator =
            jsonschema::draft202012::new(&legacy_schema).expect("legacy schema compiles");
        assert!(legacy_validator.is_valid(&legacy));
        serde_json::from_value::<RepoStatusOutputV1_0>(legacy)
            .expect("legacy output decodes through the 1.0 contract");
    }

    #[test]
    fn continuation_cursor_is_required_nullable_and_bounded() {
        let mut fixture = retained_tool_output("code.locate");
        fixture["schema_version"] = json!(VerticalTool::CodeLocate.contract_version());
        let schema: Value = serde_json::from_str(VerticalTool::CodeLocate.output_schema_json())
            .expect("valid JSON");
        let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");
        assert!(validator.is_valid(&fixture));

        let mut absent = fixture.clone();
        absent
            .as_object_mut()
            .expect("output is an object")
            .remove("next_cursor");
        assert!(!validator.is_valid(&absent));

        let mut maximum = fixture.clone();
        maximum["next_cursor"] = json!("c".repeat(4_096));
        assert!(validator.is_valid(&maximum));
        serde_json::from_value::<crate::source_entity::CodeLocateOutputV1_4>(maximum)
            .expect("maximum-sized cursor decodes");

        let mut oversized = fixture;
        oversized["next_cursor"] = json!("c".repeat(4_097));
        assert!(!validator.is_valid(&oversized));
        assert!(
            serde_json::from_value::<crate::source_entity::CodeLocateOutputV1_4>(oversized)
                .is_err()
        );
    }

    #[test]
    fn architecture_cycles_schema_accepts_bounded_large_and_self_cycles() {
        let schema: Value =
            serde_json::from_str(VerticalTool::ArchitectureCycles.output_schema_json())
                .expect("architecture cycles output schema is valid");
        let validator =
            jsonschema::draft202012::new(&schema).expect("architecture cycles schema compiles");
        let symbol = json!("sym1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v");
        let mut output = retained_tool_output("architecture.cycles");
        output["schema_version"] = json!("1.1");
        output["data"]["projection"] = json!({
            "relations": ["calls"],
            "min_confidence": 0,
            "level": "module",
            "rank_by": "size",
            "omitted_nodes": 0
        });
        output["data"]["components"] = json!([{
            "size": 128,
            "members": vec![symbol.clone(); 128],
            "internal_edges": 128,
            "edge_weight": 128,
            "change_risk": 0,
            "break_cost": 500
        }]);
        output["data"]["cycles"] = json!([{
            "nodes": vec![symbol.clone(); 129],
            "edge_evidence": [],
            "confidence": 900
        }]);
        assert!(validator.is_valid(&output));

        output["data"]["components"] = json!([{
            "size": 1,
            "members": [symbol.clone()],
            "internal_edges": 1,
            "edge_weight": 1,
            "change_risk": 0,
            "break_cost": 500
        }]);
        output["data"]["cycles"] = json!([{
            "nodes": [symbol.clone(), symbol],
            "edge_evidence": [],
            "confidence": 900
        }]);
        assert!(validator.is_valid(&output));
    }

    #[test]
    fn every_tool_accepts_only_the_checked_versioned_error_envelope() {
        let error = |schema_version| {
            json!({
                "schema_version": schema_version,
                "error": {
                    "code": "NOT_FOUND",
                    "message": "requested entity was not found",
                    "retryable": false,
                    "retry_after_ms": null,
                    "repository": null,
                    "operation": null,
                    "generation": null,
                    "details": {},
                    "next_actions": []
                }
            })
        };
        for tool in VerticalTool::ALL {
            let schema: Value =
                serde_json::from_str(tool.output_schema_json()).expect("output schema is valid");
            let validator = jsonschema::draft202012::new(&schema).expect("output schema compiles");
            let schema_version = if tool == VerticalTool::RepoList {
                crate::MCP_SCHEMA_VERSION
            } else {
                tool.contract_version()
            };
            assert!(
                validator.is_valid(&error(schema_version)),
                "{} accepts the shared error envelope",
                tool.name()
            );
        }
        let current_operation_error = error("1.6");
        serde_json::from_value::<OperationStatusOutput>(current_operation_error)
            .expect("current operation error decodes");
        let retained_operation_1_5 = error("1.5");
        serde_json::from_value::<OperationStatusOutputV1_5>(retained_operation_1_5)
            .expect("retained operation 1.5 error decodes");
        let retained_operation_1_4 = error("1.4");
        serde_json::from_value::<OperationStatusOutputV1_4>(retained_operation_1_4)
            .expect("retained operation 1.4 error decodes");
        let retained_operation_1_3 = error("1.3");
        serde_json::from_value::<OperationStatusOutputV1_3>(retained_operation_1_3)
            .expect("retained operation 1.3 error decodes");
        let current_repo_error = error("1.3");
        serde_json::from_value::<RepoIndexOutput>(current_repo_error)
            .expect("current repo error decodes");
        let retained_repo_1_2 = error("1.2");
        serde_json::from_value::<RepoIndexOutputV1_2>(retained_repo_1_2)
            .expect("retained repo 1.2 error decodes");
        let retained_operation_1_2 = error("1.2");
        serde_json::from_value::<OperationStatusOutputV1_2>(retained_operation_1_2)
            .expect("retained operation 1.2 error decodes");
        let current_error = error("1.1");
        serde_json::from_value::<RepoIndexOutputV1_1>(current_error.clone())
            .expect("retained repo error decodes");
        serde_json::from_value::<RepoStatusOutputV1_1>(current_error.clone())
            .expect("current status error decodes");
        serde_json::from_value::<OperationStatusOutputV1_1>(current_error.clone())
            .expect("retained operation 1.1 error decodes");
        serde_json::from_value::<SymbolExplainOutputV1_1>(current_error.clone())
            .expect("current explain error decodes");
        serde_json::from_value::<ChangeImpactOutputV1_1>(current_error.clone())
            .expect("retained impact error decodes");
        serde_json::from_value::<crate::change::ChangeImpactOutputV1_2>(error("1.2"))
            .expect("current impact error decodes");
        serde_json::from_value::<crate::change::HistoryCompareOutputV1_2>(error("1.2"))
            .expect("current history error decodes");
        for version in ["1.0", "1.1", "1.3"] {
            assert!(
                serde_json::from_value::<crate::change::ChangeImpactOutputV1_2>(error(version))
                    .is_err()
            );
            assert!(
                serde_json::from_value::<crate::change::HistoryCompareOutputV1_2>(error(version))
                    .is_err()
            );
        }
        serde_json::from_value::<TestsSelectOutputV1_1>(current_error.clone())
            .expect("current tests error decodes");
        let legacy_error = error("1.0");
        serde_json::from_value::<RepoIndexOutputV1_0>(legacy_error.clone())
            .expect("legacy repo error decodes");
        serde_json::from_value::<RepoStatusOutputV1_0>(legacy_error.clone())
            .expect("legacy status error decodes");
        serde_json::from_value::<OperationStatusOutputV1_0>(legacy_error.clone())
            .expect("operation error decodes");
        serde_json::from_value::<RepoListOutput>(legacy_error.clone())
            .expect("catalog error decodes");
        serde_json::from_value::<CodeLocateOutput>(legacy_error.clone())
            .expect("locate error decodes");
        serde_json::from_value::<SymbolExplainOutputV1_0>(legacy_error.clone())
            .expect("explain error decodes");
        serde_json::from_value::<ChangeImpactOutputV1_0>(legacy_error.clone())
            .expect("impact error decodes");
        serde_json::from_value::<TestsSelectOutputV1_0>(legacy_error.clone())
            .expect("tests error decodes");
        serde_json::from_value::<SourceReadOutput>(legacy_error).expect("source error decodes");

        let mut arbitrary_code = current_error.clone();
        arbitrary_code["error"]["code"] = json!("EXECUTOR_PRIVATE_CODE");
        assert!(serde_json::from_value::<RepoIndexOutputV1_1>(arbitrary_code).is_err());

        let mut source_shaped_message = current_error;
        source_shaped_message["error"]["message"] = json!("C:\\Users\\person\\secret.rs");
        assert!(serde_json::from_value::<RepoIndexOutputV1_1>(source_shaped_message).is_err());
    }

    #[test]
    fn current_operation_schemas_retain_actions_only_where_the_wire_supports_them() {
        let current_error = |version| {
            json!({
                "schema_version": version,
                "error": {
                    "code": "RESOURCE_EXHAUSTED",
                    "message": "repository capacity is exhausted",
                    "retryable": false,
                    "retry_after_ms": null,
                    "repository": null,
                    "operation": null,
                    "generation": null,
                    "details": {},
                    "next_actions": [
                        {
                            "action": "update_configuration",
                            "key": "analysis.max_repositories"
                        },
                        {
                            "action": "delete_repository"
                        }
                    ]
                }
            })
        };
        let current_repo = current_error("1.3");
        serde_json::from_value::<RepoIndexOutput>(current_repo.clone())
            .expect("current repo error actions decode");
        assert_schema_fixture(
            VerticalTool::RepoIndex.output_schema_json(),
            &current_repo,
            "repo.index 1.3 actions",
        );
        let retained_repo: Value = serde_json::from_str(
            VerticalTool::RepoIndex
                .previous_output_schema_json()
                .expect("repo.index retains schema 1.2"),
        )
        .expect("retained repo schema is valid");
        let retained_repo =
            jsonschema::draft202012::new(&retained_repo).expect("retained repo schema compiles");
        assert!(retained_repo.is_valid(&current_error("1.2")));
        for schema in [
            VerticalTool::RepoIndex
                .legacy_output_schema_json()
                .expect("repo.index retains schema 1.1"),
            VerticalTool::RepoIndex
                .initial_output_schema_json()
                .expect("repo.index retains schema 1.0"),
        ] {
            assert!(!schema.contains("update_configuration"));
            assert!(!schema.contains("delete_repository"));
        }

        let current_status = current_error("1.6");
        serde_json::from_value::<OperationStatusOutput>(current_status.clone())
            .expect("current operation error actions decode");
        assert_schema_fixture(
            VerticalTool::OperationStatus.output_schema_json(),
            &current_status,
            "operation.status 1.6 actions",
        );
        let previous_status = VerticalTool::OperationStatus
            .previous_output_schema_json()
            .expect("operation.status retains schema 1.5");
        assert!(previous_status.contains("update_configuration"));
        assert!(previous_status.contains("delete_repository"));
        let legacy_status = VerticalTool::OperationStatus
            .legacy_output_schema_json()
            .expect("operation.status retains schema 1.4");
        assert!(legacy_status.contains("update_configuration"));
        assert!(legacy_status.contains("delete_repository"));
        let second_legacy_status = VerticalTool::OperationStatus
            .second_legacy_output_schema_json()
            .expect("operation.status retains schema 1.3");
        assert!(second_legacy_status.contains("update_configuration"));
        assert!(second_legacy_status.contains("delete_repository"));
        for schema in [
            VerticalTool::OperationStatus
                .third_legacy_output_schema_json()
                .expect("operation.status retains schema 1.2"),
            VerticalTool::OperationStatus
                .fourth_legacy_output_schema_json()
                .expect("operation.status retains schema 1.1"),
            VerticalTool::OperationStatus
                .initial_output_schema_json()
                .expect("operation.status retains schema 1.0"),
        ] {
            assert!(!schema.contains("update_configuration"));
            assert!(!schema.contains("delete_repository"));
        }
    }

    #[test]
    fn diagnostics_and_warnings_reject_source_shaped_messages() {
        let mut diagnostic = retained_tool_output("repo.index");
        diagnostic["data"]["diagnostics"] = json!([{
            "code": "fixture",
            "message": "C:\\Users\\person\\secret.rs"
        }]);
        assert!(serde_json::from_value::<RepoIndexOutput>(diagnostic).is_err());

        let mut warning = retained_tool_output("code.locate");
        warning["warnings"] = json!([{
            "code": "fixture",
            "message": "src/lib.rs was skipped"
        }]);
        assert!(serde_json::from_value::<CodeLocateOutput>(warning).is_err());
    }

    #[test]
    fn file_ranges_and_source_chunks_enforce_cross_field_invariants() {
        let mut inverted_input = retained_tool_input("source.read");
        inverted_input["references"][0]["start_byte"] = json!(11);
        inverted_input["references"][0]["end_byte"] = json!(10);
        assert!(serde_json::from_value::<SourceReadInput>(inverted_input).is_err());

        let fixture = retained_tool_output("source.read");
        let mut mismatched_span = fixture.clone();
        mismatched_span["data"]["chunks"][0]["end_byte"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_span).is_err());

        let mut mismatched_reference = fixture.clone();
        mismatched_reference["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_reference).is_err());

        let mut mismatched_hash = fixture.clone();
        mismatched_hash["data"]["chunks"][0]["content_hash"] =
            json!("b3_75bprqkwsgfv4kw74qjkj2xli5knc43nxoicyuklbc4qajuuj6gsxi36m4");
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_hash).is_err());

        let mut mismatched_line_hint = fixture.clone();
        mismatched_line_hint["data"]["chunks"][0]["source_ref"]["line_hint"]["end_line"] = json!(2);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_line_hint).is_err());

        let mut profiled_line_metadata = fixture.clone();
        profiled_line_metadata["data"]["chunks"][0]["source_ref"]
            .as_object_mut()
            .expect("source reference is an object")
            .remove("line_hint");
        serde_json::from_value::<SourceReadOutput>(profiled_line_metadata)
            .expect("profiled line metadata remains valid without a redundant reference hint");

        let mut mismatched_total = fixture;
        mismatched_total["data"]["total_source_bytes"] = json!(9);
        assert!(serde_json::from_value::<SourceReadOutput>(mismatched_total).is_err());
    }

    #[test]
    fn source_chunk_base64_length_is_checked_canonically() {
        let fixture = retained_tool_output("source.read");
        let mut valid = fixture.clone();
        valid["data"]["chunks"][0]["content"] = json!("AQID");
        valid["data"]["chunks"][0]["encoding"] = json!("base64");
        valid["data"]["chunks"][0]["end_byte"] = json!(3);
        valid["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(3);
        valid["data"]["total_source_bytes"] = json!(3);
        serde_json::from_value::<SourceReadOutput>(valid).expect("canonical base64 decodes");

        let mut noncanonical = fixture;
        noncanonical["data"]["chunks"][0]["content"] = json!("AQI=");
        noncanonical["data"]["chunks"][0]["encoding"] = json!("base64");
        noncanonical["data"]["chunks"][0]["end_byte"] = json!(2);
        noncanonical["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(2);
        noncanonical["data"]["total_source_bytes"] = json!(2);
        noncanonical["data"]["chunks"][0]["content"] = json!("AQJ=");
        assert!(serde_json::from_value::<SourceReadOutput>(noncanonical).is_err());
    }

    fn retained_tool_input(name: &str) -> Value {
        retained_tool_fixture(name, "input")
    }

    fn retained_tool_output(name: &str) -> Value {
        retained_tool_fixture(name, "output")
    }

    fn retained_tool_fixture(name: &str, field: &str) -> Value {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        fixture["tools"]
            .as_array()
            .expect("tool contracts contain an array")
            .iter()
            .find(|entry| entry["tool"] == name)
            .unwrap_or_else(|| panic!("retained tool contract {name} exists"))[field]
            .clone()
    }
}
