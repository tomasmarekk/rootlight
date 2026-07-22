//! Focused fake-port tests for the production first-slice MCP executor.
//!
//! The fixtures assert wire-visible facts and keep daemon transport out of the
//! mapping suite so failures remain deterministic and source-free.

use std::{
    collections::VecDeque,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use rootlight_client::{
    AdvancedColumn as ClientAdvancedColumn, AdvancedQuery as ClientAdvancedQuery,
    AnalysisTier as ClientTier, ArchitectureCycles as ClientArchitectureCycles,
    ArchitectureOverview as ClientArchitectureOverview,
    ArchitectureOverviewComponent as ClientArchitectureComponent,
    ArchitectureOverviewConnection as ClientArchitectureConnection,
    ArchitectureOverviewDerivedView as ClientDerivedView,
    ArchitectureOverviewHotspot as ClientArchitectureHotspot, ChangeImpact as ClientChangeImpact,
    ChangeImpactEntry as ClientImpactEntry, ChangeImpactGroup as ClientImpactGroup,
    ChangeImpactResolvedChange as ClientResolvedChange,
    ChangeImpactRiskSummary as ClientRiskSummary, ChangeImpactTest as ClientChangeImpactTest,
    CodeDead as ClientCodeDead, CodeDeadBlindSpot as ClientBlindSpot,
    CodeDeadCandidate as ClientDeadCandidate, CodeDeadEntryPointSummary as ClientEntryPointSummary,
    CodeDeadSuppressionRule as ClientSuppressionRule, CoverageStatus as ClientCoverage,
    Cycle as ClientCycle, CycleBreakCandidate as ClientCycleBreak,
    CycleComponent as ClientCycleComponent, CycleProjection as ClientCycleProjection,
    FlowTrace as ClientFlowTrace, FlowTraceEdge as ClientTraceEdge,
    FlowTraceFrontier as ClientTraceFrontier, FlowTracePath as ClientTracePath,
    FlowTraceProjection as ClientTraceProjection,
    HistoryArchitectureDelta as ClientHistoryArchitectureDelta,
    HistoryBreakingCandidate as ClientHistoryBreakingCandidate,
    HistoryCompare as ClientHistoryCompare, HistoryLineageMatch as ClientHistoryLineageMatch,
    HistoryMatchedStates as ClientHistoryMatchedStates,
    HistorySemanticChange as ClientHistorySemanticChange, LocateHit, OperationKind, OperationStage,
    OperationState as ClientOperationState, PlanChange as ClientPlanChange,
    PlanChangeContextPack as ClientPlanContextPack, PlanChangeDecision as ClientPlanDecision,
    PlanChangeImpactSummary as ClientPlanImpactSummary, PlanChangeStep as ClientPlanStep,
    QueryContext, QueryUsage, RecoveryClass, RelationshipGroup as ClientRelationshipGroup,
    RelationshipTarget as ClientRelationshipTarget, RepositoryCoverageEntry, RepositoryList,
    RepositoryListEntry, RepositoryStatus, SourceChunk as ClientSourceChunk,
    SymbolExplanation as ClientExplanation, SymbolRelationships as ClientRelationships,
    TestsSelect as ClientTestsSelect, TestsSelectCoverageStrategy as ClientCoverageStrategy,
    TestsSelectGap as ClientTestGap, TestsSelectRankedTest as ClientRankedTest,
};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{
    CoverageStatus as IrCoverage, EntityKind as IrEntityKind, LineRange, SourceRef, SourceSpan,
};
use rootlight_mcp_contract::{
    CodeLocateOutput, ErrorCode, OperationStatusOutput, RepoIndexOutput, SourceReadOutput,
    SymbolExplainOutput,
    change::{
        ChangeClassification, ChangeImpactOutput, HistoryCompareOutput, PlanChangeOutput,
        RiskLevel, SemanticChangeKind, TestKind, TestsSelectOutput,
    },
    context::{
        ColumnType, ContextPackOutput, QueryAdvancedOutput, QueryBatchOutput, QueryCompleteness,
    },
    intent::{
        ArchitectureCyclesOutput, ArchitectureOverviewOutput, ArchitectureView, CodeDeadOutput,
        FlowTraceOutput, RelationKind, SymbolRelationshipsOutput,
    },
    repository::{RepoListOutput, RepoStatusOutput, RepositoryState},
    vertical::{
        AnalysisTier, CacheStatus, Freshness, IndexMode, IndexPlanScope, IndexPlanSummary,
        LanguageCoverage, OperationState, RequiredNullable,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, watch};

use super::*;
use crate::{
    HandlerResponse, OperatingRequest, RequestHandler, RequestId, ToolExecutor, ToolRouter,
};

#[derive(Debug, Clone)]
enum FakeOutcome {
    RepositoryIndex(Result<RepositoryIndexPortResponse, ClientPortError>),
    RepositoryIndexSequence(
        Arc<Mutex<VecDeque<Result<RepositoryIndexPortResponse, ClientPortError>>>>,
    ),
    PendingRepositoryIndex {
        started: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    },
    OperationStatus(Result<RepositoryOperationStatus, ClientPortError>),
    CodeLocate(Result<CodeLocatePortResponse, ClientPortError>),
    PendingCodeLocate,
    SymbolExplain(Result<SymbolExplainPortResponse, ClientPortError>),
    SourceRead(Result<SourceReadPortResponse, ClientPortError>),
    RepositoryList(Result<RepositoryList, ClientPortError>),
    RepositoryStatus(Result<RepositoryStatus, ClientPortError>),
    SymbolRelationships(Result<SymbolRelationshipsPortResponse, ClientPortError>),
    FlowTrace(Result<FlowTracePortResponse, ClientPortError>),
    ArchitectureCycles(Result<ArchitectureCyclesPortResponse, ClientPortError>),
    CodeDead(Result<CodeDeadPortResponse, ClientPortError>),
    ArchitectureOverview(Result<ArchitectureOverviewPortResponse, ClientPortError>),
    TestsSelect(Result<TestsSelectPortResponse, ClientPortError>),
    ChangeImpact(Result<ChangeImpactPortResponse, ClientPortError>),
    PlanChange(Result<PlanChangePortResponse, ClientPortError>),
    HistoryCompare(Result<HistoryComparePortResponse, ClientPortError>),
    QueryAdvanced(Result<QueryAdvancedPortResponse, ClientPortError>),
    Batch {
        status: Result<RepositoryStatus, ClientPortError>,
        locate: Result<CodeLocatePortResponse, ClientPortError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservedCall {
    RepositoryIndex(RepositoryIndexPortRequest),
    OperationStatus(OperationStatusPortRequest),
    CodeLocate(CodeLocatePortRequest),
    SymbolExplain(SymbolExplainPortRequest),
    SourceRead(SourceReadPortRequest),
    RepositoryList(RepositoryListPortRequest),
    RepositoryStatus(RepositoryStatusPortRequest),
    SymbolRelationships(SymbolRelationshipsPortRequest),
    FlowTrace(FlowTracePortRequest),
    ArchitectureCycles(ArchitectureCyclesPortRequest),
    CodeDead(CodeDeadPortRequest),
    ArchitectureOverview(ArchitectureOverviewPortRequest),
    TestsSelect(TestsSelectPortRequest),
    ChangeImpact(ChangeImpactPortRequest),
    PlanChange(PlanChangePortRequest),
    HistoryCompare(HistoryComparePortRequest),
    QueryAdvanced(QueryAdvancedPortRequest),
}

#[derive(Debug, Clone)]
struct FakePort {
    outcome: FakeOutcome,
    calls: Arc<Mutex<Vec<ObservedCall>>>,
    call_count: Arc<AtomicUsize>,
}

struct DropMarker(Arc<AtomicBool>);

impl Drop for DropMarker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl FakePort {
    fn record(&self, call: ObservedCall) {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        self.calls
            .lock()
            .expect("fake call recorder is not poisoned")
            .push(call);
    }
}

impl FirstSliceClientPort for FakePort {
    fn repository_index(
        &self,
        request: RepositoryIndexPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryIndexPortResponse> {
        self.record(ObservedCall::RepositoryIndex(request));
        let outcome = match &self.outcome {
            FakeOutcome::RepositoryIndex(outcome) => outcome.clone(),
            FakeOutcome::RepositoryIndexSequence(outcomes) => outcomes
                .lock()
                .expect("fake response sequence is not poisoned")
                .pop_front()
                .unwrap_or(Err(ClientPortError::Executor)),
            FakeOutcome::PendingRepositoryIndex { started, dropped } => {
                let started = Arc::clone(started);
                let drop_marker = DropMarker(Arc::clone(dropped));
                return Box::pin(async move {
                    let _drop_marker = drop_marker;
                    started.notify_one();
                    std::future::pending().await
                });
            }
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn operation_status(
        &self,
        request: OperationStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryOperationStatus> {
        self.record(ObservedCall::OperationStatus(request));
        let outcome = match &self.outcome {
            FakeOutcome::OperationStatus(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn code_locate(
        &self,
        request: CodeLocatePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        self.record(ObservedCall::CodeLocate(request));
        let outcome = match &self.outcome {
            FakeOutcome::CodeLocate(outcome) => outcome.clone(),
            FakeOutcome::Batch { locate, .. } => locate.clone(),
            FakeOutcome::PendingCodeLocate => return Box::pin(std::future::pending()),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn symbol_explain(
        &self,
        request: SymbolExplainPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        self.record(ObservedCall::SymbolExplain(request));
        let outcome = match &self.outcome {
            FakeOutcome::SymbolExplain(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn source_read(
        &self,
        request: SourceReadPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        self.record(ObservedCall::SourceRead(request));
        let outcome = match &self.outcome {
            FakeOutcome::SourceRead(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn repository_list(
        &self,
        request: RepositoryListPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryList> {
        self.record(ObservedCall::RepositoryList(request));
        let outcome = match &self.outcome {
            FakeOutcome::RepositoryList(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn repository_status(
        &self,
        request: RepositoryStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryStatus> {
        self.record(ObservedCall::RepositoryStatus(request));
        let outcome = match &self.outcome {
            FakeOutcome::RepositoryStatus(outcome) => outcome.clone(),
            FakeOutcome::Batch { status, .. } => status.clone(),
            FakeOutcome::SymbolExplain(Ok(_)) => Ok(repository_status_response()),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn symbol_relationships(
        &self,
        request: SymbolRelationshipsPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse> {
        self.record(ObservedCall::SymbolRelationships(request));
        let outcome = match &self.outcome {
            FakeOutcome::SymbolRelationships(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn flow_trace(
        &self,
        request: FlowTracePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse> {
        self.record(ObservedCall::FlowTrace(request));
        let outcome = match &self.outcome {
            FakeOutcome::FlowTrace(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn architecture_cycles(
        &self,
        request: ArchitectureCyclesPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse> {
        self.record(ObservedCall::ArchitectureCycles(request));
        let outcome = match &self.outcome {
            FakeOutcome::ArchitectureCycles(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn code_dead(
        &self,
        request: CodeDeadPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse> {
        self.record(ObservedCall::CodeDead(request));
        let outcome = match &self.outcome {
            FakeOutcome::CodeDead(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn architecture_overview(
        &self,
        request: ArchitectureOverviewPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse> {
        self.record(ObservedCall::ArchitectureOverview(request));
        let outcome = match &self.outcome {
            FakeOutcome::ArchitectureOverview(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn tests_select(
        &self,
        request: TestsSelectPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse> {
        self.record(ObservedCall::TestsSelect(request));
        let outcome = match &self.outcome {
            FakeOutcome::TestsSelect(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn change_impact(
        &self,
        request: ChangeImpactPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse> {
        self.record(ObservedCall::ChangeImpact(request));
        let outcome = match &self.outcome {
            FakeOutcome::ChangeImpact(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn plan_change(
        &self,
        request: PlanChangePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
        self.record(ObservedCall::PlanChange(request));
        let outcome = match &self.outcome {
            FakeOutcome::PlanChange(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn history_compare(
        &self,
        request: HistoryComparePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<HistoryComparePortResponse> {
        self.record(ObservedCall::HistoryCompare(request));
        let outcome = match &self.outcome {
            FakeOutcome::HistoryCompare(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn query_advanced(
        &self,
        request: QueryAdvancedPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<QueryAdvancedPortResponse> {
        self.record(ObservedCall::QueryAdvanced(request));
        let outcome = match &self.outcome {
            FakeOutcome::QueryAdvanced(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }
}

struct Harness {
    executor: FirstSliceToolExecutor<FakePort>,
    calls: Arc<Mutex<Vec<ObservedCall>>>,
    call_count: Arc<AtomicUsize>,
}

impl Harness {
    fn new(outcome: FakeOutcome) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let call_count = Arc::new(AtomicUsize::new(0));
        let port = FakePort {
            outcome,
            calls: Arc::clone(&calls),
            call_count: Arc::clone(&call_count),
        };
        Self {
            executor: FirstSliceToolExecutor::new(port).expect("built-in errors are valid"),
            calls,
            call_count,
        }
    }

    fn with_cursor_key(outcome: FakeOutcome, cursor_key: [u8; 32]) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let call_count = Arc::new(AtomicUsize::new(0));
        let port = FakePort {
            outcome,
            calls: Arc::clone(&calls),
            call_count: Arc::clone(&call_count),
        };
        Self {
            executor: FirstSliceToolExecutor::with_cursor_key(port, cursor_key)
                .expect("built-in errors are valid"),
            calls,
            call_count,
        }
    }

    fn only_call(&self) -> ObservedCall {
        let calls = self
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        assert_eq!(calls.len(), 1);
        calls[0].clone()
    }
}

fn cancellation() -> RequestCancellation {
    static SENDER: OnceLock<watch::Sender<bool>> = OnceLock::new();
    let sender = SENDER.get_or_init(|| watch::channel(false).0);
    RequestCancellation {
        receiver: sender.subscribe(),
    }
}

async fn execute(
    executor: &impl ToolExecutor,
    tool: VerticalTool,
    arguments: Value,
) -> Result<Map<String, Value>, ToolExecutionError> {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments are objects");
    };
    executor.execute(tool, arguments, cancellation()).await
}

fn decode<T: DeserializeOwned>(output: Map<String, Value>) -> T {
    serde_json::from_value(Value::Object(output)).expect("mapped output satisfies its wire type")
}

fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn operation() -> OperationId {
    OperationId::from_bytes([2; 16])
}

fn second_operation() -> OperationId {
    OperationId::from_bytes([10; 16])
}

fn parent_generation() -> GenerationId {
    GenerationId::from_bytes([3; 20])
}

fn generation() -> GenerationId {
    GenerationId::from_bytes([4; 20])
}

fn symbol() -> SymbolId {
    SymbolId::from_bytes([5; 20])
}

fn missing_symbol() -> SymbolId {
    SymbolId::from_bytes([6; 20])
}

fn file() -> FileId {
    FileId::from_bytes([7; 20])
}

fn content_hash() -> ContentHash {
    ContentHash::from_bytes([8; 32])
}

fn source_reference(
    start: u64,
    end: u64,
    start_line: u64,
    end_line: u64,
) -> client::SourceReference {
    client::SourceReference::new(
        repository(),
        generation(),
        file(),
        start..end,
        content_hash(),
        Some(start_line..=end_line),
    )
    .expect("test source reference is valid")
}

fn source_reference_without_lines(start: u64, end: u64) -> client::SourceReference {
    client::SourceReference::new(
        repository(),
        generation(),
        file(),
        start..end,
        content_hash(),
        None,
    )
    .expect("test source reference is valid")
}

fn wire_source_reference(start: u64, end: u64, start_line: u64, end_line: u64) -> SourceRef {
    wire_source_reference_for(repository(), generation(), start, end, start_line, end_line)
}

fn wire_source_reference_for(
    repository: RepositoryId,
    generation: GenerationId,
    start: u64,
    end: u64,
    start_line: u64,
    end_line: u64,
) -> SourceRef {
    SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file(), start, end).expect("test span is valid"),
        content_hash(),
        Some(LineRange::new(start_line, end_line).expect("test lines are valid")),
    )
}

fn schema_valid_invalid_inputs() -> Vec<(VerticalTool, Value)> {
    let exact = wire_source_reference(5, 10, 2, 2);
    vec![
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture\0invalid"}),
        ),
        (
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "references": [{
                    "source_ref": wire_source_reference_for(
                        RepositoryId::from_bytes([9; 16]),
                        generation(),
                        5,
                        10,
                        2,
                        2,
                    )
                }]
            }),
        ),
        (
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation(),
                "references": [{
                    "source_ref": wire_source_reference_for(
                        repository(),
                        parent_generation(),
                        5,
                        10,
                        2,
                        2,
                    )
                }]
            }),
        ),
        (
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "references": [
                    {"source_ref": exact.clone()},
                    {
                        "source_ref": wire_source_reference_for(
                            repository(),
                            parent_generation(),
                            10,
                            15,
                            3,
                            3,
                        )
                    }
                ]
            }),
        ),
        (
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "references": [
                    {"source_ref": exact.clone()},
                    {"source_ref": exact}
                ]
            }),
        ),
    ]
}

fn usage(results: u64, source_bytes: u64) -> QueryUsage {
    QueryUsage {
        rows: 11,
        edges: 3,
        results,
        source_bytes,
        json_bytes: 512,
        estimated_tokens: 64,
        elapsed_micros: 1_001,
    }
}

fn context(results: u64, source_bytes: u64) -> QueryContext {
    QueryContext {
        repository: repository(),
        generation: generation(),
        parent_generation: Some(parent_generation()),
        active_generation: true,
        tier: ClientTier::TierC,
        coverage_status: ClientCoverage::Complete,
        skipped_inputs: 0,
        usage: usage(results, source_bytes),
    }
}

fn metadata(trace_id: &str) -> ReadResponseMetadata {
    ReadResponseMetadata::new(
        "fixture".to_owned(),
        Freshness::Current,
        Freshness::Current,
        vec![LanguageCoverage {
            language: "rust".to_owned(),
            tier: AnalysisTier::C,
            status: IrCoverage::Complete,
        }],
        CacheStatus::Miss,
        trace_id.to_owned(),
        Vec::new(),
    )
}

fn operation_status(state: ClientOperationState) -> client::OperationStatus {
    client::OperationStatus {
        operation: operation(),
        state,
        revision: 9,
        completed_units: 4,
        total_units: 10,
        error: None,
        kind: OperationKind::RepositoryIndex,
        stage: OperationStage::Executing,
        plan_hash: [9; 32],
        detached: true,
        cancellation_requested: false,
        deadline_unix_ms: None,
        lease_expires_unix_ms: None,
        recovery_class: RecoveryClass::NotApplicable,
    }
}

fn locate_response() -> CodeLocatePortResponse {
    CodeLocatePortResponse::new(
        client::CodeLocate {
            context: context(1, 0),
            hits: vec![LocateHit {
                symbol: symbol(),
                file: file(),
                identifier: "Publisher".to_owned(),
                qualified_name: "crate::Publisher".to_owned(),
                path: "src/lib.rs".to_owned(),
                kind: "struct".to_owned(),
                language: "rust".to_owned(),
                tier: ClientTier::TierC,
                generated: false,
                score: 990,
                source: Some(source_reference(4, 12, 2, 2)),
            }],
            matched_candidates: 1,
            truncated: false,
        },
        metadata("trace-locate-1"),
        vec!["publish".to_owned()],
    )
}

fn repository_status_response() -> RepositoryStatus {
    RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: Some(parent_generation()),
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![RepositoryCoverageEntry {
            language: "rust".to_owned(),
            tier: "tier_c".to_owned(),
            status: "complete".to_owned(),
            discovered_files: 1,
            indexed_files: 1,
        }],
    }
}

fn batch_harness() -> Harness {
    Harness::new(FakeOutcome::Batch {
        status: Ok(repository_status_response()),
        locate: Ok(locate_response()),
    })
}

fn assert_capability_rejection(
    error: &ToolExecutionError,
    code: ErrorCode,
    field_path: &str,
    reason: &str,
) {
    let public = error.public_error().expect("rejection is public");
    assert_eq!(public.code(), code);
    assert_eq!(
        public
            .details()
            .get(&DetailKey::parse("field_path").expect("static key is valid")),
        Some(&PublicValue::Label(
            SafeLabel::parse(field_path).expect("field path is valid")
        ))
    );
    assert_eq!(
        public
            .details()
            .get(&DetailKey::parse("capability_reason").expect("static key is valid")),
        Some(&PublicValue::Label(
            SafeLabel::parse(reason).expect("reason is valid")
        ))
    );
}

fn executor_failure<T>() -> Result<T, ClientPortError> {
    Err(ClientPortError::Executor)
}

fn admission_harness(tool: VerticalTool) -> Harness {
    match tool {
        VerticalTool::RepoIndex => Harness::new(FakeOutcome::RepositoryIndex(executor_failure())),
        VerticalTool::RepoStatus => Harness::new(FakeOutcome::RepositoryStatus(executor_failure())),
        VerticalTool::RepoList => Harness::new(FakeOutcome::RepositoryList(executor_failure())),
        VerticalTool::OperationStatus => {
            Harness::new(FakeOutcome::OperationStatus(executor_failure()))
        }
        VerticalTool::CodeLocate => Harness::new(FakeOutcome::CodeLocate(executor_failure())),
        VerticalTool::SymbolExplain => Harness::new(FakeOutcome::SymbolExplain(executor_failure())),
        VerticalTool::SymbolRelationships => {
            Harness::new(FakeOutcome::SymbolRelationships(executor_failure()))
        }
        VerticalTool::FlowTrace => Harness::new(FakeOutcome::FlowTrace(executor_failure())),
        VerticalTool::ChangeImpact => Harness::new(FakeOutcome::ChangeImpact(executor_failure())),
        VerticalTool::TestsSelect => Harness::new(FakeOutcome::TestsSelect(executor_failure())),
        VerticalTool::ArchitectureOverview => {
            Harness::new(FakeOutcome::ArchitectureOverview(executor_failure()))
        }
        VerticalTool::ArchitectureCycles => {
            Harness::new(FakeOutcome::ArchitectureCycles(executor_failure()))
        }
        VerticalTool::CodeDead => Harness::new(FakeOutcome::CodeDead(executor_failure())),
        VerticalTool::HistoryCompare => {
            Harness::new(FakeOutcome::HistoryCompare(executor_failure()))
        }
        VerticalTool::PlanChange => Harness::new(FakeOutcome::PlanChange(executor_failure())),
        VerticalTool::ContextPack => Harness::new(FakeOutcome::SymbolExplain(executor_failure())),
        VerticalTool::SourceRead => Harness::new(FakeOutcome::SourceRead(executor_failure())),
        VerticalTool::QueryAdvanced => Harness::new(FakeOutcome::QueryAdvanced(executor_failure())),
        VerticalTool::QueryBatch => batch_harness(),
    }
}

fn explain_response(definition: client::SourceReference) -> SymbolExplainPortResponse {
    SymbolExplainPortResponse::new(
        client::SymbolExplain {
            context: context(2, 0),
            symbols: vec![ClientExplanation {
                symbol: symbol(),
                kind: "function".to_owned(),
                display_name: "publish".to_owned(),
                signature: Some("fn publish()".to_owned()),
                definition,
                outbound_exact: 1,
                outbound_candidates: 2,
                inbound_exact: 3,
                inbound_candidates: 4,
                references_exact: 5,
                provider: "treesitter-rust".to_owned(),
                evidence: "syntax".to_owned(),
                confidence: 950,
            }],
            unresolved_symbols: vec![missing_symbol()],
            truncated: false,
        },
        metadata("trace-explain-1"),
    )
}

fn source_read_response(source: client::SourceReference) -> SourceReadPortResponse {
    assert_eq!(source.byte_range(), 4..12);
    SourceReadPortResponse::new(
        client::SourceRead {
            context: context(1, 8),
            chunks: vec![ClientSourceChunk {
                source,
                path: "src/lib.rs".to_owned(),
                start_byte: 4,
                end_byte: 12,
                start_line: 2,
                end_line: 2,
                content: "xxxxxxxx".to_owned(),
                content_hash: content_hash(),
                language: "rust".to_owned(),
                generated: false,
            }],
            total_source_bytes: 8,
            truncated: false,
        },
        metadata("trace-source-compose"),
        Vec::new(),
        Vec::new(),
    )
}

async fn assert_source_reference_composes_with_read(
    source_ref: Value,
    expected: client::SourceReference,
) {
    let harness = Harness::new(FakeOutcome::SourceRead(Ok(source_read_response(
        expected.clone(),
    ))));
    let calls = Arc::clone(&harness.calls);
    let router = ToolRouter::new(
        harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("tool catalog compiles");
    let response = router
        .handle(
            operating_request(json!({
                "name": "source.read",
                "arguments": {
                    "repository": {"repository_id": repository()},
                    "generation": generation(),
                    "references": [{"source_ref": source_ref.clone()}]
                }
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(result) = response else {
        panic!("source.read returns an MCP tool result");
    };

    assert_eq!(
        result["isError"], false,
        "source.read accepts the exact returned source_ref"
    );
    assert!(
        source_ref.get("line_hint").is_none(),
        "an unavailable line hint is omitted"
    );
    let calls = calls.lock().expect("fake call recorder is not poisoned");
    let [ObservedCall::SourceRead(request)] = calls.as_slice() else {
        panic!("source.read reaches the daemon port exactly once");
    };
    assert_eq!(request.references, [expected]);
}

#[tokio::test]
async fn maps_repository_index_without_replacing_stable_identities() {
    let response = RepositoryIndexPortResponse::new(
        RepositoryIndex {
            repository: repository(),
            operation: operation(),
            state: ClientOperationState::Succeeded,
            revision: 8,
            parent_generation: Some(parent_generation()),
            published_generation: Some(generation()),
            discovered_inputs: 4,
            indexed_files: 3,
            entities: 12,
            elapsed_micros: 500,
        },
        IndexPlanSummary {
            scope: IndexPlanScope::Repository,
            mode: IndexMode::Structural,
            providers: vec!["treesitter-rust".to_owned()],
            parent_generation: RequiredNullable(Some(parent_generation())),
            estimated_disk_bytes: 4_096,
        },
        Vec::new(),
    );
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Ok(response)));
    let output: RepoIndexOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoIndex,
            json!({
                "root": "C:/fixture",
                "mode": "structural",
                "scope": {"repository": "whole"},
                "detached": true
            }),
        )
        .await
        .expect("repository index maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected repository index success");
    };
    assert_eq!(output.data.repository_id, repository());
    assert_eq!(output.data.operation_id, operation());
    assert_eq!(output.data.state, OperationState::Published);
    assert_eq!(output.data.published_generation.0, Some(generation()));
    assert_eq!(output.data.accepted_plan.providers, ["treesitter-rust"]);
    assert!(matches!(
        harness.only_call(),
        ObservedCall::RepositoryIndex(RepositoryIndexPortRequest {
            mode: IndexMode::Structural,
            detached: true,
            ..
        })
    ));
}

#[tokio::test]
async fn repository_auto_mode_reports_the_selected_structural_plan() {
    let response = RepositoryIndexPortResponse::new(
        RepositoryIndex {
            repository: repository(),
            operation: operation(),
            state: ClientOperationState::Succeeded,
            revision: 8,
            parent_generation: Some(parent_generation()),
            published_generation: Some(generation()),
            discovered_inputs: 4,
            indexed_files: 3,
            entities: 12,
            elapsed_micros: 500,
        },
        IndexPlanSummary {
            scope: IndexPlanScope::Repository,
            mode: IndexMode::Structural,
            providers: vec!["treesitter-rust".to_owned()],
            parent_generation: RequiredNullable(Some(parent_generation())),
            estimated_disk_bytes: 4_096,
        },
        Vec::new(),
    );
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Ok(response)));

    let output: RepoIndexOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture"}),
        )
        .await
        .expect("auto selects the structural first-slice plan"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repository index success");
    };

    assert_eq!(output.data.accepted_plan.mode, IndexMode::Structural);
    assert!(matches!(
        harness.only_call(),
        ObservedCall::RepositoryIndex(RepositoryIndexPortRequest {
            mode: IndexMode::Auto,
            ..
        })
    ));
}

#[tokio::test]
async fn identical_index_inputs_may_use_fresh_operations_but_converge_generation() {
    let response = |operation| {
        RepositoryIndexPortResponse::new(
            RepositoryIndex {
                repository: repository(),
                operation,
                state: ClientOperationState::Succeeded,
                revision: 8,
                parent_generation: Some(parent_generation()),
                published_generation: Some(generation()),
                discovered_inputs: 4,
                indexed_files: 3,
                entities: 12,
                elapsed_micros: 500,
            },
            IndexPlanSummary {
                scope: IndexPlanScope::Repository,
                mode: IndexMode::Structural,
                providers: vec!["treesitter-rust".to_owned()],
                parent_generation: RequiredNullable(Some(parent_generation())),
                estimated_disk_bytes: 4_096,
            },
            Vec::new(),
        )
    };
    let outcomes = VecDeque::from([Ok(response(operation())), Ok(response(second_operation()))]);
    let harness = Harness::new(FakeOutcome::RepositoryIndexSequence(Arc::new(Mutex::new(
        outcomes,
    ))));
    let arguments = json!({"root": "C:/fixture", "mode": "structural"});

    let first: RepoIndexOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoIndex,
            arguments.clone(),
        )
        .await
        .expect("first index maps"),
    );
    let second: RepoIndexOutput = decode(
        execute(&harness.executor, VerticalTool::RepoIndex, arguments)
            .await
            .expect("second index maps"),
    );
    let (ToolResponse::Success(first), ToolResponse::Success(second)) = (first, second) else {
        panic!("expected repository index successes");
    };

    assert_ne!(first.data.operation_id, second.data.operation_id);
    assert_eq!(
        first.data.published_generation,
        second.data.published_generation
    );
    assert_eq!(first.data.published_generation.0, Some(generation()));
}

#[tokio::test]
async fn maps_operation_status_action_time_progress_and_resources() {
    let response = RepositoryOperationStatus {
        operation: operation_status(ClientOperationState::Running),
        published_generation: None,
        started_unix_ms: 1,
        peak_rss_bytes: 100,
        written_bytes: 200,
        files_examined: 3,
        retry_after_ms: Some(0),
    };
    let harness = Harness::new(FakeOutcome::OperationStatus(Ok(response)));
    let output: OperationStatusOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::OperationStatus,
            json!({
                "operation_id": operation(),
                "action": "cancel",
                "wait_ms": 25,
                "after_revision": 7
            }),
        )
        .await
        .expect("operation status maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected operation status success");
    };
    assert_eq!(output.data.operation.state, OperationState::Running);
    assert_eq!(output.data.operation.started_at, "1970-01-01T00:00:00.001Z");
    assert_eq!(output.data.operation.progress.completed_units, 4);
    assert_eq!(output.data.operation.progress.total_units.0, Some(10));
    assert_eq!(output.data.retry_after_ms.0, Some(0));
    assert_eq!(
        harness.only_call(),
        ObservedCall::OperationStatus(OperationStatusPortRequest {
            operation: operation(),
            action: RepositoryOperationAction::Cancel,
            wait_ms: Some(25),
            after_revision: Some(7),
        })
    );
}

#[tokio::test]
async fn maps_code_locate_with_trust_generation_and_deterministic_output() {
    let response = locate_response();
    let response_debug = format!("{response:?}");
    assert!(!response_debug.contains("publish"));
    assert!(response_debug.contains("query_token_count: 1"));
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "query": "publish",
        "search_modes": ["exact"],
        "max_results": 10,
        "budget": {"max_results": 5},
        "response_profile": "compact"
    });
    let first = execute(
        &harness.executor,
        VerticalTool::CodeLocate,
        arguments.clone(),
    )
    .await
    .expect("first locate maps");
    let second = execute(&harness.executor, VerticalTool::CodeLocate, arguments)
        .await
        .expect("second locate maps");
    assert_eq!(first, second);

    let output: CodeLocateOutput = decode(first);
    let ToolResponse::Success(output) = output else {
        panic!("expected locate success");
    };
    assert_eq!(output.repository.repository_id, repository());
    assert_eq!(output.generation.generation_id, generation());
    assert_eq!(
        output.generation.parent_generation.0,
        Some(parent_generation())
    );
    assert_eq!(output.data.matches[0].symbol_id, Some(symbol()));
    assert_eq!(output.data.matches[0].kind, EntityKind::Type);
    assert_eq!(
        output.data.matches[0].trust,
        TrustClassification::UntrustedRepositoryData
    );
    assert_eq!(output.trust, TrustClassification::UntrustedRepositoryData);
    assert_eq!(output.usage.wall_time_ms, 2);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 2);
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let ObservedCall::CodeLocate(request) = &calls[0] else {
        panic!("expected locate request");
    };
    assert_eq!(request.mode, LocateMode::Exact);
    assert_eq!(request.maximum_results, 5);
    let request_debug = format!("{request:?}");
    assert!(!request_debug.contains("publish"));
    assert!(request_debug.contains("query_bytes: 7"));
}

#[tokio::test]
async fn query_batch_composes_locate_subtools_under_one_pinned_generation() {
    let harness = batch_harness();
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "operations": [
            {"id": "find_a", "tool": "code.locate", "arguments": {"query": "publish", "max_results": 5}},
            {"id": "find_b", "tool": "code.locate", "arguments": {"query": "stage", "max_results": 5}}
        ]
    });
    let output = execute(&harness.executor, VerticalTool::QueryBatch, arguments)
        .await
        .expect("batch executes");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(output.data.generation_id, generation());
    assert_eq!(output.generation.generation_id, generation());
    assert_eq!(output.data.operation_results.len(), 2);
    assert!(
        output
            .data
            .operation_results
            .iter()
            .all(|result| result.status == BatchOperationStatus::Ok)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn every_retained_example_reaches_runtime_without_capability_rejection() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/mcp/1.0/tool-contracts.json"
    ))
    .expect("retained tool contracts are valid JSON");
    let examples = fixture["tools"]
        .as_array()
        .expect("retained tool contracts contain an array");
    assert_eq!(examples.len(), VerticalTool::ALL.len());

    for example in examples {
        let name = example["tool"].as_str().expect("tool name is a string");
        let tool = VerticalTool::ALL
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("retained tool is registered: {name}"));
        let harness = admission_harness(tool);
        let result = execute(&harness.executor, tool, example["input"].clone()).await;
        if let Err(error) = &result
            && let Some(public) = error.public_error()
        {
            assert!(
                !matches!(
                    public.code(),
                    ErrorCode::UnsupportedCapability
                        | ErrorCode::InvalidArgument
                        | ErrorCode::OperatorForbidden
                ),
                "{name} retained example failed admission: {public:?}"
            );
        }
        assert!(
            harness.call_count.load(Ordering::Relaxed) > 0,
            "{name} retained example did not reach the client port"
        );
    }
}

#[tokio::test]
async fn query_batch_enforces_aggregate_budget_across_the_app_boundary() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "generation": "active",
            "operations": [
                {"id": "find_a", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "find_b", "tool": "code.locate", "arguments": {"query": "stage"}}
            ],
            "budget": {"max_tokens": 100}
        }),
    )
    .await
    .expect_err("aggregate child usage exceeds the parent budget");

    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::BudgetExceeded)
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        3,
        "status pinning and both observed children cross the client port"
    );
}

#[tokio::test]
async fn batch_adapter_propagates_cancellation_before_client_dispatch() {
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(locate_response())));
    let public = PublicError::builder(ErrorCode::InvalidArgument, "invalid batch")
        .build()
        .expect("static error is valid");
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::new(
            MaterializedToolValidator::compile().expect("checked contracts compile"),
        ),
        unsupported: public.clone(),
        invalid_arguments: public,
    };
    let (_sender, receiver) = watch::channel(true);
    let request = AgentToolRequest::new(BatchTool::CodeLocate, Map::new());
    let context = AgentCallContext::new(
        RequestCancellation { receiver },
        ResponseBudget {
            max_results: None,
            max_tokens: Some(100),
            max_source_bytes: None,
            max_traversal_facts: None,
            max_depth: None,
            max_paths: None,
            timeout_ms: Some(1_000),
            evidence_level: None,
        },
        None,
    );

    assert_eq!(
        adapter.execute(request, context).await,
        Err(AgentPortError::Cancelled)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn batch_adapter_rejects_unrepresentable_evidence_before_client_dispatch() {
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(locate_response())));
    let public = PublicError::builder(ErrorCode::InvalidArgument, "invalid batch")
        .build()
        .expect("static error is valid");
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::new(
            MaterializedToolValidator::compile().expect("checked contracts compile"),
        ),
        unsupported: public.clone(),
        invalid_arguments: public,
    };
    let (_sender, receiver) = watch::channel(false);
    let request = AgentToolRequest::new(
        BatchTool::CodeLocate,
        Map::from_iter([
            (
                "repository".to_owned(),
                json!({"repository_id": repository()}),
            ),
            ("generation".to_owned(), json!(generation())),
            ("query".to_owned(), json!("publish")),
        ]),
    );
    let local_budget = ResponseBudget {
        max_results: None,
        max_tokens: None,
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: Some(1_000),
        evidence_level: Some(rootlight_mcp_contract::vertical::ProvenanceLevel::Compact),
    };
    let context = AgentCallContext::new(
        RequestCancellation { receiver },
        local_budget.clone(),
        Some(std::time::Instant::now() + std::time::Duration::from_secs(1)),
    )
    .with_local_budget(Some(local_budget));

    let error = adapter
        .execute(request, context)
        .await
        .expect_err("unsupported evidence fails before execution");
    let AgentPortError::Public(error) = error else {
        panic!("expected checked unsupported-capability error");
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn batch_adapter_preserves_local_and_parent_deadline_provenance() {
    for (local, expected) in [
        (true, AgentPortError::LocalDeadlineExceeded),
        (false, AgentPortError::DeadlineExceeded),
    ] {
        let harness = Harness::new(FakeOutcome::PendingCodeLocate);
        let public = PublicError::builder(ErrorCode::InvalidArgument, "invalid batch")
            .build()
            .expect("static error is valid");
        let adapter = McpAgentToolPort {
            port: Arc::clone(&harness.executor.port),
            validator: Arc::new(
                MaterializedToolValidator::compile().expect("checked contracts compile"),
            ),
            unsupported: public.clone(),
            invalid_arguments: public,
        };
        let request = AgentToolRequest::new(
            BatchTool::CodeLocate,
            Map::from_iter([
                (
                    "repository".to_owned(),
                    json!({"repository_id": repository()}),
                ),
                ("generation".to_owned(), json!(generation())),
                ("query".to_owned(), json!("publish")),
            ]),
        );
        let context = AgentCallContext::new(
            cancellation(),
            ResponseBudget {
                max_results: None,
                max_tokens: None,
                max_source_bytes: None,
                max_traversal_facts: None,
                max_depth: None,
                max_paths: None,
                timeout_ms: Some(1),
                evidence_level: None,
            },
            Some(std::time::Instant::now()),
        )
        .with_local_deadline(local);
        assert_eq!(adapter.execute(request, context).await, Err(expected));
    }
}

#[test]
fn production_batch_mapping_covers_every_canonical_eligible_tool() {
    let mapped = [
        (McpTool::CodeLocate, BatchTool::CodeLocate),
        (McpTool::SymbolExplain, BatchTool::SymbolExplain),
        (McpTool::SymbolRelationships, BatchTool::SymbolRelationships),
        (McpTool::FlowTrace, BatchTool::FlowTrace),
        (McpTool::ChangeImpact, BatchTool::ChangeImpact),
        (McpTool::TestsSelect, BatchTool::TestsSelect),
        (
            McpTool::ArchitectureOverview,
            BatchTool::ArchitectureOverview,
        ),
        (McpTool::ArchitectureCycles, BatchTool::ArchitectureCycles),
        (McpTool::CodeDead, BatchTool::CodeDead),
        (McpTool::ContextPack, BatchTool::ContextPack),
        (McpTool::SourceRead, BatchTool::SourceRead),
    ];
    assert_eq!(
        mapped.map(|(tool, _)| tool),
        rootlight_mcp_contract::capability::BATCH_ELIGIBLE
    );
    for (catalog, batch) in mapped {
        let vertical = vertical_tool_for_batch(batch).expect("eligible tool has a dispatch target");
        assert_eq!(vertical.name(), catalog.name());
    }
    assert_eq!(vertical_tool_for_batch(BatchTool::PlanChange), None);
}

#[tokio::test]
async fn query_batch_defers_allowed_bindings_until_runtime_materialization() {
    let harness = batch_harness();
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "operations": [
            {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
            {"id": "refine", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                "query": {"$from": "find", "pointer": "/data/matches/0/symbol_id"}
            }}
        ]
    });
    let output = execute(&harness.executor, VerticalTool::QueryBatch, arguments)
        .await
        .expect("batch executes");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };
    // The dependent operation succeeds only if its binding resolved against the
    // completed dependency response.
    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert!(
        output
            .data
            .operation_results
            .iter()
            .all(|result| result.status == BatchOperationStatus::Ok)
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        3,
        "identity and both child calls prove static preflight deferred the binding"
    );
}

#[tokio::test]
async fn query_batch_skips_dependents_of_an_unavailable_subtool() {
    let harness = batch_harness();
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "operations": [
            {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
            {"id": "rels", "tool": "symbol.relationships", "arguments": {}},
            {"id": "after", "tool": "code.locate", "depends_on": ["rels"], "arguments": {"query": "stage"}}
        ]
    });
    let output = execute(&harness.executor, VerticalTool::QueryBatch, arguments)
        .await
        .expect("batch executes");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    let by_id = |id: &str| {
        output
            .data
            .operation_results
            .iter()
            .find(|result| result.id == id)
            .map(|result| result.status)
    };
    assert_eq!(by_id("find"), Some(BatchOperationStatus::Ok));
    assert_eq!(by_id("rels"), Some(BatchOperationStatus::Error));
    assert_eq!(
        by_id("after"),
        Some(BatchOperationStatus::SkippedDependency)
    );
    // Only the code.locate operation reaches the port.
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn query_batch_keeps_invalid_binding_inside_the_operation_result() {
    let harness = batch_harness();
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "refine", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                    "query": {"$from": "find", "pointer": "/data/matches/99/symbol_id"}
                }}
            ]
        }),
    )
    .await
    .expect("a runtime binding failure stays inside the batch envelope");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success envelope");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output.data.operation_results[1]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::BindingInvalid)
    );
}

#[tokio::test]
async fn query_batch_classifies_bound_value_type_before_subtool_execution() {
    let harness = batch_harness();
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "refine", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                    "query": "publish",
                    "search_modes": {"$from": "find", "pointer": "/data/matches/0/symbol_id"}
                }}
            ]
        }),
    )
    .await
    .expect("a bound type failure stays inside the batch envelope");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success envelope");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output.data.operation_results[1]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::BindingTypeMismatch)
    );
    assert_eq!(
        harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned")
            .iter()
            .filter(|call| matches!(call, ObservedCall::CodeLocate(_)))
            .count(),
        1,
        "the type-invalid dependent operation must not cross the client port"
    );
}

#[tokio::test]
async fn query_batch_does_not_launder_malformed_limits_through_budget_lowering() {
    let overflow =
        serde_json::from_str::<Value>("18446744073709551616").expect("JSON number is valid");
    for malformed in [json!("bad"), json!(-1), json!(1.5), Value::Null, overflow] {
        let harness = batch_harness();
        let output = execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "operations": [{
                    "id": "invalid",
                    "tool": "code.locate",
                    "arguments": {"query": "publish", "max_results": malformed}
                }],
                "budget": {"max_results": 1}
            }),
        )
        .await
        .expect("a child validation failure remains inside the batch envelope");
        let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
            panic!("expected batch success envelope");
        };
        assert_eq!(
            output.data.operation_results[0]
                .error
                .as_ref()
                .map(PublicError::code),
            Some(ErrorCode::InvalidArgument)
        );
        let calls = harness
            .calls
            .lock()
            .expect("fake call recorder is available");
        assert_eq!(
            calls
                .iter()
                .filter(|call| matches!(call, ObservedCall::CodeLocate(_)))
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn unrelated_static_validation_error_is_not_reclassified_as_a_binding_error() {
    let harness = batch_harness();
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "mixed", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                    "query": {"$from": "find", "pointer": "/data/matches/0/symbol_id"},
                    "search_modes": ["not_a_mode"]
                }}
            ]
        }),
    )
    .await
    .expect("the static validation failure remains inside the batch envelope");
    let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
        panic!("expected batch success envelope");
    };
    assert_eq!(
        output.data.operation_results[1]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::InvalidArgument)
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is available");
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, ObservedCall::CodeLocate(_)))
            .count(),
        1,
        "only the independent dependency may cross the client port"
    );
}

