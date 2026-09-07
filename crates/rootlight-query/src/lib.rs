//! Bounded daemon-independent intent plans for the first secure query slice.
//!
//! Plans pin one immutable generation and compose normalized IR, lexical
//! retrieval, and capability-confined source reads without exposing SQL or an
//! MCP transport contract.

#![forbid(unsafe_code)]

mod model;
mod projection;
mod service;
mod store;

pub use model::{
    ADVANCED_DEFAULT_MAX_DEPTH, ADVANCED_DEFAULT_MAX_RESULTS, ADVANCED_MAX_DEPTH,
    ADVANCED_MAX_ESTIMATED_COST, ADVANCED_MAX_RESULTS, ADVANCED_MAX_TRAVERSAL,
    AdvancedAggregateFunction, AdvancedAstNode, AdvancedColumnSchema, AdvancedColumnType,
    AdvancedCompleteness, AdvancedEntityKind, AdvancedOperator, AdvancedPlanExplanation,
    AdvancedPredicate, AdvancedQueryPlan, AdvancedQueryResult, AdvancedRelationKind,
    AdvancedSortKey, AdvancedTraverseDirection, AdvancedValue, AnalysisScope,
    ArchitectureCommunity, ArchitectureComponent, ArchitectureConnection, ArchitectureCyclesPlan,
    ArchitectureCyclesProjection, ArchitectureCyclesResult, ArchitectureHotspot,
    ArchitectureOverviewDerivedView, ArchitectureOverviewDetail, ArchitectureOverviewPlan,
    ArchitectureOverviewResult, ArchitectureOverviewView, BreakingCandidateRecord,
    ChangeImpactClassification, ChangeImpactPlan, ChangeImpactRelationPolicy, ChangeImpactResult,
    ChangeImpactRiskLevel, ChangeImpactRiskSummary, ChangeImpactTestCandidate, CodeDeadBlindSpot,
    CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan, CodeDeadResult,
    CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak, CycleComponent,
    CyclePath, CycleProjectionLevel, CycleRankBy, DeadCodeCandidate, DeadCodeClassification,
    DeadCodeReachabilitySummary, ExecutionCompleteness, ExecutionCompletenessState, FlowTraceEdge,
    FlowTraceFrontier, FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult,
    HistoryArchitectureDelta, HistoryChangeKind, HistoryComparePlan, HistoryCompareResult,
    HistoryCompareScope, HistorySemanticChangeKind, ImpactEntryRecord, ImpactGroupRecord,
    LineageMatchRecord, LocateHit, LocateMode, PlanChangeContextPack, PlanChangeDecision,
    PlanChangeImpactSummary, PlanChangeObjective, PlanChangePlan, PlanChangeResult,
    PlanChangeStepRecord, PlanEstimate, PlanExplanation, PlanKind, QueryBudget, QueryError,
    QueryOperator, QueryResource, QueryResponse, QueryUsage, RankedTestSelection,
    RelationDirection, RelationFamily, RelationshipEdgeTarget, RelationshipGroup,
    RepositoryDataTrust, ResolvedChangeRecord, SemanticChangeRecord, SourceChunkEncoding,
    SourceChunkResult, SourceReadPlan, SourceReadQueryResult, SymbolExplainPlan,
    SymbolExplainResult, SymbolRelationshipsPlan, SymbolRelationshipsResult, TestsSelectCoverage,
    TestsSelectGap, TestsSelectKind, TestsSelectPlan, TestsSelectResult, TokenAccountingProfile,
};
pub use projection::{
    LexicalProjectionBuilder, SOURCE_FALLBACK_TEXT_BYTES, project_lexical_documents,
    project_lexical_documents_with_all_sources, project_lexical_documents_with_full_source_terms,
    project_lexical_documents_with_sources, project_scoped_lexical_documents_for_query,
    project_scoped_lexical_documents_with_source, project_source_document_with_full_terms,
    project_source_fallback_document, project_source_fallback_document_with_text_limit,
};
pub use service::QueryService;
pub use store::{GenerationLease, GenerationSet};
