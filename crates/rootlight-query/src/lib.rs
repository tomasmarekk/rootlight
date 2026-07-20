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
    ArchitectureComponent, ArchitectureConnection, ArchitectureCyclesPlan,
    ArchitectureCyclesProjection, ArchitectureCyclesResult, ArchitectureHotspot,
    ArchitectureOverviewDerivedView, ArchitectureOverviewPlan, ArchitectureOverviewResult,
    ArchitectureOverviewView, ChangeImpactClassification, ChangeImpactPlan, ChangeImpactResult,
    ChangeImpactRiskLevel, ChangeImpactRiskSummary, ChangeImpactTestCandidate, CodeDeadBlindSpot,
    CodeDeadEntryPointPolicy, CodeDeadEntryPointSummary, CodeDeadPlan, CodeDeadResult,
    CodeDeadSuppressionRule, CodeLocatePlan, CodeLocateResult, CycleBreak, CycleComponent,
    CyclePath, DeadCodeCandidate, DeadCodeClassification, FlowTraceEdge, FlowTraceFrontier,
    FlowTracePath, FlowTracePlan, FlowTraceProjection, FlowTraceResult, ImpactEntryRecord,
    ImpactGroupRecord, LocateHit, LocateMode, PlanChangeContextPack, PlanChangeDecision,
    PlanChangeImpactSummary, PlanChangeObjective, PlanChangePlan, PlanChangeResult,
    PlanChangeStepRecord, PlanEstimate, PlanExplanation, PlanKind, QueryBudget, QueryError,
    QueryOperator, QueryResource, QueryResponse, QueryUsage, RankedTestSelection,
    RelationDirection, RelationFamily, RelationshipEdgeTarget, RelationshipGroup,
    RepositoryDataTrust, ResolvedChangeRecord, SourceChunkResult, SourceReadPlan,
    SourceReadQueryResult, SymbolExplainPlan, SymbolExplainResult, SymbolRelationshipsPlan,
    SymbolRelationshipsResult, TestsSelectCoverage, TestsSelectGap, TestsSelectKind,
    TestsSelectPlan, TestsSelectResult, TokenAccountingProfile,
};
pub use projection::project_lexical_documents;
pub use service::QueryService;
pub use store::GenerationSet;