#[tokio::test]
async fn explicit_nonactive_generation_fails_before_child_retrieval() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "generation": parent_generation(),
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}}
            ]
        }),
    )
    .await
    .expect_err("active-only status cannot prove an explicit historical generation");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::StaleGeneration)
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is available");
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0], ObservedCall::RepositoryStatus(_)));
}

#[tokio::test]
async fn query_batch_rejects_static_child_capabilities_before_identity_resolution() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "unsupported", "tool": "context.pack", "arguments": {
                    "seeds": {"paths": ["src/lib.rs"]}
                }}
            ]
        }),
    )
    .await
    .expect_err("the batch is rejected before identity resolution");
    assert_capability_rejection(
        &error,
        ErrorCode::UnsupportedCapability,
        "operations.0.arguments.seeds.paths.0",
        "unsupported_field",
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "static rejection must not cross the client port"
    );
}

#[tokio::test]
async fn query_batch_rejects_unproven_restricted_bindings_before_dependencies() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {
                    "query": "publish"
                }},
                {"id": "restricted", "tool": "source.read", "depends_on": ["find"], "arguments": {
                    "response_profile": {
                        "$from": "find",
                        "pointer": "/data/matches/0/symbol_id"
                    }
                }}
            ]
        }),
    )
    .await
    .expect_err("an unproven value-restricted binding rejects the batch");
    assert_capability_rejection(
        &error,
        ErrorCode::UnsupportedCapability,
        "operations.1.arguments.response_profile",
        "unproven_bound_value",
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "dependency and identity calls must not start"
    );
}

