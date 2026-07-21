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
    AdvancedSortKey, AdvancedTraverseDirection, AdvancedValue, ArchitectureComponent,
    ArchitectureConnection, ArchitectureCyclesPlan, ArchitectureCyclesProjection,
    ArchitectureCyclesResult, ArchitectureHotspot, ArchitectureOverviewDerivedView,
    ArchitectureOverviewPlan, ArchitectureOverviewResult, ArchitectureOverviewView,
    BreakingCandidateRecord, ChangeImpactClassification, ChangeImpactPlan, ChangeImpactResult,
    ChangeImpactRiskLevel, ChangeImpactRiskSummary, ChangeImpactTestCandidate, CodeDeadBlindSpot,
    CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan, CodeDeadResult,
    CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak, CycleComponent,
    CyclePath, DeadCodeCandidate, DeadCodeClassification, FlowTraceEdge, FlowTraceFrontier,
    FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult, HistoryArchitectureDelta,
    HistoryChangeKind, HistoryComparePlan, HistoryCompareResult, HistorySemanticChangeKind,
    ImpactEntryRecord, ImpactGroupRecord, LineageMatchRecord, LocateHit, LocateMode,
    PlanChangeContextPack, PlanChangeDecision, PlanChangeImpactSummary, PlanChangeObjective,
    PlanChangePlan, PlanChangeResult, PlanChangeStepRecord, PlanEstimate, PlanExplanation,
    PlanKind, QueryBudget, QueryError, QueryOperator, QueryResource, QueryResponse, QueryUsage,
    RankedTestSelection, RelationDirection, RelationFamily, RelationshipEdgeTarget,
    RelationshipGroup, RepositoryDataTrust, ResolvedChangeRecord, SemanticChangeRecord,
    SourceChunkResult, SourceReadPlan, SourceReadQueryResult, SymbolExplainPlan,
    SymbolExplainResult, SymbolRelationshipsPlan, SymbolRelationshipsResult, TestsSelectCoverage,
    TestsSelectGap, TestsSelectKind, TestsSelectPlan, TestsSelectResult, TokenAccountingProfile,
};
pub use projection::project_lexical_documents;
pub use service::QueryService;
pub use store::GenerationSet;