#[tokio::test]
async fn query_batch_rejects_bound_unsupported_fields_before_dependencies() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {
                    "query": "publish"
                }},
                {"id": "restricted", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                    "query": "publish",
                    "min_confidence": {
                        "$from": "find",
                        "pointer": "/data/matches/0/symbol_id"
                    }
                }}
            ]
        }),
    )
    .await
    .expect_err("a bound unsupported field rejects the batch");
    assert_capability_rejection(
        &error,
        ErrorCode::UnsupportedCapability,
        "operations.1.arguments.min_confidence",
        "unsupported_field",
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "dependency and identity calls must not start"
    );
}

#[tokio::test]
async fn query_batch_keeps_runtime_child_errors_inside_a_pinned_envelope() {
    let harness = batch_harness();
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "unavailable", "tool": "symbol.relationships", "arguments": {
                    "symbol_ids": [symbol()],
                    "relations": ["calls"]
                }}
            ]
        }),
    )
    .await
    .expect("runtime child failures still produce a batch envelope");
    let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
        panic!("expected batch success envelope");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Error);
    assert_eq!(output.data.generation_id, generation());
    assert_eq!(
        output.data.operation_results[0]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::Internal)
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        2,
        "identity and the runtime child both cross the client port"
    );
}

#[tokio::test]
async fn query_batch_root_gate_allows_timeout_descendants_but_blocks_max_tokens() {
    let timeout_harness = batch_harness();
    let timeout_calls = Arc::clone(&timeout_harness.call_count);
    let timeout_router = ToolRouter::new(
        timeout_harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let timeout_response = timeout_router
        .handle(
            operating_request(json!({
                "name": "query.batch",
                "arguments": {
                    "repository": {"repository_id": repository()},
                    "operations": [{
                        "id": "unsupported",
                        "tool": "context.pack",
                        "arguments": {"seeds": {"paths": ["src/lib.rs"]}},
                        "local_budget": {"timeout_ms": 50}
                    }]
                }
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(timeout_result) = timeout_response else {
        panic!("timeout case returns an MCP tool result");
    };
    assert_eq!(timeout_result["isError"], true);
    assert_eq!(
        timeout_result["structuredContent"]["error"]["details"]["field_path"]["value"],
        "operations.0.arguments.seeds.paths.0",
        "the implemented timeout descendant must reach child preflight"
    );
    assert_eq!(timeout_calls.load(Ordering::Relaxed), 0);

    let max_tokens_harness = batch_harness();
    let max_tokens_calls = Arc::clone(&max_tokens_harness.call_count);
    let max_tokens_router = ToolRouter::new(
        max_tokens_harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let max_tokens_response = max_tokens_router
        .handle(
            operating_request(json!({
                "name": "query.batch",
                "arguments": {
                    "repository": {"repository_id": repository()},
                    "operations": [{
                        "id": "blocked",
                        "tool": "code.locate",
                        "arguments": {"query": "publish"},
                        "local_budget": {"max_tokens": 100}
                    }]
                }
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(max_tokens_result) = max_tokens_response else {
        panic!("max_tokens case returns an MCP tool result");
    };
    assert_eq!(max_tokens_result["isError"], true);
    assert_eq!(
        max_tokens_result["structuredContent"]["error"]["code"],
        "UNSUPPORTED_CAPABILITY"
    );
    assert_eq!(
        max_tokens_result["structuredContent"]["error"]["details"]["field_path"]["value"],
        "operations.0.local_budget.max_tokens"
    );
    assert_eq!(
        max_tokens_result["structuredContent"]["error"]["details"]["capability_reason"]["value"],
        "blocked_field"
    );
    assert_eq!(max_tokens_calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn locate_source_reference_without_line_hint_composes_with_source_read() {
    let source = source_reference_without_lines(4, 12);
    let mut response = locate_response();
    response.result.hits[0].source = Some(source.clone());
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
    let output = Value::Object(
        execute(
            &harness.executor,
            VerticalTool::CodeLocate,
            json!({
                "repository": {"repository_id": repository()},
                "query": "publish"
            }),
        )
        .await
        .expect("locate source reference maps"),
    );
    let source_ref = output
        .pointer("/data/matches/0/source_ref")
        .expect("locate returns exact source evidence")
        .clone();

    assert_source_reference_composes_with_read(source_ref, source).await;
}

#[tokio::test]
async fn active_generation_preserves_independently_observed_stale_freshness() {
    let mut response = locate_response();
    response.metadata.structural_freshness = Freshness::Stale;
    response.metadata.semantic_freshness = Freshness::Stale;
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));

    let output: CodeLocateOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::CodeLocate,
            json!({
                "repository": {"repository_id": repository()},
                "query": "publish"
            }),
        )
        .await
        .expect("active but stale generation maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("successful mapping is expected");
    };

    assert_eq!(output.generation.structural_freshness, Freshness::Stale);
    assert_eq!(output.generation.semantic_freshness, Freshness::Stale);
}

#[tokio::test]
async fn maps_symbol_explain_with_compact_provenance_and_unresolved_ids() {
    let response = explain_response(source_reference(4, 12, 2, 2));
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(response)));
    let output: SymbolExplainOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::SymbolExplain,
            json!({
                "repository": {"repository_id": repository()},
                "symbol_ids": [symbol(), missing_symbol()],
                "include_provenance": "compact",
                "response_profile": "compact"
            }),
        )
        .await
        .expect("symbol explanation maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected symbol explanation success");
    };
    assert_eq!(output.data.symbols[0].symbol_id, symbol());
    assert_eq!(output.data.symbols[0].kind, EntityKind::Function);
    assert_eq!(
        output.data.symbols[0].provenance[0].provider,
        "treesitter-rust"
    );
    assert_eq!(output.data.symbols[0].provenance[0].confidence, 950);
    assert_eq!(output.data.unresolved_ids, [missing_symbol()]);
    assert_eq!(
        output.data.symbols[0].trust,
        TrustClassification::UntrustedRepositoryData
    );
    let ObservedCall::SymbolExplain(request) = harness.only_call() else {
        panic!("expected symbol explain request");
    };
    assert!(request.include_provenance);
    assert_eq!(request.symbols, [symbol(), missing_symbol()]);
}

#[tokio::test]
async fn context_pack_assembles_definition_evidence_under_budget() {
    let response = explain_response(source_reference(4, 12, 2, 2));
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(response)));
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "task": "fix the duplicate payment bug",
        "seeds": {"symbols": [symbol()]},
        "token_budget": 4500
    });
    let first: ContextPackOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::ContextPack,
            arguments.clone(),
        )
        .await
        .expect("context pack maps"),
    );
    let second: ContextPackOutput = decode(
        execute(&harness.executor, VerticalTool::ContextPack, arguments)
            .await
            .expect("context pack maps again"),
    );

    let ToolResponse::Success(pack) = first else {
        panic!("expected context pack success");
    };
    assert_eq!(pack.generation.generation_id, generation());
    assert!(
        !pack.data.items.is_empty(),
        "pack includes definition evidence"
    );
    assert_eq!(pack.data.items[0].symbol_id, Some(symbol()));
    assert!(pack.data.pack_id.as_str().starts_with("pack1_"));
    assert!(!pack.data.followups.is_empty());

    // The pack identity is deterministic for the same generation and request.
    let ToolResponse::Success(second) = second else {
        panic!("expected context pack success");
    };
    assert_eq!(pack.data.pack_id, second.data.pack_id);
}

#[tokio::test]
async fn repo_list_maps_registered_repositories() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: vec![RepositoryListEntry {
            repository_id: repository(),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        }],
    })));
    let output: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 10}),
        )
        .await
        .expect("repo list maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repo list success");
    };
    assert_eq!(output.data.total_count, 1);
    assert_eq!(output.data.repositories.len(), 1);
    assert_eq!(output.data.repositories[0].repository_id, repository());
    assert_eq!(output.data.repositories[0].state, RepositoryState::Ready);
    assert_eq!(
        output.data.repositories[0].active_generation.0,
        Some(generation())
    );
}

#[tokio::test]
async fn repo_list_paginates_with_authenticated_cursor() {
    let entries: Vec<RepositoryListEntry> = (0..3u8)
        .map(|i| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([i + 1; 16]),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        })
        .collect();
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: entries,
    })));
    let first: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2}),
        )
        .await
        .expect("first page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    assert_eq!(first.data.total_count, 3);
    assert_eq!(first.data.repositories.len(), 2);
    assert!(first.truncated);
    let cursor = first
        .next_cursor
        .0
        .expect("first page has a continuation cursor");

    let second: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2, "cursor": cursor.as_str()}),
        )
        .await
        .expect("second page maps"),
    );
    let ToolResponse::Success(second) = second else {
        panic!("expected second page success");
    };
    assert_eq!(second.data.repositories.len(), 1);
    assert!(!second.truncated);
    assert!(second.next_cursor.0.is_none());
}

#[test]
fn repo_list_query_normalization_is_case_folded_and_canonical() {
    assert_eq!(
        normalize_repo_list_query(Some("Straße".to_owned())),
        Some("strasse".to_owned())
    );
    assert_eq!(
        normalize_repo_list_query(Some("STRASSE".to_owned())),
        Some("strasse".to_owned())
    );
    assert_eq!(
        normalize_repo_list_query(Some("e\u{301}".to_owned())),
        normalize_repo_list_query(Some("é".to_owned()))
    );
    assert_eq!(normalize_repo_list_query(Some(String::new())), None);
    assert_eq!(normalize_repo_list_query(None), None);
}

#[tokio::test]
async fn repo_list_cursor_accepts_equivalent_normalized_query() {
    let entries: Vec<RepositoryListEntry> = (0..3_u8)
        .map(|index| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([index + 1; 16]),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        })
        .collect();
    let harness = Harness::with_cursor_key(
        FakeOutcome::RepositoryList(Ok(RepositoryList {
            repositories: entries,
        })),
        [7; 32],
    );
    let first: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"query": "Straße", "max_results": 2}),
        )
        .await
        .expect("first page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    let cursor = first
        .next_cursor
        .0
        .expect("first page has a continuation cursor");

    let second: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"query": "STRASSE", "max_results": 2, "cursor": cursor.as_str()}),
        )
        .await
        .expect("equivalent query resumes the cursor"),
    );
    let ToolResponse::Success(second) = second else {
        panic!("expected second page success");
    };
    assert_eq!(second.data.repositories.len(), 1);

    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        let ObservedCall::RepositoryList(request) = call else {
            panic!("expected only repository list calls");
        };
        assert_eq!(request.query(), Some("strasse"));
    }
}

#[tokio::test]
async fn cursor_signed_with_one_key_is_rejected_under_another() {
    let entries: Vec<RepositoryListEntry> = (0..3u8)
        .map(|i| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([i + 1; 16]),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        })
        .collect();
    let list = || {
        FakeOutcome::RepositoryList(Ok(RepositoryList {
            repositories: entries.clone(),
        }))
    };
    let first: RepoListOutput = decode(
        execute(
            &Harness::with_cursor_key(list(), [7_u8; 32]).executor,
            VerticalTool::RepoList,
            json!({"max_results": 2}),
        )
        .await
        .expect("first page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    let cursor = first
        .next_cursor
        .0
        .expect("first page has a continuation cursor");

    // The same cursor under a different signing key must fail as invalid.
    let error = execute(
        &Harness::with_cursor_key(list(), [9_u8; 32]).executor,
        VerticalTool::RepoList,
        json!({"max_results": 2, "cursor": cursor.as_str()}),
    )
    .await
    .expect_err("cursor signed under another key is rejected");
    let public = error
        .public_error()
        .expect("cursor failure is a checked public error");
    assert_eq!(public.code(), ErrorCode::InvalidCursor);
}

#[tokio::test]
async fn cursor_is_rejected_after_executor_process_restart() {
    const CHILD_MODE: &str = "ROOTLIGHT_CURSOR_RESTART_CHILD";
    const FIXTURE_PATH: &str = "ROOTLIGHT_CURSOR_RESTART_FIXTURE";
    const TEST_NAME: &str = "executor::tests::cursor_is_rejected_after_executor_process_restart";

    let entries = || {
        (0..3_u8)
            .map(|index| RepositoryListEntry {
                repository_id: RepositoryId::from_bytes([index + 1; 16]),
                active_generation: generation(),
                languages: vec!["rust".to_owned()],
                structural_freshness: "current".to_owned(),
                semantic_freshness: "current".to_owned(),
                state: "ready".to_owned(),
            })
            .collect()
    };

    if let Some(mode) = std::env::var_os(CHILD_MODE) {
        let fixture = std::env::var_os(FIXTURE_PATH)
            .map(std::path::PathBuf::from)
            .expect("child fixture path is present");
        let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
            repositories: entries(),
        })));
        if mode == "issue" {
            let output: RepoListOutput = decode(
                execute(
                    &harness.executor,
                    VerticalTool::RepoList,
                    json!({"max_results": 2}),
                )
                .await
                .expect("issuing process returns a first page"),
            );
            let ToolResponse::Success(output) = output else {
                panic!("expected first page success");
            };
            let cursor = output
                .next_cursor
                .0
                .expect("issuing process returns a continuation");
            std::fs::write(fixture, cursor.as_str()).expect("child writes cursor fixture");
            return;
        }

        let cursor = std::fs::read_to_string(&fixture).expect("restart process reads cursor");
        let error = execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2, "cursor": cursor}),
        )
        .await
        .expect_err("a restarted executor rejects the retired process key");
        let public = error
            .public_error()
            .expect("restart failure is a checked public error");
        assert_eq!(public.code(), ErrorCode::InvalidCursor);
        assert!(
            public
                .next_actions()
                .contains(&NextAction::RestartEnumeration)
        );
        std::fs::write(fixture, "invalid_cursor:restart_enumeration")
            .expect("child writes restart result");
        return;
    }

    let directory = tempfile::tempdir().expect("restart fixture directory exists");
    let fixture = directory.path().join("cursor.txt");
    let executable = std::env::current_exe().expect("test executable path is available");
    for mode in ["issue", "validate"] {
        let output = Command::new(&executable)
            .args(["--exact", TEST_NAME])
            .env(CHILD_MODE, mode)
            .env(FIXTURE_PATH, &fixture)
            .output()
            .expect("cursor child process starts");
        assert!(
            output.status.success(),
            "{mode} child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        std::fs::read_to_string(fixture).expect("parent reads restart result"),
        "invalid_cursor:restart_enumeration"
    );
}

#[test]
fn executor_fails_closed_when_cursor_key_provider_fails() {
    struct FailingProvider;

    impl CursorKeyProvider for FailingProvider {
        fn load(&self) -> Result<CursorSigningKey, ToolExecutorBuildError> {
            Err(ToolExecutorBuildError::CursorKeyInitialization)
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let call_count = Arc::new(AtomicUsize::new(0));
    let port = FakePort {
        outcome: FakeOutcome::RepositoryList(Ok(RepositoryList {
            repositories: Vec::new(),
        })),
        calls,
        call_count,
    };
    assert!(matches!(
        FirstSliceToolExecutor::with_cursor_key_provider(port, &FailingProvider),
        Err(ToolExecutorBuildError::CursorKeyInitialization)
    ));
}

#[test]
fn executor_rejects_all_zero_cursor_key_material() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let call_count = Arc::new(AtomicUsize::new(0));
    let port = FakePort {
        outcome: FakeOutcome::RepositoryList(Ok(RepositoryList {
            repositories: Vec::new(),
        })),
        calls,
        call_count,
    };
    assert!(matches!(
        FirstSliceToolExecutor::with_cursor_key(port, [0; 32]),
        Err(ToolExecutorBuildError::CursorKeyInitialization)
    ));
}

#[test]
fn cursor_signing_key_debug_output_redacts_secret_material() {
    let key = CursorSigningKey::deterministic([0xAB; 32]).expect("test key is valid");
    let debug = format!("{key:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("[171"));
    assert!(!debug.contains("171, 171"));
}

#[tokio::test]
async fn repo_list_cursor_is_rejected_after_catalog_snapshot_changes() {
    let entries = |count: u8| {
        (0..count)
            .map(|index| RepositoryListEntry {
                repository_id: RepositoryId::from_bytes([index + 1; 16]),
                active_generation: generation(),
                languages: vec!["rust".to_owned()],
                structural_freshness: "current".to_owned(),
                semantic_freshness: "current".to_owned(),
                state: "ready".to_owned(),
            })
            .collect()
    };
    let first: RepoListOutput = decode(
        execute(
            &Harness::with_cursor_key(
                FakeOutcome::RepositoryList(Ok(RepositoryList {
                    repositories: entries(3),
                })),
                [7; 32],
            )
            .executor,
            VerticalTool::RepoList,
            json!({"max_results": 2}),
        )
        .await
        .expect("first page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    let cursor = first
        .next_cursor
        .0
        .expect("first page has a continuation cursor");

    let error = execute(
        &Harness::with_cursor_key(
            FakeOutcome::RepositoryList(Ok(RepositoryList {
                repositories: entries(4),
            })),
            [7; 32],
        )
        .executor,
        VerticalTool::RepoList,
        json!({"max_results": 2, "cursor": cursor.as_str()}),
    )
    .await
    .expect_err("catalog drift invalidates the cursor");
    assert_eq!(
        error
            .public_error()
            .expect("snapshot mismatch is a checked public error")
            .code(),
        ErrorCode::InvalidCursor
    );
}

#[tokio::test]
async fn every_repo_list_cursor_failure_category_maps_to_invalid_cursor() {
    let entries: Vec<RepositoryListEntry> = (0..3_u8)
        .map(|index| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([index + 1; 16]),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        })
        .collect();
    let signing_key = CursorSigningKey::deterministic([7; 32]).expect("test key is valid");
    let snapshot = repo_list_snapshot_id(&entries);
    let base = repo_list_cursor_context(None, 2, snapshot, signing_key.key_id);
    let wire = |context: CursorContext, issued_at_ms| {
        AuthenticatedCursor::create(
            context,
            2_u32.to_le_bytes().to_vec(),
            issued_at_ms,
            &signing_key.secret,
        )
        .expect("test cursor fits")
        .to_wire()
    };
    let current = wire(base.clone(), now_unix_ms());
    let mut tampered = current.clone().into_bytes();
    let last = tampered
        .last_mut()
        .expect("cursor always has an encoded payload");
    *last = if *last == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).expect("base64url remains UTF-8");

    let mut wrong_key_id = base.clone();
    wrong_key_id.key_id = wrong_key_id.key_id.saturating_add(1);
    let mut wrong_tool_major = base.clone();
    wrong_tool_major.tool_major_version = 2;
    let mut wrong_tool = base.clone();
    wrong_tool.tool = McpTool::RepoStatus;
    let mut wrong_repository = base.clone();
    wrong_repository.repository = repository();
    let mut wrong_generation = base.clone();
    wrong_generation.generation = generation();
    let mut wrong_request = base.clone();
    wrong_request.query_fingerprint = repo_list_fingerprint(Some("other"));
    let mut wrong_plan = base.clone();
    wrong_plan.plan_fingerprint = [9; 32];
    let mut wrong_profile = base.clone();
    wrong_profile.response_profile = ResponseProfile::Standard;
    let mut wrong_page_size = base.clone();
    wrong_page_size.page_size = 3;
    let mut stale_snapshot = base.clone();
    stale_snapshot.snapshot_id = [9; 32];

    let cases = [
        ("malformed", "c2.A".to_owned()),
        ("legacy version", "c1.AAAA".to_owned()),
        ("tampered", tampered),
        ("expired", wire(base, now_unix_ms().saturating_sub(400_000))),
        (
            "future issue time",
            wire(
                repo_list_cursor_context(None, 2, snapshot, signing_key.key_id),
                now_unix_ms().saturating_add(60_000),
            ),
        ),
        ("unknown key", wire(wrong_key_id, now_unix_ms())),
        ("wrong tool", wire(wrong_tool, now_unix_ms())),
        ("wrong tool major", wire(wrong_tool_major, now_unix_ms())),
        ("wrong repository", wire(wrong_repository, now_unix_ms())),
        ("wrong generation", wire(wrong_generation, now_unix_ms())),
        ("wrong request", wire(wrong_request, now_unix_ms())),
        ("wrong plan", wire(wrong_plan, now_unix_ms())),
        ("wrong profile", wire(wrong_profile, now_unix_ms())),
        ("wrong page size", wire(wrong_page_size, now_unix_ms())),
        ("stale snapshot", wire(stale_snapshot, now_unix_ms())),
    ];

    for (label, cursor) in cases {
        let harness = Harness::with_cursor_key(
            FakeOutcome::RepositoryList(Ok(RepositoryList {
                repositories: entries.clone(),
            })),
            [7; 32],
        );
        let error = execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2, "cursor": cursor}),
        )
        .await
        .expect_err(label);
        assert_eq!(
            error
                .public_error()
                .expect("cursor failures are checked public errors")
                .code(),
            ErrorCode::InvalidCursor,
            "{label}"
        );
    }
}

#[tokio::test]
async fn repo_list_rejects_a_malformed_cursor() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: vec![RepositoryListEntry {
            repository_id: repository(),
            active_generation: generation(),
            languages: vec!["rust".to_owned()],
            structural_freshness: "current".to_owned(),
            semantic_freshness: "current".to_owned(),
            state: "ready".to_owned(),
        }],
    })));
    let result = execute(
        &harness.executor,
        VerticalTool::RepoList,
        json!({"max_results": 2, "cursor": "c1.AAAA"}),
    )
    .await;
    let error = result.expect_err("a malformed cursor is rejected");
    let public = error
        .public_error()
        .expect("malformed cursor is a checked public error");
    assert_eq!(
        public.code(),
        ErrorCode::InvalidCursor,
        "cursor failures map to INVALID_CURSOR, not a generic argument or internal error"
    );
}

#[tokio::test]
async fn executor_maps_malformed_argument_types_to_type_mismatch() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: vec![],
    })));
    let error = execute(
        &harness.executor,
        VerticalTool::RepoList,
        json!({"max_results": "not-a-number"}),
    )
    .await
    .expect_err("malformed arguments are rejected");
    let public = error
        .public_error()
        .expect("malformed arguments are a checked public error");
    assert_eq!(
        public.code(),
        ErrorCode::TypeMismatch,
        "argument decoding failures are client-correctable, not internal"
    );
}

#[tokio::test]
async fn repo_status_maps_active_generation_and_coverage() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: Some(parent_generation()),
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![RepositoryCoverageEntry {
            language: "rust".to_owned(),
            tier: "tier_a".to_owned(),
            status: "complete".to_owned(),
            discovered_files: 3,
            indexed_files: 3,
        }],
    })));
    let output: RepoStatusOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoStatus,
            json!({"repository": {"repository_id": repository()}}),
        )
        .await
        .expect("repo status maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repo status success");
    };
    assert_eq!(output.data.repository_state, RepositoryState::Ready);
    assert_eq!(
        output
            .data
            .active_generation
            .0
            .expect("active generation")
            .generation_id,
        generation()
    );
    assert_eq!(output.data.coverage.indexed_files, 3);
    assert_eq!(output.data.coverage.languages[0].tier, "A");
}

#[tokio::test]
async fn symbol_relationships_maps_groups_and_totals() {
    let response = SymbolRelationshipsPortResponse::new(
        ClientRelationships {
            context: context(1, 0),
            groups: vec![ClientRelationshipGroup {
                seed: symbol(),
                relation: "calls".to_owned(),
                direction: "outbound".to_owned(),
                items: vec![ClientRelationshipTarget {
                    symbol: missing_symbol(),
                    confidence: 900,
                    source_refs: vec![source_reference(0, 10, 1, 1)],
                }],
                total_count: 1,
            }],
            returned_edges: 1,
            total_edges: 1,
            exact: true,
            truncated: false,
        },
        metadata("trace-rel-1"),
    );
    let harness = Harness::new(FakeOutcome::SymbolRelationships(Ok(response)));
    let output: SymbolRelationshipsOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::SymbolRelationships,
            json!({
                "repository": {"repository_id": repository()},
                "symbol_ids": [symbol()],
                "relations": ["calls"]
            }),
        )
        .await
        .expect("symbol relationships maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected symbol relationships success");
    };
    assert_eq!(output.data.groups.len(), 1);
    let group = &output.data.groups[0];
    assert_eq!(group.seed, symbol());
    assert_eq!(group.relation, RelationKind::Calls);
    assert_eq!(group.direction, Direction::Outbound);
    assert_eq!(group.total_count, 1);
    assert_eq!(group.items.len(), 1);
    assert_eq!(group.items[0].symbol_id, missing_symbol());
    assert_eq!(group.items[0].confidence, 900);
    assert_eq!(group.items[0].source_refs.len(), 1);
    assert_eq!(output.data.totals.returned_edges, 1);
    assert_eq!(output.data.totals.total_edges, 1);
    assert!(output.data.totals.exact);
    assert!(output.data.unresolved.is_empty());
    let ObservedCall::SymbolRelationships(request) = harness.only_call() else {
        panic!("expected symbol relationships call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.seeds(), &[symbol()]);
    assert_eq!(request.relations(), &["calls".to_owned()]);
}

#[tokio::test]
async fn flow_trace_maps_paths_frontier_and_projection() {
    let response = FlowTracePortResponse::new(
        ClientFlowTrace {
            context: context(1, 0),
            paths: vec![ClientTracePath {
                confidence: 800,
                nodes: vec![symbol(), missing_symbol()],
                edges: vec![ClientTraceEdge {
                    kind: "calls".to_owned(),
                    confidence: 800,
                    source_refs: vec![source_reference(0, 10, 1, 1)],
                }],
                cyclic: false,
            }],
            frontier: ClientTraceFrontier {
                reached_nodes: 2,
                examined_edges: 1,
                truncated: false,
                unresolved_boundaries: 0,
            },
            projection: ClientTraceProjection {
                relations: vec!["calls".to_owned()],
                min_confidence: 0,
            },
        },
        metadata("trace-flow-1"),
    );
    let harness = Harness::new(FakeOutcome::FlowTrace(Ok(response)));
    let output: FlowTraceOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::FlowTrace,
            json!({
                "repository": {"repository_id": repository()},
                "from": {"symbol_id": symbol()},
                "relations": ["calls"]
            }),
        )
        .await
        .expect("flow trace maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected flow trace success");
    };
    assert_eq!(output.data.paths.len(), 1);
    let path = &output.data.paths[0];
    assert_eq!(path.confidence, 800);
    assert_eq!(path.nodes, vec![symbol(), missing_symbol()]);
    assert_eq!(path.edges.len(), 1);
    assert_eq!(path.edges[0].kind, RelationKind::Calls);
    assert_eq!(path.edges[0].confidence, 800);
    assert_eq!(path.edges[0].source_refs.len(), 1);
    assert!(!path.cyclic);
    assert_eq!(output.data.frontier.reached_nodes, 2);
    assert_eq!(output.data.frontier.examined_edges, 1);
    assert!(!output.data.frontier.truncated);
    assert_eq!(output.data.frontier.unresolved_boundaries, 0);
    assert_eq!(output.data.projection.relations.len(), 1);
    assert!(
        output
            .data
            .projection
            .relations
            .contains(&RelationKind::Calls)
    );
    assert_eq!(output.data.projection.min_confidence, 0);
    let ObservedCall::FlowTrace(request) = harness.only_call() else {
        panic!("expected flow trace call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.from(), symbol());
    assert_eq!(request.to(), None);
    assert_eq!(request.relations(), &["calls".to_owned()]);
}

#[tokio::test]
async fn architecture_cycles_maps_components_cycles_and_breaks() {
    let response = ArchitectureCyclesPortResponse::new(
        ClientArchitectureCycles {
            context: context(1, 0),
            components: vec![ClientCycleComponent {
                size: 2,
                members: vec![symbol(), missing_symbol()],
                internal_edges: 2,
            }],
            cycles: vec![ClientCycle {
                nodes: vec![symbol(), missing_symbol(), symbol()],
                edge_evidence: vec![source_reference(0, 10, 1, 1)],
                confidence: 700,
            }],
            break_candidates: vec![ClientCycleBreak {
                from: missing_symbol(),
                to: symbol(),
                kind: "calls".to_owned(),
                break_cost: 700,
                source_refs: vec![source_reference(0, 10, 1, 1)],
            }],
            projection: ClientCycleProjection {
                relations: vec!["calls".to_owned()],
                min_confidence: 0,
            },
        },
        metadata("architecture-cycles-1"),
    );
    let harness = Harness::new(FakeOutcome::ArchitectureCycles(Ok(response)));
    let output: ArchitectureCyclesOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::ArchitectureCycles,
            json!({
                "repository": {"repository_id": repository()},
                "projection": {"relations": ["calls"], "level": "symbol"}
            }),
        )
        .await
        .expect("architecture cycles maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected architecture cycles success");
    };
    assert_eq!(output.data.components.len(), 1);
    let component = &output.data.components[0];
    assert_eq!(component.size, 2);
    assert_eq!(
        component.members,
        vec![symbol().to_string(), missing_symbol().to_string()]
    );
    assert_eq!(component.internal_edges, 2);
    assert_eq!(output.data.cycles.len(), 1);
    let cycle = &output.data.cycles[0];
    assert_eq!(
        cycle.nodes,
        vec![
            symbol().to_string(),
            missing_symbol().to_string(),
            symbol().to_string()
        ]
    );
    assert_eq!(cycle.confidence, 700);
    assert_eq!(cycle.edge_evidence.len(), 1);
    assert_eq!(output.data.break_candidates.len(), 1);
    let candidate = &output.data.break_candidates[0];
    assert_eq!(candidate.from, missing_symbol().to_string());
    assert_eq!(candidate.to, symbol().to_string());
    assert_eq!(candidate.kind, RelationKind::Calls);
    assert_eq!(candidate.break_cost, 700);
    assert_eq!(candidate.source_refs.len(), 1);
    let ObservedCall::ArchitectureCycles(request) = harness.only_call() else {
        panic!("expected architecture cycles call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.relations(), &["calls".to_owned()]);
    assert_eq!(request.min_size(), None);
    assert_eq!(request.max_cycles(), None);
    assert_eq!(request.include_self_cycles(), None);
}

#[tokio::test]
async fn architecture_cycles_rejects_unsupported_ranking() {
    let harness = Harness::new(FakeOutcome::ArchitectureCycles(Err(
        ClientPortError::Executor,
    )));
    let error = execute(
        &harness.executor,
        VerticalTool::ArchitectureCycles,
        json!({
            "repository": {"repository_id": repository()},
            "projection": {"relations": ["calls"], "level": "symbol"},
            "rank_by": "size"
        }),
    )
    .await
    .expect_err("unsupported ranking is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn code_dead_maps_candidates_entry_points_and_blind_spots() {
    let response = CodeDeadPortResponse::new(
        ClientCodeDead {
            context: context(1, 0),
            candidates: vec![ClientDeadCandidate {
                symbol_id: missing_symbol(),
                classification: "proven_dead".to_owned(),
                confidence: 1_000,
                why: vec![
                    "no_incoming_references".to_owned(),
                    "unreachable_from_entry_points".to_owned(),
                ],
                suppressions_checked: vec!["entry_point".to_owned()],
                source_refs: vec![source_reference(0, 10, 1, 1)],
            }],
            entry_points: ClientEntryPointSummary {
                policy: "standard".to_owned(),
                entry_point_count: 2,
                complete: false,
            },
            blind_spots: vec![ClientBlindSpot {
                category: "dynamic_dispatch".to_owned(),
                affected_count: 0,
            }],
            false_positive_controls: vec![ClientSuppressionRule {
                rule: "exported".to_owned(),
                suppressed_count: 2,
            }],
        },
        metadata("code-dead-1"),
    );
    let harness = Harness::new(FakeOutcome::CodeDead(Ok(response)));
    let output: CodeDeadOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::CodeDead,
            json!({
                "repository": {"repository_id": repository()}
            }),
        )
        .await
        .expect("code dead maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected code dead success");
    };
    assert_eq!(output.data.candidates.len(), 1);
    let candidate = &output.data.candidates[0];
    assert_eq!(candidate.symbol_id, missing_symbol());
    assert_eq!(candidate.classification, DeadClassification::ProvenDead);
    assert_eq!(candidate.confidence, 1_000);
    assert_eq!(
        candidate.why,
        vec![
            "no_incoming_references".to_owned(),
            "unreachable_from_entry_points".to_owned()
        ]
    );
    assert_eq!(
        candidate.suppressions_checked,
        vec!["entry_point".to_owned()]
    );
    assert_eq!(candidate.source_refs.len(), 1);
    assert_eq!(output.data.entry_points.policy, EntryPointPolicy::Standard);
    assert_eq!(output.data.entry_points.entry_point_count, 2);
    assert!(!output.data.entry_points.complete);
    assert_eq!(output.data.blind_spots.len(), 1);
    assert_eq!(output.data.blind_spots[0].category, "dynamic_dispatch");
    assert_eq!(output.data.false_positive_controls.len(), 1);
    assert_eq!(output.data.false_positive_controls[0].rule, "exported");
    let ObservedCall::CodeDead(request) = harness.only_call() else {
        panic!("expected code dead call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.entry_point_policy(), None);
    assert_eq!(request.include_exported(), None);
    assert_eq!(request.include_tests(), None);
    assert_eq!(request.min_confidence(), None);
    assert_eq!(request.max_candidates(), None);
}

#[tokio::test]
async fn code_dead_rejects_unsupported_scope() {
    let harness = Harness::new(FakeOutcome::CodeDead(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::CodeDead,
        json!({
            "repository": {"repository_id": repository()},
            "scope": {"paths": ["src"]}
        }),
    )
    .await
    .expect_err("unsupported scope is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn architecture_overview_maps_components_connections_and_hotspots() {
    let response = ArchitectureOverviewPortResponse::new(
        ClientArchitectureOverview {
            context: context(1, 0),
            components: vec![ClientArchitectureComponent {
                id: "file-a".to_owned(),
                kind: "file".to_owned(),
                name: "src/a.rs".to_owned(),
                symbol_count: 2,
                responsibility_evidence: vec!["contains_symbols".to_owned()],
                confidence: 800,
            }],
            connections: vec![ClientArchitectureConnection {
                from: "file-a".to_owned(),
                to: "file-b".to_owned(),
                kind: "calls".to_owned(),
                weight: 2,
                confidence: 900,
            }],
            hotspots: vec![ClientArchitectureHotspot {
                component_id: "file-b".to_owned(),
                fan_in: 1,
                fan_out: 0,
                change_frequency: None,
                complexity: None,
                score: 1_000,
            }],
            views: vec![ClientDerivedView {
                view: "hotspots".to_owned(),
                algorithm_version: "fan_in_out_v1".to_owned(),
            }],
        },
        metadata("architecture-overview-1"),
    );
    let harness = Harness::new(FakeOutcome::ArchitectureOverview(Ok(response)));
    let output: ArchitectureOverviewOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::ArchitectureOverview,
            json!({
                "repository": {"repository_id": repository()},
                "views": ["hotspots"]
            }),
        )
        .await
        .expect("architecture overview maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected architecture overview success");
    };
    assert_eq!(output.data.components.len(), 1);
    let component = &output.data.components[0];
    assert_eq!(component.id, "file-a");
    assert_eq!(component.kind, "file");
    assert_eq!(component.name, "src/a.rs");
    assert_eq!(component.symbol_count, 2);
    assert_eq!(component.confidence, 800);
    assert_eq!(
        component.trust,
        TrustClassification::UntrustedRepositoryData
    );
    assert_eq!(output.data.connections.len(), 1);
    let connection = &output.data.connections[0];
    assert_eq!(connection.from, "file-a");
    assert_eq!(connection.to, "file-b");
    assert_eq!(connection.kind, RelationKind::Calls);
    assert_eq!(connection.weight, 2);
    assert_eq!(connection.confidence, 900);
    assert_eq!(output.data.hotspots.len(), 1);
    let hotspot = &output.data.hotspots[0];
    assert_eq!(hotspot.component_id, "file-b");
    assert_eq!(hotspot.fan_in, 1);
    assert_eq!(hotspot.fan_out, 0);
    assert_eq!(hotspot.change_frequency, None);
    assert_eq!(hotspot.complexity, None);
    assert_eq!(hotspot.score, 1_000);
    assert_eq!(output.data.views.len(), 1);
    assert_eq!(output.data.views[0].view, ArchitectureView::Hotspots);
    assert_eq!(output.data.views[0].algorithm_version, "fan_in_out_v1");
    let ObservedCall::ArchitectureOverview(request) = harness.only_call() else {
        panic!("expected architecture overview call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.views(), &["hotspots".to_owned()]);
    assert_eq!(request.max_components(), None);
    assert_eq!(request.include_edges(), None);
    assert_eq!(request.min_confidence(), None);
}

#[tokio::test]
async fn architecture_overview_rejects_unsupported_view() {
    let harness = Harness::new(FakeOutcome::ArchitectureOverview(Err(
        ClientPortError::Executor,
    )));
    let error = execute(
        &harness.executor,
        VerticalTool::ArchitectureOverview,
        json!({
            "repository": {"repository_id": repository()},
            "views": ["services"]
        }),
    )
    .await
    .expect_err("unsupported view is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn tests_select_maps_ranked_tests_strategy_and_gaps() {
    let response = TestsSelectPortResponse::new(
        ClientTestsSelect {
            context: context(1, 0),
            tests: vec![ClientRankedTest {
                test_id: "test-1".to_owned(),
                kind: "unit".to_owned(),
                path: Some("src/a.rs".to_owned()),
                score: 970,
                why: vec!["direct_test_edge".to_owned(), "via:calls".to_owned()],
                estimated_cost_ms: None,
                command_hint: Some("test:unit".to_owned()),
            }],
            coverage_strategy: ClientCoverageStrategy {
                direct_edges: true,
                transitive_signals: false,
                history_signals: false,
                build_target_signals: true,
            },
            gaps: vec![ClientTestGap {
                scope: "scope-1".to_owned(),
                reason: "no_related_test".to_owned(),
            }],
        },
        metadata("tests-select-1"),
    );
    let harness = Harness::new(FakeOutcome::TestsSelect(Ok(response)));
    let output: TestsSelectOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::TestsSelect,
            json!({
                "repository": {"repository_id": repository()},
                "seeds": {"symbols": [symbol()]}
            }),
        )
        .await
        .expect("tests select maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected tests select success");
    };
    assert_eq!(output.data.tests.len(), 1);
    let test = &output.data.tests[0];
    assert_eq!(test.test_id, "test-1");
    assert_eq!(test.kind, TestKind::Unit);
    assert_eq!(test.path.as_deref(), Some("src/a.rs"));
    assert_eq!(test.score, 970);
    assert_eq!(
        test.why,
        vec!["direct_test_edge".to_owned(), "via:calls".to_owned()]
    );
    assert_eq!(test.estimated_cost_ms, None);
    assert_eq!(test.command_hint.as_deref(), Some("test:unit"));
    assert!(output.data.coverage_strategy.direct_edges);
    assert!(!output.data.coverage_strategy.transitive_signals);
    assert!(!output.data.coverage_strategy.history_signals);
    assert!(output.data.coverage_strategy.build_target_signals);
    assert_eq!(output.data.gaps.len(), 1);
    assert_eq!(output.data.gaps[0].scope, "scope-1");
    assert_eq!(output.data.gaps[0].reason.as_str(), "no_related_test");
    let ObservedCall::TestsSelect(request) = harness.only_call() else {
        panic!("expected tests select call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.seeds(), &[symbol()]);
    assert_eq!(request.test_kinds(), &[] as &[String]);
    assert_eq!(request.max_tests(), None);
    assert_eq!(request.include_commands(), None);
}

#[tokio::test]
async fn tests_select_rejects_unsupported_frameworks() {
    let harness = Harness::new(FakeOutcome::TestsSelect(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::TestsSelect,
        json!({
            "repository": {"repository_id": repository()},
            "seeds": {"symbols": [symbol()]},
            "frameworks": ["cargo-nextest"]
        }),
    )
    .await
    .expect_err("unsupported frameworks are rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn change_impact_maps_resolved_changes_impact_groups_and_risk() {
    let response = ChangeImpactPortResponse::new(
        ClientChangeImpact {
            context: context(1, 0),
            resolved_changes: vec![ClientResolvedChange {
                symbol_id: Some(symbol()),
                file_id: Some(file()),
                classification: "body".to_owned(),
                kind: Some("function".to_owned()),
            }],
            impacted: vec![ClientImpactGroup {
                source_index: 0,
                dependents: vec![ClientImpactEntry {
                    symbol_id: missing_symbol(),
                    kind: "function".to_owned(),
                    distance: 1,
                    confidence: 900,
                    via: vec!["calls".to_owned()],
                    is_public: false,
                }],
            }],
            tests: Vec::new(),
            risk_summary: ClientRiskSummary {
                level: "low".to_owned(),
                reasons: vec![
                    "transitive_fanout".to_owned(),
                    "dynamic_dispatch_blind_spot".to_owned(),
                ],
                coverage: "unknown".to_owned(),
                breaking_surface: false,
                fanout: 1,
                dynamic_blind_spots: true,
            },
        },
        metadata("change-impact-1"),
    );
    let harness = Harness::new(FakeOutcome::ChangeImpact(Ok(response)));
    let output: ChangeImpactOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::ChangeImpact,
            json!({
                "repository": {"repository_id": repository()},
                "change": {"symbol_ids": [symbol()]}
            }),
        )
        .await
        .expect("change impact maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected change impact success");
    };
    assert_eq!(output.data.resolved_changes.len(), 1);
    let change = &output.data.resolved_changes[0];
    assert_eq!(change.symbol_id, RequiredNullable(Some(symbol())));
    assert_eq!(change.file_id, RequiredNullable(Some(file())));
    assert_eq!(change.classification, ChangeClassification::Body);
    assert_eq!(change.kind, Some(IrEntityKind::Function));
    assert_eq!(output.data.impacted.len(), 1);
    let group = &output.data.impacted[0];
    assert_eq!(group.source_index, 0);
    assert_eq!(group.dependents.len(), 1);
    let dependent = &group.dependents[0];
    assert_eq!(dependent.symbol_id, missing_symbol());
    assert_eq!(dependent.kind, IrEntityKind::Function);
    assert_eq!(dependent.distance, 1);
    assert_eq!(dependent.confidence, 900);
    assert_eq!(dependent.via, vec!["calls".to_owned()]);
    assert!(!dependent.is_public);
    assert!(output.data.service_impacts.is_empty());
    assert!(output.data.tests.is_empty());
    assert_eq!(output.data.risk_summary.level, RiskLevel::Low);
    assert_eq!(output.data.risk_summary.coverage, IrCoverage::Unknown);
    assert!(!output.data.risk_summary.breaking_surface);
    assert_eq!(output.data.risk_summary.fanout, 1);
    assert!(output.data.risk_summary.dynamic_blind_spots);
    let ObservedCall::ChangeImpact(request) = harness.only_call() else {
        panic!("expected change impact call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.changed_symbols(), &[symbol()]);
    assert_eq!(request.changed_paths(), &[] as &[String]);
    assert_eq!(request.max_depth(), None);
    assert_eq!(request.include_tests(), None);
}

#[tokio::test]
async fn change_impact_rejects_a_revision_range_diff() {
    let harness = Harness::new(FakeOutcome::ChangeImpact(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::ChangeImpact,
        json!({
            "repository": {"repository_id": repository()},
            "change": {"revision_range": "HEAD~1..HEAD"}
        }),
    )
    .await
    .expect_err("revision range diffs are rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn plan_change_maps_steps_impact_summary_decisions_and_context_pack() {
    let response = PlanChangePortResponse::new(
        ClientPlanChange {
            context: context(1, 0),
            plan: vec![
                ClientPlanStep {
                    step: 1,
                    action: "Inspect the target symbols and reproduce the reported defect."
                        .to_owned(),
                    targets: vec![symbol()],
                    depends_on: Vec::new(),
                    risks: Vec::new(),
                    verification: Some("confirm current behavior of the target symbols".to_owned()),
                },
                ClientPlanStep {
                    step: 2,
                    action: "Apply the minimal fix to the target symbols.".to_owned(),
                    targets: vec![symbol()],
                    depends_on: vec![1],
                    risks: vec!["regression".to_owned()],
                    verification: None,
                },
            ],
            affected_scope: ClientPlanImpactSummary {
                affected_symbols: 1,
                affected_files: 1,
                risk_level: "low".to_owned(),
                touches_public_surface: false,
            },
            test_plan: vec![ClientChangeImpactTest {
                test_id: "test-id".to_owned(),
                relevance: 800,
                why: vec!["via:calls".to_owned()],
                estimated_cost_ms: None,
            }],
            open_decisions: vec![ClientPlanDecision {
                question: "confirm_behavior_preservation".to_owned(),
                recommended_default: "preserve_observable_behavior".to_owned(),
            }],
            context_pack_request: ClientPlanContextPack {
                symbols: vec![symbol()],
                files: vec![file()],
            },
        },
        metadata("plan-change-1"),
    );
    let harness = Harness::new(FakeOutcome::PlanChange(Ok(response)));
    let output: PlanChangeOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::PlanChange,
            json!({
                "repository": {"repository_id": repository()},
                "objective": "bug_fix",
                "objective_text": "fix the defect",
                "targets": [{"symbol_id": symbol()}]
            }),
        )
        .await
        .expect("plan change maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected plan change success");
    };
    assert_eq!(output.data.plan.len(), 2);
    assert_eq!(output.data.plan[0].step, 1);
    assert_eq!(output.data.plan[0].targets, vec![symbol()]);
    assert!(output.data.plan[0].depends_on.is_empty());
    assert_eq!(output.data.plan[1].step, 2);
    assert_eq!(output.data.plan[1].depends_on, vec![1]);
    assert_eq!(output.data.plan[1].risks, vec!["regression".to_owned()]);
    assert_eq!(output.data.affected_scope.affected_symbols, 1);
    assert_eq!(output.data.affected_scope.affected_files, 1);
    assert_eq!(output.data.affected_scope.risk_level, RiskLevel::Low);
    assert!(!output.data.affected_scope.touches_public_surface);
    assert_eq!(output.data.test_plan.len(), 1);
    assert_eq!(output.data.test_plan[0].test_id, "test-id");
    assert_eq!(output.data.open_decisions.len(), 1);
    assert_eq!(
        output.data.open_decisions[0].question,
        "confirm_behavior_preservation"
    );
    assert_eq!(output.data.context_pack_request.symbols, vec![symbol()]);
    assert_eq!(output.data.context_pack_request.files, vec![file()]);
    let ObservedCall::PlanChange(request) = harness.only_call() else {
        panic!("expected plan change call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.objective(), "bug_fix");
    assert_eq!(request.objective_text(), "fix the defect");
    assert_eq!(request.target_symbols(), &[symbol()]);
    assert_eq!(request.target_files(), &[] as &[FileId]);
    assert_eq!(request.max_steps(), None);
}

#[tokio::test]
async fn plan_change_rejects_a_change_context() {
    let harness = Harness::new(FakeOutcome::PlanChange(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::PlanChange,
        json!({
            "repository": {"repository_id": repository()},
            "objective": "bug_fix",
            "objective_text": "fix the defect",
            "targets": [{"symbol_id": symbol()}],
            "change_context": {"symbol_ids": [symbol()]}
        }),
    )
    .await
    .expect_err("change context is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn history_compare_maps_changes_breaking_candidates_and_lineage() {
    let response = HistoryComparePortResponse::new(
        ClientHistoryCompare {
            context: context(1, 0),
            matched_states: ClientHistoryMatchedStates {
                base_generation: parent_generation(),
                head_generation: generation(),
                coverage: "bounded".to_owned(),
            },
            changes: vec![ClientHistorySemanticChange {
                kind: "added".to_owned(),
                symbol_id: symbol(),
                entity_kind: "function".to_owned(),
                breaking_candidate: false,
                significance: 200,
            }],
            architecture_delta: ClientHistoryArchitectureDelta {
                new_cross_service_edges: 0,
                removed_cross_service_edges: 0,
                new_boundaries: 0,
                removed_boundaries: 0,
            },
            breaking_candidates: vec![ClientHistoryBreakingCandidate {
                symbol_id: symbol(),
                consumer_count: 3,
                is_public_surface: true,
                reason: "removed_public_surface".to_owned(),
            }],
            lineage: vec![ClientHistoryLineageMatch {
                base_symbol_id: symbol(),
                head_symbol_id: symbol(),
                confidence: 1_000,
                is_rename: false,
            }],
        },
        metadata("history-compare-1"),
    );
    let harness = Harness::new(FakeOutcome::HistoryCompare(Ok(response)));
    let output: HistoryCompareOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::HistoryCompare,
            json!({
                "repository": {"repository_id": repository()},
                "base": parent_generation(),
                "head": generation()
            }),
        )
        .await
        .expect("history compare maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected history compare success");
    };
    assert_eq!(
        output.data.matched_states.base_generation,
        parent_generation()
    );
    assert_eq!(output.data.matched_states.head_generation, generation());
    assert_eq!(output.data.matched_states.coverage, IrCoverage::Bounded);
    assert_eq!(output.data.changes.len(), 1);
    assert_eq!(output.data.changes[0].kind, SemanticChangeKind::Added);
    assert_eq!(output.data.changes[0].symbol_id, symbol());
    assert_eq!(output.data.changes[0].significance, 200);
    assert!(!output.data.changes[0].breaking_candidate);
    assert_eq!(output.data.architecture_delta.new_cross_service_edges, 0);
    assert_eq!(
        output.data.architecture_delta.removed_cross_service_edges,
        0
    );
    assert_eq!(output.data.architecture_delta.new_boundaries, 0);
    assert_eq!(output.data.architecture_delta.removed_boundaries, 0);
    assert_eq!(output.data.breaking_candidates.len(), 1);
    assert_eq!(output.data.breaking_candidates[0].consumer_count, 3);
    assert!(output.data.breaking_candidates[0].is_public_surface);
    assert_eq!(
        output.data.breaking_candidates[0].reason.as_str(),
        "removed_public_surface"
    );
    assert_eq!(output.data.lineage.len(), 1);
    assert_eq!(output.data.lineage[0].base_symbol_id, symbol());
    assert_eq!(output.data.lineage[0].head_symbol_id, symbol());
    assert_eq!(output.data.lineage[0].confidence, 1_000);
    assert!(!output.data.lineage[0].is_rename);
    let ObservedCall::HistoryCompare(request) = harness.only_call() else {
        panic!("expected history compare call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.base(), parent_generation());
    assert_eq!(request.head(), generation());
    assert!(request.change_kinds().is_empty());
    assert_eq!(request.max_results(), None);
}

#[tokio::test]
async fn history_compare_rejects_a_git_revision_selector() {
    let harness = Harness::new(FakeOutcome::HistoryCompare(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::HistoryCompare,
        json!({
            "repository": {"repository_id": repository()},
            "base": {"git": "main"},
            "head": generation()
        }),
    )
    .await
    .expect_err("git revision selector is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn query_advanced_maps_columns_rows_and_completeness() {
    let response = QueryAdvancedPortResponse::new(
        ClientAdvancedQuery {
            context: context(1, 0),
            columns: vec![ClientAdvancedColumn {
                name: "id".to_owned(),
                column_type: "symbol_id".to_owned(),
            }],
            rows: vec![json!({"id": "sym"})],
            plan: None,
            completeness: "complete".to_owned(),
        },
        metadata("query-advanced-1"),
    );
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(response)));
    let output: QueryAdvancedOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryAdvanced,
            json!({
                "repository": {"repository_id": repository()},
                "query": {"op": "scan", "entity": "function"}
            }),
        )
        .await
        .expect("query advanced maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected query advanced success");
    };
    assert_eq!(output.data.columns.len(), 1);
    assert_eq!(output.data.columns[0].name, "id");
    assert_eq!(output.data.columns[0].column_type, ColumnType::SymbolId);
    assert_eq!(output.data.rows.len(), 1);
    assert_eq!(output.data.rows[0], json!({"id": "sym"}));
    assert_eq!(output.data.completeness, QueryCompleteness::Complete);
    assert_eq!(output.data.plan, RequiredNullable(None));
    let ObservedCall::QueryAdvanced(request) = harness.only_call() else {
        panic!("expected query advanced call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(request.max_results(), None);
    assert_eq!(request.explain(), None);
    assert!(request.query_ast().contains("scan"));
}

#[tokio::test]
async fn query_advanced_rejects_a_paging_cursor() {
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::QueryAdvanced,
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "cursor": "abc"
        }),
    )
    .await
    .expect_err("paging cursor is rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn query_advanced_distinguishes_cost_limit_from_capability_and_budget() {
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::QueryAdvanced,
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "cost_limit": 1
        }),
    )
    .await
    .expect_err("a tight cost ceiling is rejected before the port");
    let public = error
        .public_error()
        .expect("cost rejection is a checked public error");
    assert_eq!(public.code(), ErrorCode::CostLimit);
    assert_ne!(public.code(), ErrorCode::BudgetExceeded);
    assert_ne!(public.code(), ErrorCode::UnsupportedCapability);
    assert!(
        public
            .details()
            .contains_key(&DetailKey::parse("estimated_cost").expect("static key is valid"))
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn exhausted_budget_preserves_its_domain_code_across_the_client_port() {
    let budget = PublicError::builder(
        ErrorCode::BudgetExceeded,
        error_definition(ErrorCode::BudgetExceeded).message,
    )
    .detail(
        DetailKey::parse("budget_limit").expect("static key is valid"),
        PublicValue::Unsigned(100),
    )
    .next_action(NextAction::CorrectField {
        field: DetailKey::parse("budget").expect("static key is valid"),
    })
    .build()
    .expect("budget error fixture is checked");
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Err(ClientPortError::Public(
        Box::new(budget),
    ))));
    let error = execute(
        &harness.executor,
        VerticalTool::QueryAdvanced,
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"}
        }),
    )
    .await
    .expect_err("the daemon budget rejection crosses the MCP adapter");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::BudgetExceeded)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
}

#[test]
fn capability_option_data_scope_and_budget_failures_remain_distinct() {
    for code in [
        ErrorCode::UnsupportedCapability,
        ErrorCode::InvalidArgument,
        ErrorCode::IncompleteCoverage,
        ErrorCode::CostLimit,
        ErrorCode::BudgetExceeded,
    ] {
        let public = PublicError::builder(code, error_definition(code).message)
            .build()
            .expect("registry fixture builds");
        let mapped = map_port_error(ClientPortError::Public(Box::new(public)));
        assert_eq!(mapped.public_error().map(PublicError::code), Some(code));
    }
}

#[tokio::test]
async fn explain_source_reference_without_line_hint_composes_with_source_read() {
    let source = source_reference_without_lines(4, 12);
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(explain_response(
        source.clone(),
    ))));
    let output = Value::Object(
        execute(
            &harness.executor,
            VerticalTool::SymbolExplain,
            json!({
                "repository": {"repository_id": repository()},
                "symbol_ids": [symbol()]
            }),
        )
        .await
        .expect("symbol definition maps"),
    );
    let source_ref = output
        .pointer("/data/symbols/0/definition")
        .expect("symbol explanation returns exact definition evidence")
        .clone();

    assert_source_reference_composes_with_read(source_ref, source).await;
}

#[tokio::test]
async fn maps_expanded_source_range_as_the_returned_verified_reference() {
    let requested = source_reference(5, 10, 2, 2);
    let response = SourceReadPortResponse::new(
        client::SourceRead {
            context: context(1, 15),
            chunks: vec![ClientSourceChunk {
                source: requested.clone(),
                path: "src/lib.rs".to_owned(),
                start_byte: 0,
                end_byte: 15,
                start_line: 1,
                end_line: 3,
                content: "0123456789abcde".to_owned(),
                content_hash: content_hash(),
                language: "rust".to_owned(),
                generated: false,
            }],
            total_source_bytes: 15,
            truncated: false,
        },
        metadata("trace-source-1"),
        Vec::new(),
        Vec::new(),
    );
    let harness = Harness::new(FakeOutcome::SourceRead(Ok(response)));
    let input_ref = wire_source_reference(5, 10, 2, 2);
    let output: SourceReadOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation(),
                "references": [{"source_ref": input_ref}],
                "context_lines_before": 2,
                "context_lines_after": 2,
                "merge_overlaps": false,
                "include_line_numbers": true,
                "encoding": "utf8_lossless_when_valid",
                "response_profile": "compact"
            }),
        )
        .await
        .expect("source read maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected source read success");
    };
    let chunk = &output.data.chunks[0];
    assert_eq!(chunk.source_ref.span().start_byte(), 0);
    assert_eq!(chunk.source_ref.span().end_byte(), 15);
    assert_eq!(
        chunk
            .source_ref
            .line_hint()
            .expect("line hint")
            .start_line(),
        1
    );
    assert_eq!(chunk.start_byte, 0);
    assert_eq!(chunk.end_byte, 15);
    assert_eq!(output.data.total_source_bytes, 15);
    assert_eq!(chunk.trust, TrustClassification::UntrustedRepositoryData);
    let ObservedCall::SourceRead(request) = harness.only_call() else {
        panic!("expected source read request");
    };
    assert_eq!(request.references, [requested]);
}

#[tokio::test]
async fn rejects_every_currently_unsupported_valid_option_before_the_port() {
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Executor)));
    let source = wire_source_reference(5, 10, 2, 2);
    let cases = vec![
        (
            VerticalTool::RepoStatus,
            json!({"repository": {"repository_id": repository()}, "budget": {}}),
        ),
        (
            VerticalTool::RepoStatus,
            json!({"repository": {"repository_id": repository()}, "response_profile": "standard"}),
        ),
        (
            VerticalTool::RepoList,
            json!({"response_profile": "standard"}),
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": {"repository_id": repository()}, "task": "fix a bug", "seeds": {"symbols": [symbol()]}, "token_budget": 1000, "min_confidence": 800}),
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": {"repository_id": repository()}, "task": "fix a bug", "seeds": {"symbols": [symbol()]}, "token_budget": 1000, "source_policy": "signatures"}),
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": {"repository_id": repository()}, "task": "fix a bug", "seeds": {"symbols": [symbol()], "paths": ["src/lib.rs"]}, "token_budget": 1000}),
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": {"repository_id": repository()}, "task": "fix a bug", "seeds": {"symbols": [symbol()]}, "token_budget": 1000, "continuation": "opaque"}),
        ),
        (
            VerticalTool::QueryBatch,
            json!({"repository": {"repository_id": repository()}, "operations": [{"id": "a", "tool": "code.locate", "arguments": {"query": "x"}}], "response_profile": "standard"}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"repository_id": repository()}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "scope": {"paths": ["src"]}}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "mode": "deep"}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "requested_tiers": {"rust": "C"}}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "configuration_patch": {"feature": true}}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "wait_ms": 0}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"alias": "fixture"}, "query": "x"}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "kinds": ["function"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "scope": {"paths": ["src"]}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "languages": ["rust"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "search_modes": ["structural"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "search_modes": ["docs"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "search_modes": ["path"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "search_modes": ["semantic"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "search_modes": ["exact", "lexical"]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "related_to": [symbol()]}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "min_confidence": 700}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"max_tokens": 100}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"max_source_bytes": 1}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"max_traversal_facts": 1}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"max_depth": 1}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"max_paths": 1}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"timeout_ms": 10}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"evidence_level": "compact"}}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "cursor": "opaque"}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": {"repository_id": repository()}, "query": "x", "response_profile": "standard"}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"alias": "fixture"}, "symbol_ids": [symbol()]}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "sections": ["signature"]}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "relation_sample_limit": 0}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "source_preview_lines": 0}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "include_provenance": "full"}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "budget": {}}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "response_profile": "evidence"}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"alias": "fixture"}, "references": [{"source_ref": source.clone()}]}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"symbol_id": symbol()}]}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"file_id": file(), "start_byte": 0, "end_byte": 1}]}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "context_lines_before": 0, "context_lines_after": 0}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "context_lines_before": 2}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "merge_overlaps": true}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "max_source_bytes": 1}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "include_line_numbers": false}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "encoding": "bytes_base64"}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source.clone()}], "budget": {}}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source}], "response_profile": "standard"}),
        ),
    ];

    for (tool, arguments) in cases {
        let error = execute(&harness.executor, tool, arguments)
            .await
            .expect_err("unsupported option is rejected");
        let public = error
            .public_error()
            .expect("unsupported option is a checked public error");
        assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
        assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    }
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn unsupported_fields_are_rejected_with_field_specific_actions() {
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Executor)));
    let cases = [
        (
            VerticalTool::RepoStatus,
            json!({"repository": {"repository_id": repository()}, "budget": {}}),
            "budget",
        ),
        (
            VerticalTool::RepoList,
            json!({"response_profile": "standard"}),
            "response_profile",
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": {"repository_id": repository()}, "task": "fix a bug", "seeds": {"symbols": [symbol()]}, "token_budget": 1000, "min_confidence": 800}),
            "min_confidence",
        ),
    ];
    for (tool, arguments, field) in cases {
        let error = execute(&harness.executor, tool, arguments)
            .await
            .expect_err("unsupported field is rejected");
        let public = error.public_error().expect("checked public error");
        assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
        assert!(
            public.next_actions().iter().any(|action| matches!(
                action,
                NextAction::CorrectField { field: named } if named.as_str() == field
            )),
            "expected a correct-field action naming {field}"
        );
    }
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn context_pack_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::ContextPack,
        json!({"repository": {"repository_id": repository()}, "task": "fix the duplicate payment bug", "seeds": {"symbols": [symbol()]}, "token_budget": 4500, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: ContextPackOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.items.is_empty(),
        "explain performs no retrieval"
    );
    assert!(output.data.pack_id.as_str().starts_with("pack1_"));
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["context_assembly".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no evidence assembly"
    );
}

#[tokio::test]
async fn repo_status_explain_attaches_a_plan_to_the_metadata_read() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::RepoStatus,
        json!({"repository": {"repository_id": repository()}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: RepoStatusOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["status_read".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "repo.status reads only metadata"
    );
}

#[tokio::test]
async fn plan_change_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::PlanChange,
        json!({"repository": {"repository_id": repository()}, "objective": "bug_fix", "objective_text": "fix the defect", "targets": [{"symbol_id": symbol()}], "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: PlanChangeOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert_eq!(
        output.data.plan.len(),
        1,
        "explain emits one marked placeholder step"
    );
    assert!(
        output.data.test_plan.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["change_planning".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no change planning"
    );
}

#[tokio::test]
async fn history_compare_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::HistoryCompare,
        json!({"repository": {"repository_id": repository()}, "base": parent_generation(), "head": generation(), "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: HistoryCompareOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.changes.is_empty(),
        "explain performs no retrieval"
    );
    assert!(
        output.data.lineage.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(
        explanation.operators,
        vec!["revision_comparison".to_owned()]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no revision comparison"
    );
}

#[tokio::test]
async fn code_dead_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::CodeDead,
        json!({"repository": {"repository_id": repository()}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: CodeDeadOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.candidates.is_empty(),
        "explain performs no retrieval"
    );
    assert_eq!(output.data.entry_points.entry_point_count, 0);
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(
        explanation.operators,
        vec!["reachability_analysis".to_owned()]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no reachability analysis"
    );
}

#[tokio::test]
async fn architecture_cycles_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::ArchitectureCycles,
        json!({"repository": {"repository_id": repository()}, "projection": {"relations": ["calls"], "level": "symbol"}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: ArchitectureCyclesOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.components.is_empty(),
        "explain performs no retrieval"
    );
    assert!(
        output.data.cycles.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["cycle_detection".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no cycle detection"
    );
}

#[tokio::test]
async fn architecture_overview_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::ArchitectureOverview,
        json!({"repository": {"repository_id": repository()}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: ArchitectureOverviewOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.components.is_empty(),
        "explain performs no retrieval"
    );
    assert!(
        output.data.connections.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(
        explanation.operators,
        vec!["architecture_mapping".to_owned()]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no architecture mapping"
    );
}

#[tokio::test]
async fn tests_select_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::TestsSelect,
        json!({"repository": {"repository_id": repository()}, "seeds": {"symbols": [symbol()]}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: TestsSelectOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.tests.is_empty(),
        "explain performs no retrieval"
    );
    assert!(output.data.gaps.is_empty(), "explain performs no retrieval");
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["test_selection".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no test selection"
    );
}

#[tokio::test]
async fn change_impact_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::ChangeImpact,
        json!({"repository": {"repository_id": repository()}, "change": {"symbol_ids": [symbol()]}, "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: ChangeImpactOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.impacted.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["change_analysis".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no impact analysis"
    );
}

#[tokio::test]
async fn flow_trace_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::FlowTrace,
        json!({"repository": {"repository_id": repository()}, "from": {"symbol_id": symbol()}, "relations": ["calls"], "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: FlowTraceOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.paths.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["path_traversal".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no traversal"
    );
}

#[tokio::test]
async fn symbol_relationships_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::SymbolRelationships,
        json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "relations": ["calls"], "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: SymbolRelationshipsOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.groups.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(
        explanation.operators,
        vec!["relationship_expansion".to_owned()]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no expansion"
    );
}

#[tokio::test]
async fn source_read_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let source = wire_source_reference(5, 10, 2, 2);
    let output = execute(
        &harness.executor,
        VerticalTool::SourceRead,
        json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source}], "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: SourceReadOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.chunks.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["source_read".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no source read"
    );
}

#[tokio::test]
async fn symbol_explain_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::SymbolExplain,
        json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()], "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: SymbolExplainOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.symbols.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["symbol_lookup".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no symbol retrieval"
    );
}

#[tokio::test]
async fn code_locate_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::CodeLocate,
        json!({"repository": {"repository_id": repository()}, "query": "publish", "explain": true}),
    )
    .await
    .expect("explain executes");
    let output: CodeLocateOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.matches.is_empty(),
        "explain performs no retrieval"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["lexical_scan".to_owned()]);
    assert!(
        explanation.fingerprint.starts_with("plan1_"),
        "explain binds a stable plan fingerprint"
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no locate retrieval"
    );
}

#[tokio::test]
async fn explain_fingerprint_is_stable_for_identical_requests() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let arguments =
        json!({"repository": {"repository_id": repository()}, "query": "publish", "explain": true});
    let first: CodeLocateOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::CodeLocate,
            arguments.clone(),
        )
        .await
        .expect("explain executes"),
    );
    let second: CodeLocateOutput = decode(
        execute(&harness.executor, VerticalTool::CodeLocate, arguments)
            .await
            .expect("explain executes again"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected explain success");
    };
    let ToolResponse::Success(second) = second else {
        panic!("expected explain success");
    };
    let first_fingerprint = first
        .data
        .explanation
        .expect("explain returns a plan")
        .fingerprint;
    let second_fingerprint = second
        .data
        .explanation
        .expect("explain returns a plan")
        .fingerprint;
    assert!(first_fingerprint.starts_with("plan1_"));
    assert_eq!(
        first_fingerprint, second_fingerprint,
        "identical normalized requests on a pinned generation yield one fingerprint"
    );
}

#[tokio::test]
async fn repo_list_explain_returns_a_plan_without_retrieval() {
    let entries = vec![RepositoryListEntry {
        repository_id: repository(),
        active_generation: generation(),
        languages: vec!["rust".to_owned()],
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
    }];
    let snapshot = repo_list_snapshot_id(&entries);
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: entries,
    })));
    let output: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 10, "explain": true}),
        )
        .await
        .expect("explain executes"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(
        output.data.repositories.is_empty(),
        "explain performs no retrieval"
    );
    assert_eq!(output.data.total_count, 1);
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["repository_listing".to_owned()]);
    let full_fingerprint = repo_list_plan_fingerprint(&snapshot);
    let expected = blake3::Hash::from_bytes(full_fingerprint).to_hex();
    assert_eq!(
        explanation.fingerprint,
        format!("plan1_{}", &expected[..32]),
        "the public explanation and cursor context use one physical plan"
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata catalog call runs"
    );
}

#[tokio::test]
async fn query_batch_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        active_generation: generation(),
        parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "generation": "active",
            "operations": [
                {"id": "find_a", "tool": "code.locate", "arguments": {"query": "publish", "max_results": 5}},
                {"id": "find_b", "tool": "code.locate", "arguments": {"query": "stage", "max_results": 5}}
            ],
            "explain": true
        }),
    )
    .await
    .expect("explain executes");
    let output: QueryBatchOutput = decode(output);
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Planned);
    assert_eq!(output.data.operation_results.len(), 2);
    assert!(
        output
            .data
            .operation_results
            .iter()
            .all(|result| result.status == BatchOperationStatus::NotRun),
        "explain runs no batch operation"
    );
    let explanation = output.data.explanation.expect("explain returns a plan");
    assert_eq!(explanation.operators, vec!["batch_dispatch".to_owned()]);
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "only the metadata status call runs, no batch dispatch"
    );
}

#[tokio::test]
async fn explain_plan_is_invariant_across_index_states() {
    // The source-free plan and fingerprint depend only on the normalized
    // request and pinned generation, never on repository index state, so one
    // request yields a single fingerprint across the empty, partial, stale,
    // fresh, small, large, and unsupported capability states.
    let complete = RepositoryCoverageEntry {
        language: "rust".to_owned(),
        tier: "tier_a".to_owned(),
        status: "complete".to_owned(),
        discovered_files: 3,
        indexed_files: 3,
    };
    let large = RepositoryCoverageEntry {
        language: "rust".to_owned(),
        tier: "tier_a".to_owned(),
        status: "complete".to_owned(),
        discovered_files: 5000,
        indexed_files: 5000,
    };
    let states: [(&str, &str, &str, Vec<RepositoryCoverageEntry>); 7] = [
        ("empty", "current", "current", vec![]),
        ("ready", "current", "current", vec![complete.clone()]),
        ("ready", "stale", "stale", vec![complete.clone()]),
        ("ready", "current", "current", vec![complete.clone()]),
        ("ready", "current", "current", vec![complete]),
        ("ready", "current", "current", vec![large]),
        ("degraded", "stale", "current", vec![]),
    ];
    let mut fingerprints = Vec::new();
    for (state, structural, semantic, coverage) in states {
        let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
            repository_id: repository(),
            active_generation: generation(),
            parent_generation: None,
            structural_freshness: structural.to_owned(),
            semantic_freshness: semantic.to_owned(),
            state: state.to_owned(),
            coverage,
        })));
        let output: CodeLocateOutput = decode(
            execute(
                &harness.executor,
                VerticalTool::CodeLocate,
                json!({"repository": {"repository_id": repository()}, "query": "publish", "explain": true}),
            )
            .await
            .expect("explain executes"),
        );
        let ToolResponse::Success(output) = output else {
            panic!("expected explain success");
        };
        fingerprints.push(
            output
                .data
                .explanation
                .expect("explain returns a plan")
                .fingerprint,
        );
    }
    let first = fingerprints[0].clone();
    assert!(first.starts_with("plan1_"));
    for fingerprint in &fingerprints[1..] {
        assert_eq!(
            fingerprint, &first,
            "plan fingerprint is invariant across index states"
        );
    }
}

#[tokio::test]
async fn executor_rejects_semantically_invalid_arguments_before_the_port() {
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Executor)));

    for (tool, arguments) in schema_valid_invalid_inputs() {
        let error = execute(&harness.executor, tool, arguments)
            .await
            .expect_err("semantically invalid arguments are rejected");
        let public = error
            .public_error()
            .expect("caller-controlled invalid input is a checked public error");
        assert_eq!(public.code(), ErrorCode::InvalidArgument);
        assert_eq!(public.message(), INVALID_ARGUMENT_MESSAGE);
    }
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn router_returns_invalid_argument_for_semantically_invalid_inputs() {
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Executor)));
    let call_count = Arc::clone(&harness.call_count);
    let router = ToolRouter::new(
        harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");

    for (tool, arguments) in schema_valid_invalid_inputs() {
        let response = router
            .handle(
                operating_request(json!({
                    "name": tool.name(),
                    "arguments": arguments
                })),
                cancellation(),
            )
            .await;
        let HandlerResponse::Success(result) = response else {
            panic!("invalid arguments are an MCP tool result");
        };
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "INVALID_ARGUMENT"
        );
    }
    assert_eq!(call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn router_keeps_public_failures_typed_and_internal_failures_static() {
    let not_found = PublicError::builder(ErrorCode::NotFound, "requested entity was not found")
        .build()
        .expect("test public error is valid");
    let public_router = ToolRouter::new(
        Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Public(
            Box::new(not_found),
        ))))
        .executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let public_response = public_router
        .handle(
            operating_request(json!({
                "name": "repo.index",
                "arguments": {"root": "C:/fixture"}
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(result) = public_response else {
        panic!("domain failure is an MCP tool result");
    };
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["code"], "NOT_FOUND");

    for (error, expected_message) in [
        (ClientPortError::Transport, "tool transport failed"),
        (
            ClientPortError::InvalidResponse,
            "tool response mapping failed",
        ),
        (ClientPortError::Executor, "tool executor failed"),
    ] {
        let router = ToolRouter::new(
            Harness::new(FakeOutcome::RepositoryIndex(Err(error))).executor,
            rootlight_mcp_contract::ExposureProfile::Developer,
        )
        .expect("router compiles");
        let response = router
            .handle(
                operating_request(json!({
                    "name": "repo.index",
                    "arguments": {"root": "C:/fixture"}
                })),
                cancellation(),
            )
            .await;
        let HandlerResponse::Error { code, message } = response else {
            panic!("internal port failure is a protocol error");
        };
        assert_eq!(code, -32_603);
        assert_eq!(message, expected_message);
    }
}

#[tokio::test]
async fn cancellation_drops_a_pending_client_port_future() {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let router = ToolRouter::new(
        Harness::new(FakeOutcome::PendingRepositoryIndex {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        })
        .executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let (sender, receiver) = watch::channel(false);
    let task = tokio::spawn(async move {
        router
            .handle(
                operating_request(json!({
                    "name": "repo.index",
                    "arguments": {"root": "C:/fixture"}
                })),
                RequestCancellation { receiver },
            )
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("port future starts");
    sender.send(true).expect("request remains in flight");
    let response = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("cancelled request completes")
        .expect("request task does not panic");

    assert!(matches!(response, HandlerResponse::Cancelled));
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn closed_unknown_entity_kind_is_an_internal_mapping_failure() {
    let mut response = locate_response();
    response.result.hits[0].kind = "repository".to_owned();
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
    let error = execute(
        &harness.executor,
        VerticalTool::CodeLocate,
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish"
        }),
    )
    .await
    .expect_err("unsupported daemon entity kind is rejected");
    assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
    assert!(error.public_error().is_none());
}

#[test]
fn unix_millis_mapping_is_stable_at_calendar_boundaries() {
    assert_eq!(
        format_unix_millis(0).expect("epoch maps"),
        "1970-01-01T00:00:00Z"
    );
    assert_eq!(
        format_unix_millis(86_400_000).expect("next day maps"),
        "1970-01-02T00:00:00Z"
    );
    assert_eq!(
        format_unix_millis(1_704_067_199_999).expect("leap boundary maps"),
        "2023-12-31T23:59:59.999Z"
    );
}

fn operating_request(params: Value) -> OperatingRequest {
    OperatingRequest {
        id: RequestId::Number(serde_json::Number::from(1)),
        method: "tools/call".to_owned(),
        params: Some(params),
    }
}
