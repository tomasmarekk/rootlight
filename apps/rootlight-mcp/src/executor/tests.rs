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

use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};
use rootlight_agent::context_continuation::ContextContinuationStateParts;
use rootlight_client::{
    AdvancedColumn as ClientAdvancedColumn, AdvancedPlan as ClientAdvancedPlan,
    AdvancedQuery as ClientAdvancedQuery, AnalysisTier as ClientTier,
    ArchitectureCycles as ClientArchitectureCycles,
    ArchitectureOverview as ClientArchitectureOverview,
    ArchitectureOverviewCommunity as ClientArchitectureCommunity,
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
    FlowTraceProjection as ClientTraceProjection, GenerationSelector as ClientGenerationSelector,
    HistoryArchitectureDelta as ClientHistoryArchitectureDelta,
    HistoryBreakingCandidate as ClientHistoryBreakingCandidate,
    HistoryCompare as ClientHistoryCompare, HistoryLineageMatch as ClientHistoryLineageMatch,
    HistoryMatchedStates as ClientHistoryMatchedStates,
    HistorySemanticChange as ClientHistorySemanticChange, LocateHit, OperationKind, OperationStage,
    OperationState as ClientOperationState, PlanChange as ClientPlanChange,
    PlanChangeContextPack as ClientPlanContextPack, PlanChangeDecision as ClientPlanDecision,
    PlanChangeImpactSummary as ClientPlanImpactSummary, PlanChangeStep as ClientPlanStep,
    QueryContext, QueryUsage, RecoveryClass, RelationshipGroup as ClientRelationshipGroup,
    RelationshipTarget as ClientRelationshipTarget, RepositoryCatalogEntry,
    RepositoryCatalogFreshness, RepositoryCatalogPage, RepositoryCatalogPageRequest,
    RepositoryCatalogSnapshotId, RepositoryCatalogSortKey, RepositoryCatalogState,
    RepositoryCoverageEntry, RepositoryList, RepositoryListEntry, RepositoryStatus,
    RepositoryStatusOperation, ResultCompleteness as ClientResultCompleteness,
    ResultCompletenessState as ClientResultCompletenessState, SourceChunk as ClientSourceChunk,
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
        PlanEvidenceOmissionReason, PlanEvidenceProvider, PlanProviderState, RiskLevel,
        SemanticChangeKind, TestKind, TestsSelectOutput,
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
    CodeLocateSequence(Arc<Mutex<VecDeque<Result<CodeLocatePortResponse, ClientPortError>>>>),
    PendingCodeLocate,
    SymbolExplain(Result<SymbolExplainPortResponse, ClientPortError>),
    SymbolExplainPerRequest(Result<SymbolExplainPortResponse, ClientPortError>),
    SourceRead(Result<SourceReadPortResponse, ClientPortError>),
    RepositoryList(Result<RepositoryList, ClientPortError>),
    RepositoryCatalogPageSequence(
        Arc<Mutex<VecDeque<Result<RepositoryCatalogPage, ClientPortError>>>>,
    ),
    RepositoryStatus(Result<RepositoryStatus, ClientPortError>),
    SymbolRelationships(Result<SymbolRelationshipsPortResponse, ClientPortError>),
    SymbolRelationshipsSequence(
        Arc<Mutex<VecDeque<Result<SymbolRelationshipsPortResponse, ClientPortError>>>>,
    ),
    FlowTrace(Result<FlowTracePortResponse, ClientPortError>),
    ArchitectureCycles(Result<ArchitectureCyclesPortResponse, ClientPortError>),
    CodeDead(Result<CodeDeadPortResponse, ClientPortError>),
    ArchitectureOverview(Result<ArchitectureOverviewPortResponse, ClientPortError>),
    TestsSelect(Result<TestsSelectPortResponse, ClientPortError>),
    ChangeImpact(Result<ChangeImpactPortResponse, ClientPortError>),
    PlanChange(Result<PlanChangePortResponse, ClientPortError>),
    HistoryCompare(Result<HistoryComparePortResponse, ClientPortError>),
    QueryAdvanced(Result<QueryAdvancedPortResponse, ClientPortError>),
    QueryAdvancedSequence(Arc<Mutex<VecDeque<Result<QueryAdvancedPortResponse, ClientPortError>>>>),
    Batch {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        locate: Result<CodeLocatePortResponse, ClientPortError>,
    },
    BatchGenerationRace {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        locate: Result<CodeLocatePortResponse, ClientPortError>,
        active_generation: Arc<Mutex<GenerationId>>,
        locate_calls: Arc<AtomicUsize>,
    },
    BatchLocateSequence {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        locate: Arc<Mutex<VecDeque<Result<CodeLocatePortResponse, ClientPortError>>>>,
    },
    BatchPlanChange {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        locate: Result<CodeLocatePortResponse, ClientPortError>,
        plan_change: Box<Result<PlanChangePortResponse, ClientPortError>>,
    },
    BatchContextPack {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        explain: Result<SymbolExplainPortResponse, ClientPortError>,
    },
    BatchSourceRead {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
        source: Result<SourceReadPortResponse, ClientPortError>,
    },
    BatchPendingLocate {
        status: Box<Result<RepositoryStatus, ClientPortError>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedAnalyticCall<T> {
    request: T,
    options: client::RequestOptions,
}

impl<T> ObservedAnalyticCall<T> {
    fn new(request: T, options: client::RequestOptions) -> Self {
        Self { request, options }
    }
}

impl<T> std::ops::Deref for ObservedAnalyticCall<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.request
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservedCall {
    RepositoryIndex(RepositoryIndexPortRequest),
    OperationStatus(OperationStatusPortRequest),
    CodeLocate(ObservedAnalyticCall<CodeLocatePortRequest>),
    SymbolExplain(ObservedAnalyticCall<SymbolExplainPortRequest>),
    SourceRead(ObservedAnalyticCall<SourceReadPortRequest>),
    RepositoryList(RepositoryCatalogPagePortRequest),
    RepositoryStatus(RepositoryStatusPortRequest),
    SymbolRelationships(ObservedAnalyticCall<SymbolRelationshipsPortRequest>),
    FlowTrace(ObservedAnalyticCall<FlowTracePortRequest>),
    ArchitectureCycles(ObservedAnalyticCall<ArchitectureCyclesPortRequest>),
    CodeDead(ObservedAnalyticCall<CodeDeadPortRequest>),
    ArchitectureOverview(ObservedAnalyticCall<ArchitectureOverviewPortRequest>),
    TestsSelect(ObservedAnalyticCall<TestsSelectPortRequest>),
    ChangeImpact(ObservedAnalyticCall<ChangeImpactPortRequest>),
    PlanChange(ObservedAnalyticCall<PlanChangePortRequest>),
    HistoryCompare(ObservedAnalyticCall<HistoryComparePortRequest>),
    QueryAdvanced(ObservedAnalyticCall<QueryAdvancedPortRequest>),
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

fn catalog_page_for_fixture(
    list: RepositoryList,
    request: &RepositoryCatalogPageRequest,
) -> Result<RepositoryCatalogPage, ClientPortError> {
    let snapshot_id = fixture_catalog_snapshot(&list.repositories);
    if request
        .snapshot_id()
        .is_some_and(|requested| requested != snapshot_id)
    {
        return Err(ClientPortError::Public(Box::new(authoritative_error(
            MappedDomainFailure::invalid_cursor(),
        ))));
    }
    let display_name = request
        .normalized_query()
        .map(str::to_owned)
        .unwrap_or_else(|| "repository".to_owned());
    let mut repositories: Vec<_> = list
        .repositories
        .into_iter()
        .map(|entry| RepositoryCatalogEntry {
            repository_id: entry.repository_id,
            display_name: display_name.clone(),
            alias: None,
            active_generation: Some(entry.active_generation),
            generation_count: 1,
            state: fixture_catalog_state(&entry.state),
            languages: entry.languages,
            structural_freshness: fixture_catalog_freshness(&entry.structural_freshness),
            semantic_freshness: fixture_catalog_freshness(&entry.semantic_freshness),
            coverage: Vec::new(),
        })
        .filter(|entry| {
            request
                .states()
                .is_none_or(|states| states.contains(&entry.state))
        })
        .collect();
    repositories.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.repository_id.cmp(&right.repository_id))
    });
    let total_count =
        u64::try_from(repositories.len()).expect("test repository catalog is bounded");
    let start = if let Some(after) = request.after() {
        repositories
            .iter()
            .position(|entry| {
                fixture_catalog_sort_key(entry).is_ok_and(|key| key.as_bytes() == after.as_bytes())
            })
            .map(|index| index.saturating_add(1))
            .ok_or_else(|| {
                ClientPortError::Public(Box::new(authoritative_error(
                    MappedDomainFailure::invalid_cursor(),
                )))
            })?
    } else {
        0
    };
    let end = start
        .saturating_add(usize::from(request.page_size()))
        .min(repositories.len());
    let page = repositories
        .get(start..end)
        .expect("bounded test page range exists")
        .to_vec();
    let truncated = end < repositories.len();
    let next_after = if truncated {
        page.last().map(fixture_catalog_sort_key).transpose()?
    } else {
        None
    };
    Ok(RepositoryCatalogPage {
        repositories: page,
        snapshot_id,
        next_after,
        total_count: Some(total_count),
        truncated,
        sort_version: 1,
    })
}

fn fixture_catalog_snapshot(entries: &[RepositoryListEntry]) -> RepositoryCatalogSnapshotId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.test.catalog-snapshot.v1");
    hasher.update(
        &u64::try_from(entries.len())
            .expect("test repository catalog is bounded")
            .to_le_bytes(),
    );
    for entry in entries {
        hasher.update(entry.repository_id.as_bytes());
        hasher.update(entry.active_generation.as_bytes());
    }
    RepositoryCatalogSnapshotId::from_bytes(*hasher.finalize().as_bytes())
}

fn fixture_catalog_sort_key(
    entry: &RepositoryCatalogEntry,
) -> Result<RepositoryCatalogSortKey, ClientPortError> {
    let name_length = u16::try_from(entry.display_name.len()).map_err(|_| {
        ClientPortError::Public(Box::new(authoritative_error(
            MappedDomainFailure::invalid_cursor(),
        )))
    })?;
    let mut bytes = Vec::with_capacity(2 + entry.display_name.len() + 16);
    bytes.extend_from_slice(&name_length.to_le_bytes());
    bytes.extend_from_slice(entry.display_name.as_bytes());
    bytes.extend_from_slice(entry.repository_id.as_bytes());
    RepositoryCatalogSortKey::from_bytes(&bytes).map_err(|_| {
        ClientPortError::Public(Box::new(authoritative_error(
            MappedDomainFailure::invalid_cursor(),
        )))
    })
}

fn fixture_catalog_state(state: &str) -> RepositoryCatalogState {
    match state {
        "ready" => RepositoryCatalogState::Ready,
        "indexing" => RepositoryCatalogState::Indexing,
        "corrupt" => RepositoryCatalogState::Corrupt,
        "migration_required" => RepositoryCatalogState::MigrationRequired,
        "rebuild_required" => RepositoryCatalogState::RebuildRequired,
        _ => RepositoryCatalogState::Degraded,
    }
}

fn fixture_catalog_freshness(freshness: &str) -> RepositoryCatalogFreshness {
    match freshness {
        "current" => RepositoryCatalogFreshness::Current,
        "superseded" => RepositoryCatalogFreshness::Superseded,
        _ => RepositoryCatalogFreshness::Stale,
    }
}

fn catalog_entry(marker: u8, display_name: &str) -> RepositoryCatalogEntry {
    RepositoryCatalogEntry {
        repository_id: RepositoryId::from_bytes([marker; 16]),
        display_name: display_name.to_owned(),
        alias: Some(format!("{display_name}-alias")),
        active_generation: Some(GenerationId::from_bytes([marker; 20])),
        generation_count: u64::from(marker),
        state: RepositoryCatalogState::Ready,
        languages: vec!["rust".to_owned()],
        structural_freshness: RepositoryCatalogFreshness::Current,
        semantic_freshness: RepositoryCatalogFreshness::Superseded,
        coverage: vec![RepositoryCoverageEntry {
            language: "rust".to_owned(),
            tier: "tier_c".to_owned(),
            status: "bounded".to_owned(),
            discovered_files: 3,
            indexed_files: 2,
        }],
    }
}

fn catalog_page(
    snapshot: [u8; 32],
    repositories: Vec<RepositoryCatalogEntry>,
    total_count: u64,
    truncated: bool,
) -> RepositoryCatalogPage {
    let next_after = truncated
        .then(|| {
            repositories
                .last()
                .expect("a truncated test page has a last entry")
        })
        .map(fixture_catalog_sort_key)
        .transpose()
        .expect("test catalog sort key is valid");
    RepositoryCatalogPage {
        repositories,
        snapshot_id: RepositoryCatalogSnapshotId::from_bytes(snapshot),
        next_after,
        total_count: Some(total_count),
        truncated,
        sort_version: 1,
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
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        self.record(ObservedCall::CodeLocate(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::CodeLocate(outcome) => outcome.clone(),
            FakeOutcome::CodeLocateSequence(outcomes) => outcomes
                .lock()
                .expect("fake locate sequence is not poisoned")
                .pop_front()
                .expect("fake locate sequence is not exhausted"),
            FakeOutcome::Batch { locate, .. } => locate.clone(),
            FakeOutcome::BatchGenerationRace {
                locate,
                active_generation,
                locate_calls,
                ..
            } => {
                if locate_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    *active_generation
                        .lock()
                        .expect("fake active generation is not poisoned") = alternate_generation();
                }
                locate.clone()
            }
            FakeOutcome::BatchLocateSequence { locate, .. } => locate
                .lock()
                .expect("fake batch locate sequence is not poisoned")
                .pop_front()
                .expect("fake batch locate sequence is not exhausted"),
            FakeOutcome::BatchPlanChange { locate, .. } => locate.clone(),
            FakeOutcome::BatchPendingLocate { .. } => {
                return Box::pin(std::future::pending());
            }
            FakeOutcome::PendingCodeLocate => return Box::pin(std::future::pending()),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn symbol_explain(
        &self,
        request: SymbolExplainPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        let requested_symbols = request.symbols().to_vec();
        self.record(ObservedCall::SymbolExplain(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::SymbolExplain(outcome) => outcome.clone(),
            FakeOutcome::SymbolExplainPerRequest(outcome) => outcome.clone().map(|mut response| {
                response
                    .result
                    .symbols
                    .retain(|explanation| requested_symbols.contains(&explanation.symbol));
                response
                    .result
                    .unresolved_symbols
                    .retain(|symbol| requested_symbols.contains(symbol));
                response
            }),
            FakeOutcome::BatchContextPack { explain, .. } => explain.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn source_read(
        &self,
        request: SourceReadPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        self.record(ObservedCall::SourceRead(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::SourceRead(outcome) => outcome.clone(),
            FakeOutcome::BatchSourceRead { source, .. } => source.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn repository_catalog_page(
        &self,
        request: RepositoryCatalogPagePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryCatalogPage> {
        let catalog_request = request.clone();
        self.record(ObservedCall::RepositoryList(request));
        let outcome = match &self.outcome {
            FakeOutcome::RepositoryList(outcome) => outcome
                .clone()
                .and_then(|list| catalog_page_for_fixture(list, &catalog_request)),
            FakeOutcome::RepositoryCatalogPageSequence(outcomes) => outcomes
                .lock()
                .expect("fake catalog response sequence is not poisoned")
                .pop_front()
                .unwrap_or(Err(ClientPortError::Executor)),
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
            FakeOutcome::Batch { status, .. } => status.as_ref().clone(),
            FakeOutcome::BatchGenerationRace { status, .. }
            | FakeOutcome::BatchLocateSequence { status, .. } => status.as_ref().clone(),
            FakeOutcome::BatchPlanChange { status, .. } => status.as_ref().clone(),
            FakeOutcome::BatchContextPack { status, .. } => status.as_ref().clone(),
            FakeOutcome::BatchSourceRead { status, .. } => status.as_ref().clone(),
            FakeOutcome::BatchPendingLocate { status } => status.as_ref().clone(),
            FakeOutcome::SymbolExplain(Ok(_)) | FakeOutcome::SymbolExplainPerRequest(Ok(_)) => {
                Ok(repository_status_response())
            }
            FakeOutcome::PlanChange(Ok(_)) => Ok(repository_status_response()),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn symbol_relationships(
        &self,
        request: SymbolRelationshipsPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse> {
        self.record(ObservedCall::SymbolRelationships(
            ObservedAnalyticCall::new(request, options),
        ));
        let outcome = match &self.outcome {
            FakeOutcome::SymbolRelationships(outcome) => outcome.clone(),
            FakeOutcome::SymbolRelationshipsSequence(outcomes) => outcomes
                .lock()
                .expect("fake relationships sequence is not poisoned")
                .pop_front()
                .expect("fake relationships sequence is not exhausted"),
            FakeOutcome::PlanChange(Ok(_)) | FakeOutcome::BatchPlanChange { .. } => {
                Ok(plan_relationships_response())
            }
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn flow_trace(
        &self,
        request: FlowTracePortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse> {
        self.record(ObservedCall::FlowTrace(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::FlowTrace(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn architecture_cycles(
        &self,
        request: ArchitectureCyclesPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse> {
        self.record(ObservedCall::ArchitectureCycles(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::ArchitectureCycles(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn code_dead(
        &self,
        request: CodeDeadPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse> {
        self.record(ObservedCall::CodeDead(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::CodeDead(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn architecture_overview(
        &self,
        request: ArchitectureOverviewPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse> {
        self.record(ObservedCall::ArchitectureOverview(
            ObservedAnalyticCall::new(request, options),
        ));
        let outcome = match &self.outcome {
            FakeOutcome::ArchitectureOverview(outcome) => outcome.clone(),
            FakeOutcome::PlanChange(Ok(_)) | FakeOutcome::BatchPlanChange { .. } => {
                Ok(plan_architecture_response())
            }
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn tests_select(
        &self,
        request: TestsSelectPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse> {
        self.record(ObservedCall::TestsSelect(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::TestsSelect(outcome) => outcome.clone(),
            FakeOutcome::PlanChange(Ok(_)) | FakeOutcome::BatchPlanChange { .. } => {
                Ok(plan_tests_response())
            }
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn change_impact(
        &self,
        request: ChangeImpactPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse> {
        self.record(ObservedCall::ChangeImpact(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::ChangeImpact(outcome) => outcome.clone(),
            FakeOutcome::PlanChange(Ok(_)) | FakeOutcome::BatchPlanChange { .. } => {
                Ok(plan_impact_response())
            }
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn plan_change(
        &self,
        request: PlanChangePortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
        self.record(ObservedCall::PlanChange(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::PlanChange(outcome) => outcome.clone(),
            FakeOutcome::BatchPlanChange { plan_change, .. } => plan_change.as_ref().clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn history_compare(
        &self,
        request: HistoryComparePortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<HistoryComparePortResponse> {
        self.record(ObservedCall::HistoryCompare(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::HistoryCompare(outcome) => outcome.clone(),
            _ => Err(ClientPortError::Executor),
        };
        Box::pin(async move { outcome })
    }

    fn query_advanced(
        &self,
        request: QueryAdvancedPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<QueryAdvancedPortResponse> {
        self.record(ObservedCall::QueryAdvanced(ObservedAnalyticCall::new(
            request, options,
        )));
        let outcome = match &self.outcome {
            FakeOutcome::QueryAdvanced(outcome) => outcome.clone(),
            FakeOutcome::QueryAdvancedSequence(outcomes) => outcomes
                .lock()
                .expect("fake advanced sequence is not poisoned")
                .pop_front()
                .expect("fake advanced sequence is not exhausted"),
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

#[tokio::test]
async fn analytic_request_options_reach_the_port_unchanged() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let port = FakePort {
        outcome: FakeOutcome::CodeLocate(Err(ClientPortError::Executor)),
        calls: Arc::clone(&calls),
        call_count: Arc::new(AtomicUsize::new(0)),
    };
    let options = client::RequestOptions::new().with_timeout(
        client::RequestTimeout::new(std::time::Duration::from_secs(7))
            .expect("fixture timeout is strictly positive"),
    );
    let request = CodeLocatePortRequest {
        repository: repository(),
        generation: ClientGenerationSelector::Active,
        query: "transport options".to_owned(),
        mode: LocateMode::Text,
        maximum_results: 3,
        page_offset: 0,
    };

    let _ = port.code_locate(request, options, cancellation()).await;

    let calls = calls.lock().expect("fake call recorder is not poisoned");
    let [ObservedCall::CodeLocate(call)] = calls.as_slice() else {
        panic!("code locate reaches the fake port exactly once");
    };
    assert_eq!(call.options, options);
}

#[test]
fn analytical_budget_lowers_every_public_resource_dimension() {
    let ceiling = BudgetLimits::server_ceiling().maximums();
    let requested = ResponseBudget {
        max_results: Some(3),
        max_tokens: Some(100),
        max_source_bytes: Some(256),
        max_traversal_facts: Some(4),
        max_depth: Some(2),
        max_paths: Some(5),
        timeout_ms: Some(10),
        evidence_level: None,
    };
    let budget = AnalyticalBudget::new(Some(&requested)).expect("public budget is representable");
    let maximums = budget.limits.maximums();

    assert_eq!(maximums.rows, ceiling.rows);
    assert_eq!(maximums.results, 3);
    assert_eq!(maximums.tokens, 100);
    assert_eq!(maximums.actual_tokens, ceiling.actual_tokens);
    assert_eq!(maximums.source_bytes, 256);
    assert_eq!(maximums.traversal_facts, 4);
    assert_eq!(maximums.depth, 2);
    assert_eq!(maximums.paths, 5);
    assert_eq!(maximums.json_bytes, ceiling.json_bytes);
    assert_eq!(maximums.memory_bytes, ceiling.memory_bytes);
    assert_eq!(maximums.time_ms, 10);

    let timeout = budget
        .options
        .timeout()
        .expect("effective budget carries an explicit transport timeout");
    assert_eq!(
        timeout.duration(),
        Duration::from_millis(10) + ANALYTICAL_TRANSPORT_OVERHEAD
    );
    let transported = budget
        .options
        .effective_budget()
        .expect("effective budget reaches the daemon")
        .limits();
    assert_eq!(transported.rows, maximums.rows);
    assert_eq!(transported.edges, maximums.traversal_facts);
    assert_eq!(transported.results, maximums.results);
    assert_eq!(transported.source_bytes, maximums.source_bytes);
    assert_eq!(transported.json_bytes, maximums.json_bytes);
    assert_eq!(transported.estimated_tokens, maximums.tokens);
    assert_eq!(transported.memory_bytes, maximums.memory_bytes);
    assert_eq!(
        transported.duration,
        Duration::from_millis(maximums.time_ms)
    );
    assert_eq!(transported.depth, Some(maximums.depth));
    assert_eq!(transported.paths, Some(maximums.paths));

    let source_limited = AnalyticalBudget::with_source_limit(Some(&requested), Some(128))
        .expect("top-level source limit is representable");
    assert_eq!(source_limited.limits.maximums().source_bytes, 128);
}

#[test]
fn context_evidence_options_use_the_reserved_json_envelope_for_daemon_tokens() {
    let reservation = BudgetCharge {
        rows: 23,
        results: 7,
        tokens: 211,
        actual_tokens: 0,
        source_bytes: 307,
        traversal_facts: 11,
        depth: 3,
        paths: 5,
        json_bytes: 4_096,
        memory_bytes: 8_192,
        time_ms: 17,
    };
    let options = context_evidence::context_evidence_options(reservation)
        .expect("complete parent reservation is valid");
    let transported = options
        .effective_budget()
        .expect("child request carries an explicit budget")
        .limits();

    assert_eq!(transported.rows, reservation.rows);
    assert_eq!(transported.edges, reservation.traversal_facts);
    assert_eq!(transported.results, reservation.results);
    assert_eq!(transported.source_bytes, reservation.source_bytes);
    assert_eq!(transported.json_bytes, reservation.json_bytes);
    assert_eq!(
        transported.estimated_tokens,
        reservation.tokens.max(reservation.json_bytes)
    );
    assert_eq!(transported.memory_bytes, reservation.memory_bytes);
    assert_eq!(
        transported.duration,
        Duration::from_millis(reservation.time_ms)
    );
    assert_eq!(transported.depth, Some(reservation.depth));
    assert_eq!(transported.paths, Some(reservation.paths));
    assert_eq!(
        options
            .timeout()
            .expect("child request carries a bounded transport deadline")
            .duration(),
        Duration::from_millis(reservation.time_ms) + ANALYTICAL_TRANSPORT_OVERHEAD
    );
}

#[test]
fn relationship_evidence_reserves_an_additive_share_for_each_composite_stage() {
    let reservation = BudgetCharge {
        rows: 1_856,
        results: 40,
        tokens: 4_096,
        actual_tokens: 0,
        source_bytes: 4_096,
        traversal_facts: 384,
        depth: 4,
        paths: 16,
        json_bytes: 163_840,
        memory_bytes: 163_840,
        time_ms: 2_000,
    };
    let direct = context_evidence::relationship_evidence_options(reservation, 0)
        .expect("direct symbol evidence reserves discovery and explanation shares");
    let with_three_lookups = context_evidence::relationship_evidence_options(reservation, 3)
        .expect("path evidence also budgets each anchor lookup");
    let direct = direct.effective_budget().expect("direct budget").limits();
    let with_three_lookups = with_three_lookups
        .effective_budget()
        .expect("lookup budget")
        .limits();

    assert_eq!(direct.rows, 928);
    assert_eq!(direct.results, 20);
    assert_eq!(direct.source_bytes, 2_048);
    assert_eq!(direct.edges, 192);
    assert_eq!(direct.paths, Some(8));
    assert_eq!(direct.json_bytes, 81_920);
    assert_eq!(direct.estimated_tokens, 81_920);
    assert_eq!(direct.memory_bytes, 81_920);
    assert_eq!(direct.depth, Some(4));
    assert_eq!(direct.duration, Duration::from_millis(2_000));

    assert_eq!(with_three_lookups.rows, 371);
    assert_eq!(with_three_lookups.results, 8);
    assert_eq!(with_three_lookups.source_bytes, 819);
    assert_eq!(with_three_lookups.edges, 76);
    assert_eq!(with_three_lookups.paths, Some(3));
    assert_eq!(with_three_lookups.json_bytes, 32_768);
    assert_eq!(with_three_lookups.estimated_tokens, 32_768);
    assert_eq!(with_three_lookups.memory_bytes, 32_768);
    assert_eq!(with_three_lookups.depth, Some(4));
    assert_eq!(with_three_lookups.duration, Duration::from_millis(2_000));
}

#[test]
fn context_evidence_budget_share_reclaims_unused_additive_capacity() {
    let reservation = BudgetCharge {
        rows: 1_856,
        results: 40,
        tokens: 4_096,
        actual_tokens: 0,
        source_bytes: 4_096,
        traversal_facts: 384,
        depth: 4,
        paths: 16,
        json_bytes: 163_840,
        memory_bytes: 163_840,
        time_ms: 2_000,
    };
    let discovery_usage = BudgetCharge {
        rows: 131,
        results: 3,
        tokens: 512,
        actual_tokens: 0,
        source_bytes: 0,
        traversal_facts: 131,
        depth: 2,
        paths: 0,
        json_bytes: 16_384,
        memory_bytes: 8_192,
        time_ms: 25,
    };
    let first = context_evidence::context_evidence_budget_share(reservation, discovery_usage, 3)
        .expect("three explanations receive positive shares");
    assert_eq!(first.traversal_facts, 84);
    assert_eq!(first.depth, reservation.depth);
    assert_eq!(first.time_ms, reservation.time_ms);

    let after_underuse = BudgetCharge {
        rows: discovery_usage.rows + 100,
        results: discovery_usage.results + 1,
        tokens: discovery_usage.tokens + 128,
        actual_tokens: 0,
        source_bytes: discovery_usage.source_bytes,
        traversal_facts: discovery_usage.traversal_facts + 40,
        depth: reservation.depth,
        paths: discovery_usage.paths,
        json_bytes: discovery_usage.json_bytes + 4_096,
        memory_bytes: discovery_usage.memory_bytes + 2_048,
        time_ms: 40,
    };
    let second = context_evidence::context_evidence_budget_share(reservation, after_underuse, 2)
        .expect("later explanations reclaim unused capacity");
    assert_eq!(second.traversal_facts, 106);
    assert!(second.traversal_facts > first.traversal_facts);
    assert!(after_underuse.rows + second.rows * 2 <= reservation.rows);
    assert!(after_underuse.results + second.results * 2 <= reservation.results);
    assert!(after_underuse.source_bytes + second.source_bytes * 2 <= reservation.source_bytes);
    assert!(
        after_underuse.traversal_facts + second.traversal_facts * 2 <= reservation.traversal_facts
    );
    assert!(after_underuse.paths + second.paths * 2 <= reservation.paths);
    assert!(after_underuse.json_bytes + second.json_bytes * 2 <= reservation.json_bytes);
    assert!(after_underuse.memory_bytes + second.memory_bytes * 2 <= reservation.memory_bytes);
}

#[test]
fn source_evidence_divides_scanned_rows_across_composite_calls() {
    let reservation = BudgetCharge {
        rows: 225,
        results: 4,
        tokens: 512,
        actual_tokens: 0,
        source_bytes: 2_048,
        traversal_facts: 16,
        depth: 4,
        paths: 1,
        json_bytes: 16_384,
        memory_bytes: 16_384,
        time_ms: 2_000,
    };
    let direct = context_evidence::source_evidence_options(reservation, 0)
        .expect("direct symbol evidence has two transport calls");
    let with_three_lookups = context_evidence::source_evidence_options(reservation, 3)
        .expect("path evidence also budgets each anchor lookup");

    assert_eq!(
        direct
            .effective_budget()
            .expect("direct budget")
            .limits()
            .rows,
        112
    );
    assert_eq!(
        with_three_lookups
            .effective_budget()
            .expect("lookup budget")
            .limits()
            .rows,
        45
    );
}

#[test]
fn omitted_analytical_budget_transports_the_complete_server_ceiling() {
    let budget = AnalyticalBudget::new(None).expect("server ceiling is representable");
    assert_eq!(budget.limits, BudgetLimits::server_ceiling());
    assert_eq!(
        budget
            .options
            .effective_budget()
            .expect("omitted public budget still has a server-owned ceiling")
            .limits()
            .results,
        BudgetLimits::server_ceiling().maximums().results
    );
    assert_eq!(
        budget
            .options
            .timeout()
            .expect("server ceiling supplies the transport timeout")
            .duration(),
        Duration::from_millis(BudgetLimits::server_ceiling().maximums().time_ms)
            + ANALYTICAL_TRANSPORT_OVERHEAD
    );
}

#[test]
fn batch_retrieval_budget_preserves_results_without_transporting_tokens() {
    let budget = ResponseBudget {
        max_results: Some(1_000),
        max_tokens: Some(3_000),
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: Some(1_000),
        evidence_level: None,
    };
    let mut default_arguments = Map::new();
    apply_child_budget(BatchTool::CodeLocate, &budget, &mut default_arguments)
        .expect("default locate budget is representable");
    assert_eq!(
        default_arguments["budget"]["max_results"],
        json!(20),
        "the standalone default is narrower than the shared batch ceiling"
    );
    assert_eq!(
        default_arguments["budget"].get("max_tokens"),
        None,
        "the aggregate token limit is enforced against the mapped child envelope"
    );

    let mut explicit_arguments = Map::from_iter([("max_results".to_owned(), json!(7))]);
    apply_child_budget(BatchTool::CodeLocate, &budget, &mut explicit_arguments)
        .expect("explicit locate limit is representable");
    assert_eq!(explicit_arguments["budget"]["max_results"], json!(7));

    let mut explain_arguments = Map::new();
    apply_child_budget(BatchTool::SymbolExplain, &budget, &mut explain_arguments)
        .expect("symbol explain budget is representable");
    assert_eq!(explain_arguments["budget"].get("max_tokens"), None);
}

#[test]
fn batch_profile_injection_uses_the_registry_wire_field() {
    let mut locate = Map::new();
    apply_child_profile(
        BatchTool::CodeLocate,
        ResponseProfile::Standard,
        &mut locate,
    )
    .expect("code.locate supports the standard representation");
    assert_eq!(locate["response_profile"], json!("standard"));

    let mut impact = Map::new();
    apply_child_profile(
        BatchTool::ChangeImpact,
        ResponseProfile::Evidence,
        &mut impact,
    )
    .expect("change.impact supports the evidence representation");
    assert_eq!(impact["profile"], json!("evidence"));

    assert!(
        apply_child_profile(
            BatchTool::SourceRead,
            ResponseProfile::Standard,
            &mut Map::new()
        )
        .is_err(),
        "a fixed compact child cannot silently widen its representation"
    );
}

#[test]
fn final_serialization_enforces_exact_byte_and_conservative_token_boundaries() {
    let unsupported = PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
        .build()
        .expect("static unsupported error is valid");
    let input: CodeLocateInput = decode_input(Map::from_iter([
        (
            "repository".to_owned(),
            json!({"repository_id": repository()}),
        ),
        ("query".to_owned(), json!("publish")),
    ]))
    .expect("fixture input decodes");
    let request = normalize_code_locate(input, &unsupported).expect("fixture input normalizes");
    let output = map_code_locate(locate_response(), &request, None).expect("fixture output maps");
    let measured = serialize_measured_read_success(
        output.clone(),
        Instant::now(),
        BudgetLimits::server_ceiling(),
    )
    .expect("server ceiling admits the fixture");
    let exact_bytes = measured["usage"]["json_bytes"]
        .as_u64()
        .expect("measured usage contains exact JSON bytes");
    assert!(exact_bytes > 1);

    let mut exact = BudgetLimits::server_ceiling().maximums();
    exact.json_bytes = exact_bytes;
    exact.tokens = exact_bytes;
    serialize_measured_read_success(
        output.clone(),
        Instant::now(),
        BudgetLimits::from_maximums(exact),
    )
    .expect("the exact byte and conservative token boundaries are inclusive");

    let mut below_bytes = exact;
    below_bytes.json_bytes = exact_bytes - 1;
    let error = serialize_measured_read_success(
        output.clone(),
        Instant::now(),
        BudgetLimits::from_maximums(below_bytes),
    )
    .expect_err("one byte above the response ceiling is rejected");
    assert_canonical_budget_error(
        error
            .public_error()
            .expect("byte exhaustion is a checked budget error"),
    );

    let mut below_tokens = exact;
    below_tokens.tokens = exact_bytes - 1;
    let error = serialize_measured_read_success(
        output,
        Instant::now(),
        BudgetLimits::from_maximums(below_tokens),
    )
    .expect_err("one byte above the conservative token ceiling is rejected");
    assert_canonical_budget_error(
        error
            .public_error()
            .expect("token exhaustion is a checked budget error"),
    );
}

#[test]
fn context_pack_budget_exhaustion_is_a_checked_public_error() {
    let unsupported = PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
        .build()
        .expect("static unsupported error is valid");
    let error = map_context_pack_service_error(
        ContextPackServiceError::BudgetExceeded,
        &unsupported,
        &unsupported,
    );

    assert_canonical_budget_error(
        error
            .public_error()
            .expect("budget exhaustion remains a checked public error"),
    );
}

async fn execute(
    executor: &impl ToolExecutor,
    tool: VerticalTool,
    arguments: Value,
) -> Result<Map<String, Value>, ToolExecutionError> {
    execute_as(executor, tool, arguments, ExposureProfile::Developer).await
}

async fn execute_as(
    executor: &impl ToolExecutor,
    tool: VerticalTool,
    arguments: Value,
    exposure_profile: ExposureProfile,
) -> Result<Map<String, Value>, ToolExecutionError> {
    let Value::Object(arguments) = arguments else {
        panic!("test arguments are objects");
    };
    executor
        .execute(tool, arguments, exposure_profile, cancellation())
        .await
}

fn decode<T: DeserializeOwned>(output: Map<String, Value>) -> T {
    serde_json::from_value(Value::Object(output)).expect("mapped output satisfies its wire type")
}

fn normalize_measured_usage(output: &mut Map<String, Value>) {
    let Some(Value::Object(usage)) = output.get_mut("usage") else {
        return;
    };
    for field in ["wall_time_ms", "json_bytes", "estimated_tokens"] {
        usage.insert(field.to_owned(), Value::from(0));
    }
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
        token_accounting: None,
        memory_bytes: None,
        elapsed_micros: 1_001,
    }
}

fn context(results: u64, source_bytes: u64) -> QueryContext {
    QueryContext {
        repository: repository(),
        generation: generation(),
        parent_generation: Some(parent_generation()),
        active_generation: true,
        structural_freshness: client::QueryFreshness::Current,
        semantic_freshness: client::QueryFreshness::Current,
        tier: ClientTier::TierC,
        coverage_status: ClientCoverage::Complete,
        skipped_inputs: 0,
        usage: usage(results, source_bytes),
    }
}

fn complete_execution() -> ClientResultCompleteness {
    ClientResultCompleteness {
        state: ClientResultCompletenessState::Complete,
        limiting_resources: Vec::new(),
        continuation: client::ContinuationAvailability::NotApplicable,
        guidance: Vec::new(),
    }
}

fn pageable_execution(resource: client::LimitingResourceKind) -> ClientResultCompleteness {
    ClientResultCompleteness {
        state: ClientResultCompletenessState::Truncated,
        limiting_resources: vec![client::LimitingResource {
            kind: resource,
            limit: None,
            observed: None,
        }],
        continuation: client::ContinuationAvailability::Available,
        guidance: vec![client::ContinuationGuidance::UseCursor],
    }
}

fn truncated_execution(
    resource: client::LimitingResourceKind,
    guidance: client::ContinuationGuidance,
) -> ClientResultCompleteness {
    ClientResultCompleteness {
        state: ClientResultCompletenessState::Truncated,
        limiting_resources: vec![client::LimitingResource {
            kind: resource,
            limit: None,
            observed: None,
        }],
        continuation: client::ContinuationAvailability::Unavailable,
        guidance: vec![guidance],
    }
}

fn unsupported_execution(resource: client::LimitingResourceKind) -> ClientResultCompleteness {
    ClientResultCompleteness {
        state: ClientResultCompletenessState::UnsupportedPartial,
        limiting_resources: vec![client::LimitingResource {
            kind: resource,
            limit: None,
            observed: None,
        }],
        continuation: client::ContinuationAvailability::Unavailable,
        guidance: vec![client::ContinuationGuidance::UnsupportedNoContinuation],
    }
}

fn assert_public_truncation<T>(output: &ReadEnvelope<T>, resource: ContractLimitingResourceKind) {
    assert!(output.truncated);
    assert_eq!(output.completeness.state, CompletenessState::Truncated);
    assert!(
        output
            .completeness
            .limiting_resources
            .iter()
            .any(|observed| observed.kind == resource)
    );
    assert_eq!(
        output.completeness.continuation,
        ContinuationAvailability::Unavailable
    );
    assert!(output.next_cursor.0.is_none());
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
            next_page_offset: None,
            execution_completeness: complete_execution(),
        },
        metadata("trace-locate-1"),
        vec!["publish".to_owned()],
    )
}

fn locate_page(
    symbol_id: SymbolId,
    label: &str,
    next_page_offset: Option<u64>,
) -> CodeLocatePortResponse {
    let mut response = locate_response();
    response.result.hits[0].symbol = symbol_id;
    response.result.hits[0].identifier = label.to_owned();
    response.result.hits[0].qualified_name = format!("crate::{label}");
    response.result.matched_candidates = 3;
    response.result.truncated = next_page_offset.is_some();
    response.result.next_page_offset = next_page_offset;
    response.result.execution_completeness = if next_page_offset.is_some() {
        pageable_execution(client::LimitingResourceKind::Results)
    } else {
        complete_execution()
    };
    response
}

fn relationships_page(
    target: SymbolId,
    next_page_offset: Option<u64>,
) -> SymbolRelationshipsPortResponse {
    SymbolRelationshipsPortResponse::new(
        ClientRelationships {
            context: context(1, 0),
            groups: vec![ClientRelationshipGroup {
                seed: symbol(),
                relation: "calls".to_owned(),
                direction: "outbound".to_owned(),
                items: vec![ClientRelationshipTarget {
                    symbol: target,
                    confidence: 900,
                    source_refs: vec![source_reference(0, 10, 1, 1)],
                }],
                total_count: 3,
            }],
            returned_edges: 1,
            total_edges: 3,
            exact: true,
            truncated: next_page_offset.is_some(),
            next_page_offset,
            execution_completeness: if next_page_offset.is_some() {
                pageable_execution(client::LimitingResourceKind::Results)
            } else {
                complete_execution()
            },
        },
        metadata("trace-rel-page"),
    )
}

fn advanced_page(row_id: &str, next_page_offset: Option<u64>) -> QueryAdvancedPortResponse {
    QueryAdvancedPortResponse::new(
        ClientAdvancedQuery {
            context: context(1, 0),
            columns: vec![ClientAdvancedColumn {
                name: "id".to_owned(),
                column_type: "symbol_id".to_owned(),
            }],
            rows: vec![json!({"id": row_id})],
            plan: None,
            completeness: if next_page_offset.is_some() {
                "paged".to_owned()
            } else {
                "complete".to_owned()
            },
            next_page_offset,
            execution_completeness: if next_page_offset.is_some() {
                pageable_execution(client::LimitingResourceKind::Results)
            } else {
                complete_execution()
            },
        },
        metadata("trace-advanced-page"),
    )
}

fn repository_status_response() -> RepositoryStatus {
    RepositoryStatus {
        repository_id: repository(),
        display_name: "fixture".to_owned(),
        alias: None,
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: Some(parent_generation()),
        active_parent_generation: Some(parent_generation()),
        active_structural_freshness: "current".to_owned(),
        active_semantic_freshness: "current".to_owned(),
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        publication_state: "published".to_owned(),
        coverage: vec![RepositoryCoverageEntry {
            language: "rust".to_owned(),
            tier: "tier_c".to_owned(),
            status: "complete".to_owned(),
            discovered_files: 1,
            indexed_files: 1,
        }],
        operations: Vec::new(),
    }
}

fn batch_harness() -> Harness {
    Harness::new(FakeOutcome::Batch {
        status: Box::new(Ok(repository_status_response())),
        locate: Ok(locate_response()),
    })
}

fn batch_plan_change_response() -> PlanChangePortResponse {
    PlanChangePortResponse::new(
        ClientPlanChange {
            context: context(1, 0),
            plan: vec![ClientPlanStep {
                step: 1,
                action: "Apply the bounded change.".to_owned(),
                targets: vec![symbol()],
                depends_on: Vec::new(),
                risks: Vec::new(),
                verification: Some("run the focused regression test".to_owned()),
            }],
            affected_scope: ClientPlanImpactSummary {
                affected_symbols: 1,
                affected_files: 1,
                risk_level: "low".to_owned(),
                touches_public_surface: false,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ClientPlanContextPack {
                symbols: vec![symbol()],
                files: vec![file()],
            },
            execution_completeness: complete_execution(),
        },
        metadata("batch-plan-change"),
    )
}

fn plan_relationships_response() -> SymbolRelationshipsPortResponse {
    SymbolRelationshipsPortResponse::new(
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
            next_page_offset: None,
            execution_completeness: complete_execution(),
        },
        metadata("plan-relationships"),
    )
}

fn plan_architecture_response() -> ArchitectureOverviewPortResponse {
    ArchitectureOverviewPortResponse::new(
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
            connections: Vec::new(),
            hotspots: Vec::new(),
            communities: Vec::new(),
            views: Vec::new(),
            execution_completeness: complete_execution(),
        },
        metadata("plan-architecture"),
    )
}

fn plan_tests_response() -> TestsSelectPortResponse {
    TestsSelectPortResponse::new(
        ClientTestsSelect {
            context: context(1, 0),
            tests: vec![ClientRankedTest {
                test_id: "test-1".to_owned(),
                kind: "unit".to_owned(),
                path: Some("src/a.rs".to_owned()),
                score: 970,
                why: vec!["direct_test_edge".to_owned()],
                estimated_cost_ms: None,
                command_hint: None,
            }],
            coverage_strategy: ClientCoverageStrategy {
                direct_edges: true,
                transitive_signals: false,
                history_signals: false,
                file_colocation_signals: true,
            },
            gaps: Vec::new(),
            execution_completeness: complete_execution(),
        },
        metadata("plan-tests"),
    )
}

fn plan_impact_response() -> ChangeImpactPortResponse {
    ChangeImpactPortResponse::new(
        ClientChangeImpact {
            context: context(1, 0),
            resolved_changes: vec![ClientResolvedChange {
                symbol_id: Some(symbol()),
                file_id: Some(file()),
                classification: "body".to_owned(),
                kind: Some("function".to_owned()),
            }],
            impacted: Vec::new(),
            tests: Vec::new(),
            risk_summary: ClientRiskSummary {
                level: "low".to_owned(),
                reasons: Vec::new(),
                coverage: "complete".to_owned(),
                breaking_surface: false,
                fanout: 0,
                dynamic_blind_spots: false,
            },
            execution_completeness: complete_execution(),
        },
        metadata("plan-impact"),
    )
}

fn assert_canonical_budget_error(error: &PublicError) {
    let definition = error_definition(ErrorCode::BudgetExceeded);
    assert_eq!(error.code(), ErrorCode::BudgetExceeded);
    assert_eq!(error.message(), definition.message);
    assert!(!error.retryable());
    assert!(error.retry_after_ms().is_none());
    assert!(error.details().is_empty());
    assert_eq!(
        error.next_actions(),
        &[NextAction::CorrectField {
            field: DetailKey::parse("budget").expect("static detail key is valid"),
        }]
    );
}

fn pagination_arguments(tool: VerticalTool) -> Value {
    match tool {
        VerticalTool::CodeLocate => json!({
            "repository": {"repository_id": repository()},
            "query": "publish",
            "search_modes": ["exact"],
            "max_results": 2
        }),
        VerticalTool::SymbolRelationships => json!({
            "repository": {"repository_id": repository()},
            "symbol_ids": [symbol()],
            "relations": ["calls"],
            "max_results": 2
        }),
        VerticalTool::QueryAdvanced => json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "max_results": 2
        }),
        _ => panic!("fixture supports only repository read pagination"),
    }
}

fn pagination_cursor_context(
    tool: VerticalTool,
    arguments: Value,
    exposure_profile: ExposureProfile,
    key_id: u64,
) -> CursorContext {
    let Value::Object(arguments) = arguments else {
        panic!("pagination arguments are objects");
    };
    let unsupported = PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
        .build()
        .expect("static unsupported error is valid");
    match tool {
        VerticalTool::CodeLocate => {
            let input: CodeLocateInput =
                decode_input(arguments).expect("code locate fixture decodes");
            let budget =
                AnalyticalBudget::new(input.budget.as_ref()).expect("fixture budget is valid");
            let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
            let request =
                normalize_code_locate(input, &unsupported).expect("code locate fixture normalizes");
            code_locate_cursor_context(
                &request,
                generation(),
                exposure_profile,
                response_profile,
                budget.limits,
                key_id,
            )
        }
        VerticalTool::SymbolRelationships => {
            let input: SymbolRelationshipsInput =
                decode_input(arguments).expect("relationships fixture decodes");
            let budget =
                AnalyticalBudget::new(input.budget.as_ref()).expect("fixture budget is valid");
            let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
            let request = normalize_symbol_relationships(input, &unsupported)
                .expect("relationships fixture normalizes");
            symbol_relationships_cursor_context(
                &request,
                generation(),
                exposure_profile,
                response_profile,
                budget.limits,
                key_id,
            )
        }
        VerticalTool::QueryAdvanced => {
            let input: QueryAdvancedInput =
                decode_input(arguments).expect("advanced fixture decodes");
            let request =
                normalize_query_advanced(input, &unsupported).expect("advanced fixture normalizes");
            query_advanced_cursor_context(&request, generation(), exposure_profile, key_id)
        }
        _ => panic!("fixture supports only repository read pagination"),
    }
}

fn issue_pagination_cursor(
    tool: VerticalTool,
    arguments: Value,
    exposure_profile: ExposureProfile,
    signing_key: CursorSigningKey,
    issued_at_ms: u64,
    stale_plan: bool,
) -> String {
    let mut context =
        pagination_cursor_context(tool, arguments, exposure_profile, signing_key.key_id);
    if stale_plan {
        context.plan_fingerprint = [0xA5; 32];
    }
    AuthenticatedCursor::create(
        context,
        1_u64.to_be_bytes().to_vec(),
        issued_at_ms,
        &signing_key.secret,
    )
    .expect("pagination cursor fixture is valid")
    .to_wire()
}

fn context_pack_arguments() -> Value {
    json!({
        "repository": {"repository_id": repository()},
        "task": "explain the parser",
        "seeds": {"symbols": [symbol()]},
        "token_budget": 1000
    })
}

fn issue_context_pack_cursor(
    arguments: Value,
    exposure_profile: ExposureProfile,
    signing_key: CursorSigningKey,
    issued_at_ms: u64,
    planner_version: u32,
) -> String {
    let Value::Object(arguments) = arguments else {
        panic!("context-pack arguments are objects");
    };
    let input: ContextPackInput = decode_input(arguments).expect("context-pack fixture decodes");
    let canonical = CanonicalContextPackRequest::new(&input, repository(), generation())
        .expect("context-pack fixture canonicalizes");
    let binding = ContextContinuationBinding {
        repository: canonical.repository(),
        generation: canonical.generation(),
        request_digest: canonical.digest_bytes(),
        response_profile: canonical.response_profile(),
        token_budget: canonical.token_budget(),
        planner_version,
        role_policy_version: rootlight_mcp_contract::context::OBJECTIVE_ROLE_POLICY_VERSION,
    };
    let state = ContextContinuationState::new(ContextContinuationStateParts {
        next_page: 1,
        output_budget: 1_000,
        corpus_digest: [3; 32],
        page_start_digest: [0; 32],
        page_start_count: 0,
        emitted_digest: [4; 32],
        emitted_count: 1,
        remaining_candidates: 1,
        page_item_counts: vec![1],
    })
    .expect("context frontier fixture is valid");
    AuthenticatedCursor::create(
        context_pack_cursor_context(binding, exposure_profile, signing_key.key_id),
        state.encode(),
        issued_at_ms,
        &signing_key.secret,
    )
    .expect("context cursor fixture is valid")
    .to_wire()
}

fn with_argument(mut arguments: Value, field: &str, value: Value) -> Value {
    arguments
        .as_object_mut()
        .expect("pagination arguments are objects")
        .insert(field.to_owned(), value);
    arguments
}

fn pagination_failure(tool: VerticalTool) -> FakeOutcome {
    match tool {
        VerticalTool::CodeLocate => FakeOutcome::CodeLocate(Err(ClientPortError::Executor)),
        VerticalTool::SymbolRelationships => {
            FakeOutcome::SymbolRelationships(Err(ClientPortError::Executor))
        }
        VerticalTool::QueryAdvanced => FakeOutcome::QueryAdvanced(Err(ClientPortError::Executor)),
        _ => panic!("fixture supports only repository read pagination"),
    }
}

#[tokio::test]
async fn repository_read_cursors_bind_every_cross_tool_execution_dimension() {
    const KEY_MATERIAL: [u8; 32] = [0x5C; 32];
    let signing_key =
        CursorSigningKey::deterministic(KEY_MATERIAL).expect("test signing key is valid");
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::CursorContinuation)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            ("code.locate", "cursor"),
            ("context.pack", "continuation"),
            ("query.advanced", "cursor"),
            ("symbol.relationships", "cursor"),
        ])
    );

    for tool in [
        VerticalTool::CodeLocate,
        VerticalTool::SymbolRelationships,
        VerticalTool::QueryAdvanced,
    ] {
        let base = pagination_arguments(tool);
        let valid_cursor = issue_pagination_cursor(
            tool,
            base.clone(),
            ExposureProfile::Developer,
            signing_key,
            now_unix_ms(),
            false,
        );
        let expired_cursor = issue_pagination_cursor(
            tool,
            base.clone(),
            ExposureProfile::Developer,
            signing_key,
            now_unix_ms().saturating_sub(400_000),
            false,
        );
        let stale_plan_cursor = issue_pagination_cursor(
            tool,
            base.clone(),
            ExposureProfile::Developer,
            signing_key,
            now_unix_ms(),
            true,
        );
        let mut tampered_cursor = valid_cursor.clone().into_bytes();
        let final_byte = tampered_cursor
            .last_mut()
            .expect("cursor has an encoded payload");
        *final_byte = if *final_byte == b'A' { b'B' } else { b'A' };
        let tampered_cursor = String::from_utf8(tampered_cursor).expect("base64url remains UTF-8");

        let query_mutation = match tool {
            VerticalTool::CodeLocate => {
                with_argument(base.clone(), "query", json!("different_query"))
            }
            VerticalTool::SymbolRelationships => {
                with_argument(base.clone(), "relations", json!(["references"]))
            }
            VerticalTool::QueryAdvanced => with_argument(
                base.clone(),
                "query",
                json!({"op": "scan", "entity": "file"}),
            ),
            _ => unreachable!("tool set is closed above"),
        };
        let mut cases = vec![
            (
                "exposure profile",
                with_argument(base.clone(), "cursor", json!(valid_cursor.clone())),
                ExposureProfile::Analysis,
            ),
            (
                "effective page size",
                with_argument(
                    with_argument(base.clone(), "max_results", json!(3)),
                    "cursor",
                    json!(valid_cursor.clone()),
                ),
                ExposureProfile::Developer,
            ),
            (
                "query and physical plan",
                with_argument(query_mutation, "cursor", json!(valid_cursor.clone())),
                ExposureProfile::Developer,
            ),
            (
                "generation",
                with_argument(
                    with_argument(base.clone(), "generation", json!(alternate_generation())),
                    "cursor",
                    json!(valid_cursor.clone()),
                ),
                ExposureProfile::Developer,
            ),
            (
                "expired token",
                with_argument(base.clone(), "cursor", json!(expired_cursor)),
                ExposureProfile::Developer,
            ),
            (
                "tampered token",
                with_argument(base.clone(), "cursor", json!(tampered_cursor)),
                ExposureProfile::Developer,
            ),
            (
                "stale physical plan",
                with_argument(base.clone(), "cursor", json!(stale_plan_cursor)),
                ExposureProfile::Developer,
            ),
        ];
        if matches!(
            tool,
            VerticalTool::CodeLocate | VerticalTool::SymbolRelationships
        ) {
            cases.push((
                "response profile",
                with_argument(
                    with_argument(base, "response_profile", json!("standard")),
                    "cursor",
                    json!(valid_cursor),
                ),
                ExposureProfile::Developer,
            ));
        }

        for (dimension, arguments, exposure_profile) in cases {
            let harness = Harness::with_cursor_key(pagination_failure(tool), KEY_MATERIAL);
            let error = execute_as(&harness.executor, tool, arguments, exposure_profile)
                .await
                .expect_err("mutated cursor is rejected");
            assert_eq!(
                error.public_error().map(PublicError::code),
                Some(ErrorCode::InvalidCursor),
                "{tool:?} failed to bind {dimension}"
            );
            assert_eq!(
                harness.call_count.load(Ordering::Relaxed),
                0,
                "{tool:?} performed daemon work for {dimension}"
            );
        }
    }
}

#[tokio::test]
async fn context_pack_cursor_failures_are_rejected_before_daemon_work() {
    const KEY_MATERIAL: [u8; 32] = [0x6D; 32];
    let signing_key =
        CursorSigningKey::deterministic(KEY_MATERIAL).expect("test signing key is valid");
    let base = context_pack_arguments();
    let valid = issue_context_pack_cursor(
        base.clone(),
        ExposureProfile::Developer,
        signing_key,
        now_unix_ms(),
        rootlight_mcp_contract::context::PLANNER_VERSION,
    );
    let expired = issue_context_pack_cursor(
        base.clone(),
        ExposureProfile::Developer,
        signing_key,
        now_unix_ms().saturating_sub(400_000),
        rootlight_mcp_contract::context::PLANNER_VERSION,
    );
    let retired_planner = issue_context_pack_cursor(
        base.clone(),
        ExposureProfile::Developer,
        signing_key,
        now_unix_ms(),
        rootlight_mcp_contract::context::PLANNER_VERSION.saturating_add(1),
    );
    let mut tampered = valid.clone().into_bytes();
    let last = tampered.last_mut().expect("cursor has an encoded payload");
    *last = if *last == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).expect("base64url remains UTF-8");

    let cases = [
        (
            "request",
            with_argument(
                with_argument(base.clone(), "task", json!("different task")),
                "continuation",
                json!(valid.clone()),
            ),
            ExposureProfile::Developer,
        ),
        (
            "generation",
            with_argument(
                with_argument(base.clone(), "generation", json!(alternate_generation())),
                "continuation",
                json!(valid.clone()),
            ),
            ExposureProfile::Developer,
        ),
        (
            "response profile",
            with_argument(
                with_argument(base.clone(), "response_profile", json!("standard")),
                "continuation",
                json!(valid.clone()),
            ),
            ExposureProfile::Developer,
        ),
        (
            "budget increase",
            with_argument(
                with_argument(base.clone(), "token_budget", json!(1001)),
                "continuation",
                json!(valid.clone()),
            ),
            ExposureProfile::Developer,
        ),
        (
            "exposure profile",
            with_argument(base.clone(), "continuation", json!(valid.clone())),
            ExposureProfile::Analysis,
        ),
        (
            "expiry",
            with_argument(base.clone(), "continuation", json!(expired)),
            ExposureProfile::Developer,
        ),
        (
            "tamper",
            with_argument(base.clone(), "continuation", json!(tampered)),
            ExposureProfile::Developer,
        ),
        (
            "planner retirement",
            with_argument(base, "continuation", json!(retired_planner)),
            ExposureProfile::Developer,
        ),
    ];

    for (dimension, arguments, exposure_profile) in cases {
        let harness = Harness::with_cursor_key(
            FakeOutcome::SymbolExplain(Err(ClientPortError::Executor)),
            KEY_MATERIAL,
        );
        let error = execute_as(
            &harness.executor,
            VerticalTool::ContextPack,
            arguments,
            exposure_profile,
        )
        .await
        .expect_err(dimension);
        assert_eq!(
            error
                .public_error()
                .expect("binding failure is public")
                .code(),
            ErrorCode::InvalidCursor,
            "{dimension}"
        );
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "{dimension} fails before daemon work"
        );
    }
}

#[tokio::test]
async fn repository_read_cursor_binds_the_complete_effective_budget() {
    const KEY_MATERIAL: [u8; 32] = [0x7E; 32];
    let tool = VerticalTool::CodeLocate;
    let budgeted = with_argument(
        pagination_arguments(tool),
        "budget",
        json!({"max_tokens": 1_000}),
    );
    let cursor = issue_pagination_cursor(
        tool,
        budgeted.clone(),
        ExposureProfile::Developer,
        CursorSigningKey::deterministic(KEY_MATERIAL).expect("test signing key is valid"),
        now_unix_ms(),
        false,
    );

    let valid_harness = Harness::with_cursor_key(
        FakeOutcome::CodeLocate(Err(ClientPortError::Executor)),
        KEY_MATERIAL,
    );
    let error = execute(
        &valid_harness.executor,
        tool,
        with_argument(budgeted.clone(), "cursor", json!(cursor.clone())),
    )
    .await
    .expect_err("valid cursor reaches the failing fake port");
    assert!(error.public_error().is_none());
    assert_eq!(valid_harness.call_count.load(Ordering::Relaxed), 1);

    let changed = with_argument(
        with_argument(
            pagination_arguments(tool),
            "budget",
            json!({"max_tokens": 1_001}),
        ),
        "cursor",
        json!(cursor),
    );
    let changed_harness = Harness::with_cursor_key(
        FakeOutcome::CodeLocate(Err(ClientPortError::Executor)),
        KEY_MATERIAL,
    );
    let error = execute(&changed_harness.executor, tool, changed)
        .await
        .expect_err("cursor cannot be reused under a different budget");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::InvalidCursor)
    );
    assert_eq!(changed_harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn repository_read_pages_match_golden_sequences_without_gaps() {
    const KEY_MATERIAL: [u8; 32] = [0x6D; 32];
    let locate_ids = [symbol(), missing_symbol(), SymbolId::from_bytes([3; 20])];
    let locate_outcomes = locate_ids
        .iter()
        .zip([Some(1), Some(2), None])
        .enumerate()
        .map(|(index, (symbol_id, next))| {
            Ok(locate_page(*symbol_id, &format!("item_{index}"), next))
        })
        .collect();
    let locate_harness = Harness::with_cursor_key(
        FakeOutcome::CodeLocateSequence(Arc::new(Mutex::new(locate_outcomes))),
        KEY_MATERIAL,
    );
    let mut locate_arguments = with_argument(
        pagination_arguments(VerticalTool::CodeLocate),
        "max_results",
        json!(1),
    );
    let mut observed_locate = Vec::new();
    loop {
        let output: CodeLocateOutput = decode(
            execute(
                &locate_harness.executor,
                VerticalTool::CodeLocate,
                locate_arguments.clone(),
            )
            .await
            .expect("code locate page succeeds"),
        );
        let ToolResponse::Success(page) = output else {
            panic!("expected code locate success");
        };
        observed_locate.push(
            page.data.matches[0]
                .symbol_id
                .expect("fixture locate item is a symbol"),
        );
        match page.next_cursor.0 {
            Some(cursor) => {
                assert!(page.truncated);
                locate_arguments = with_argument(locate_arguments, "cursor", json!(cursor));
            }
            None => {
                assert!(!page.truncated);
                break;
            }
        }
    }
    assert_eq!(observed_locate, locate_ids);

    let relationship_targets = [
        missing_symbol(),
        SymbolId::from_bytes([3; 20]),
        SymbolId::from_bytes([4; 20]),
    ];
    let relationship_outcomes = relationship_targets
        .iter()
        .zip([Some(1), Some(2), None])
        .map(|(target, next)| Ok(relationships_page(*target, next)))
        .collect();
    let relationship_harness = Harness::with_cursor_key(
        FakeOutcome::SymbolRelationshipsSequence(Arc::new(Mutex::new(relationship_outcomes))),
        KEY_MATERIAL,
    );
    let mut relationship_arguments = with_argument(
        pagination_arguments(VerticalTool::SymbolRelationships),
        "max_results",
        json!(1),
    );
    let mut observed_relationships = Vec::new();
    loop {
        let output: SymbolRelationshipsOutput = decode(
            execute(
                &relationship_harness.executor,
                VerticalTool::SymbolRelationships,
                relationship_arguments.clone(),
            )
            .await
            .expect("relationships page succeeds"),
        );
        let ToolResponse::Success(page) = output else {
            panic!("expected relationships success");
        };
        observed_relationships.push(page.data.groups[0].items[0].symbol_id);
        match page.next_cursor.0 {
            Some(cursor) => {
                assert!(page.truncated);
                relationship_arguments =
                    with_argument(relationship_arguments, "cursor", json!(cursor));
            }
            None => {
                assert!(!page.truncated);
                break;
            }
        }
    }
    assert_eq!(observed_relationships, relationship_targets);

    let advanced_ids = ["row_a", "row_b", "row_c"];
    let advanced_outcomes = advanced_ids
        .iter()
        .zip([Some(1), Some(2), None])
        .map(|(row_id, next)| Ok(advanced_page(row_id, next)))
        .collect();
    let advanced_harness = Harness::with_cursor_key(
        FakeOutcome::QueryAdvancedSequence(Arc::new(Mutex::new(advanced_outcomes))),
        KEY_MATERIAL,
    );
    let mut advanced_arguments = with_argument(
        pagination_arguments(VerticalTool::QueryAdvanced),
        "max_results",
        json!(1),
    );
    let mut observed_advanced = Vec::new();
    loop {
        let output: QueryAdvancedOutput = decode(
            execute(
                &advanced_harness.executor,
                VerticalTool::QueryAdvanced,
                advanced_arguments.clone(),
            )
            .await
            .expect("advanced page succeeds"),
        );
        let ToolResponse::Success(page) = output else {
            panic!("expected advanced success");
        };
        observed_advanced.push(
            page.data.rows[0]["id"]
                .as_str()
                .expect("fixture id is text")
                .to_owned(),
        );
        match page.next_cursor.0 {
            Some(cursor) => {
                assert!(page.truncated);
                assert_eq!(page.data.completeness, QueryCompleteness::Paged);
                advanced_arguments = with_argument(advanced_arguments, "cursor", json!(cursor));
            }
            None => {
                assert!(!page.truncated);
                assert_eq!(page.data.completeness, QueryCompleteness::Complete);
                break;
            }
        }
    }
    assert_eq!(observed_advanced, advanced_ids);

    let offsets = |harness: &Harness| {
        harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned")
            .iter()
            .map(|call| match call {
                ObservedCall::CodeLocate(request) => request.page_offset(),
                ObservedCall::SymbolRelationships(request) => request.page_offset(),
                ObservedCall::QueryAdvanced(request) => request.page_offset(),
                _ => panic!("pagination fixture recorded an unrelated call"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(offsets(&locate_harness), [0, 1, 2]);
    assert_eq!(offsets(&relationship_harness), [0, 1, 2]);
    assert_eq!(offsets(&advanced_harness), [0, 1, 2]);
}

#[tokio::test]
async fn hard_limits_never_masquerade_as_page_continuations() {
    for resource in ["candidates", "returned_text_bytes"] {
        let budget = PublicError::builder(
            ErrorCode::BudgetExceeded,
            error_definition(ErrorCode::BudgetExceeded).message,
        )
        .detail(
            DetailKey::parse("resource").expect("static detail key is valid"),
            PublicValue::Label(SafeLabel::parse(resource).expect("static resource label is valid")),
        )
        .next_action(NextAction::CorrectField {
            field: DetailKey::parse("budget").expect("static detail key is valid"),
        })
        .build()
        .expect("budget error fixture is checked");
        let harness = Harness::new(FakeOutcome::CodeLocate(Err(ClientPortError::Public(
            Box::new(budget),
        ))));
        let error = execute(
            &harness.executor,
            VerticalTool::CodeLocate,
            pagination_arguments(VerticalTool::CodeLocate),
        )
        .await
        .expect_err("hard search limit remains a domain error");
        assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::BudgetExceeded)
        );
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
    }

    let mut relationships = relationships_page(missing_symbol(), None);
    relationships.result.exact = false;
    relationships.result.truncated = true;
    relationships.result.execution_completeness = truncated_execution(
        client::LimitingResourceKind::Edges,
        client::ContinuationGuidance::ReduceRelations,
    );
    let relationship_harness = Harness::new(FakeOutcome::SymbolRelationships(Ok(relationships)));
    let relationship_output: SymbolRelationshipsOutput = decode(
        execute(
            &relationship_harness.executor,
            VerticalTool::SymbolRelationships,
            pagination_arguments(VerticalTool::SymbolRelationships),
        )
        .await
        .expect("hard-truncated relationships response maps"),
    );
    let ToolResponse::Success(relationship_page) = relationship_output else {
        panic!("expected relationships success");
    };
    assert!(relationship_page.truncated);
    assert!(relationship_page.next_cursor.0.is_none());
    let relationship_warning_codes = relationship_page
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    assert!(relationship_warning_codes.contains(&"result_truncated"));
    assert!(relationship_warning_codes.contains(&"limit_edges"));
    assert!(relationship_warning_codes.contains(&"reduce_relations"));
    assert!(!relationship_warning_codes.contains(&"use_cursor"));

    let mut advanced = advanced_page("partial", None);
    advanced.result.completeness = "truncated".to_owned();
    advanced.result.execution_completeness = truncated_execution(
        client::LimitingResourceKind::Results,
        client::ContinuationGuidance::NarrowScope,
    );
    let advanced_harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced)));
    let advanced_output: QueryAdvancedOutput = decode(
        execute(
            &advanced_harness.executor,
            VerticalTool::QueryAdvanced,
            pagination_arguments(VerticalTool::QueryAdvanced),
        )
        .await
        .expect("hard-truncated advanced response maps"),
    );
    let ToolResponse::Success(advanced_page) = advanced_output else {
        panic!("expected advanced success");
    };
    assert!(advanced_page.truncated);
    assert!(advanced_page.next_cursor.0.is_none());
    let advanced_warning_codes = advanced_page
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    assert!(advanced_warning_codes.contains(&"result_truncated"));
    assert!(advanced_warning_codes.contains(&"limit_results"));
    assert!(advanced_warning_codes.contains(&"narrow_scope"));
    assert!(!advanced_warning_codes.contains(&"use_cursor"));
}

#[tokio::test]
async fn unsupported_partial_results_surface_public_warnings_without_a_cursor() {
    let mut advanced = advanced_page("partial", None);
    advanced.result.completeness = "unsupported".to_owned();
    advanced.result.execution_completeness =
        unsupported_execution(client::LimitingResourceKind::Capability);
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced)));

    let output: QueryAdvancedOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryAdvanced,
            pagination_arguments(VerticalTool::QueryAdvanced),
        )
        .await
        .expect("unsupported partial result maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected advanced success");
    };
    assert!(!output.truncated);
    assert!(output.next_cursor.0.is_none());
    assert_eq!(output.data.completeness, QueryCompleteness::Unsupported);
    let warning_codes = output
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    assert!(warning_codes.contains(&"unsupported_partial"));
    assert!(warning_codes.contains(&"limit_capability"));
    assert!(warning_codes.contains(&"no_continuation"));
    assert!(!warning_codes.contains(&"use_cursor"));
}

#[tokio::test]
async fn paging_cursor_requires_available_execution_continuation() {
    let mut advanced = advanced_page("partial", Some(1));
    advanced.result.execution_completeness = truncated_execution(
        client::LimitingResourceKind::Results,
        client::ContinuationGuidance::NarrowScope,
    );
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced)));

    let error = execute(
        &harness.executor,
        VerticalTool::QueryAdvanced,
        pagination_arguments(VerticalTool::QueryAdvanced),
    )
    .await
    .expect_err("cursor and execution completeness must agree");
    assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
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
            execution_completeness: complete_execution(),
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
                start_line: Some(2),
                end_line: Some(2),
                content: b"xxxxxxxx".to_vec(),
                encoding: client::SourceEncoding::Utf8,
                content_hash: content_hash(),
                language: "rust".to_owned(),
                generated: false,
            }],
            total_source_bytes: 8,
            truncated: false,
            execution_completeness: complete_execution(),
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
            mode: client::RepositoryIndexMode::Structural,
            state: ClientOperationState::Succeeded,
            revision: 8,
            parent_generation: Some(parent_generation()),
            published_generation: Some(generation()),
            discovered_inputs: 4,
            indexed_files: 3,
            entities: 12,
            elapsed_micros: 500,
            estimated_disk_bytes: 4_096,
            diagnostics: Vec::new(),
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
            mode: client::RepositoryIndexMode::Structural,
            state: ClientOperationState::Succeeded,
            revision: 8,
            parent_generation: Some(parent_generation()),
            published_generation: Some(generation()),
            discovered_inputs: 4,
            indexed_files: 3,
            entities: 12,
            elapsed_micros: 500,
            estimated_disk_bytes: 4_096,
            diagnostics: Vec::new(),
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
async fn repository_deep_mode_preserves_the_isolated_project_plan() {
    let response = RepositoryIndexPortResponse::new(
        RepositoryIndex {
            repository: repository(),
            operation: operation(),
            mode: client::RepositoryIndexMode::Deep,
            state: ClientOperationState::Succeeded,
            revision: 8,
            parent_generation: Some(parent_generation()),
            published_generation: Some(generation()),
            discovered_inputs: 4,
            indexed_files: 4,
            entities: 12,
            elapsed_micros: 500,
            estimated_disk_bytes: 4_096,
            diagnostics: Vec::new(),
        },
        IndexPlanSummary {
            scope: IndexPlanScope::Repository,
            mode: IndexMode::Deep,
            providers: vec![
                "rootlight-first-slice-treesitter".to_owned(),
                "rootlight-project-semantics".to_owned(),
            ],
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
            json!({"root": "C:/fixture", "mode": "deep"}),
        )
        .await
        .expect("deep selects the isolated project plan"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repository index success");
    };

    assert_eq!(output.data.accepted_plan.mode, IndexMode::Deep);
    assert!(matches!(
        harness.only_call(),
        ObservedCall::RepositoryIndex(RepositoryIndexPortRequest {
            mode: IndexMode::Deep,
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
                mode: client::RepositoryIndexMode::Structural,
                state: ClientOperationState::Succeeded,
                revision: 8,
                parent_generation: Some(parent_generation()),
                published_generation: Some(generation()),
                discovered_inputs: 4,
                indexed_files: 3,
                entities: 12,
                elapsed_micros: 500,
                estimated_disk_bytes: 4_096,
                diagnostics: Vec::new(),
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
    let mut first = execute(
        &harness.executor,
        VerticalTool::CodeLocate,
        arguments.clone(),
    )
    .await
    .expect("first locate maps");
    let mut second = execute(&harness.executor, VerticalTool::CodeLocate, arguments)
        .await
        .expect("second locate maps");
    normalize_measured_usage(&mut first);
    normalize_measured_usage(&mut second);
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
    assert_eq!(output.usage.wall_time_ms, 0);
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
    assert!(
        calls
            .iter()
            .all(|call| matches!(call, ObservedCall::CodeLocate(_))),
        "ordinary target retrieval must not add a repository-status preflight"
    );
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
        "budget": {"max_tokens": 16000},
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
async fn query_batch_public_profiles_preserve_child_semantics() {
    let mut semantics = Vec::new();

    for profile in ["compact", "standard", "evidence"] {
        let harness = batch_harness();
        let call_count = Arc::clone(&harness.call_count);
        let router = ToolRouter::new(
            harness.executor,
            rootlight_mcp_contract::ExposureProfile::Developer,
        )
        .expect("router compiles");
        let response = router
            .handle(
                operating_request(json!({
                    "name": "query.batch",
                    "arguments": {
                        "repository": {"repository_id": repository()},
                        "response_profile": profile,
                        "operations": [{
                            "id": "find",
                            "tool": "code.locate",
                            "arguments": {"query": "publish", "max_results": 5}
                        }]
                    }
                })),
                cancellation(),
            )
            .await;
        let HandlerResponse::Success(result) = response else {
            panic!("{profile} query.batch returns an MCP tool result");
        };

        assert_eq!(result["isError"], false, "{profile} profile is public");
        let content = &result["structuredContent"];
        assert_eq!(content["data"]["batch_status"], "ok");
        assert_eq!(content["data"]["generation_id"], json!(generation()));
        assert_eq!(content["data"]["operation_results"][0]["status"], "ok");
        assert_eq!(call_count.load(Ordering::Relaxed), 2);
        semantics.push((
            content["data"]["operation_results"][0]["id"].clone(),
            content["data"]["operation_results"][0]["tool"].clone(),
            content["data"]["operation_results"][0]["status"].clone(),
            content["data"]["operation_results"][0]["data"]["matches"][0]["symbol_id"].clone(),
        ));
    }

    assert!(
        semantics.windows(2).all(|pair| pair[0] == pair[1]),
        "representation profiles must not change child identity or status"
    );
}

#[tokio::test]
async fn query_batch_identity_survives_active_generation_race() {
    let active_generation = Arc::new(Mutex::new(generation()));
    let locate_calls = Arc::new(AtomicUsize::new(0));
    let harness = Harness::new(FakeOutcome::BatchGenerationRace {
        status: Box::new(Ok(repository_status_response())),
        locate: Ok(locate_response()),
        active_generation: Arc::clone(&active_generation),
        locate_calls: Arc::clone(&locate_calls),
    });

    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": "active",
                "operations": [
                    {"id": "before_publish", "tool": "code.locate", "arguments": {"query": "publish"}},
                    {"id": "after_publish", "tool": "code.locate", "arguments": {"query": "stage"}}
                ]
            }),
        )
        .await
        .expect("generation publication cannot move a pinned batch"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };

    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(output.repository.repository_id, repository());
    assert_eq!(output.generation.generation_id, generation());
    assert_eq!(output.data.generation_id, generation());
    assert_eq!(
        *active_generation
            .lock()
            .expect("fake active generation is not poisoned"),
        alternate_generation(),
        "the fake publishes a new active generation during child execution"
    );
    assert_eq!(locate_calls.load(Ordering::SeqCst), 2);

    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, ObservedCall::RepositoryStatus(_)))
            .count(),
        1,
        "an accepted batch resolves repository and generation exactly once"
    );
    let child_generations: Vec<_> = calls
        .iter()
        .filter_map(|call| match call {
            ObservedCall::CodeLocate(request) => Some(request.generation()),
            _ => None,
        })
        .collect();
    assert_eq!(
        child_generations,
        vec![
            ClientGenerationSelector::Generation(generation()),
            ClientGenerationSelector::Generation(generation()),
        ],
        "every child receives the original exact generation"
    );
}

#[tokio::test]
async fn query_batch_identity_survives_first_operation_failure() {
    let harness = batch_harness();
    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": "active",
                "operations": [
                    {
                        "id": "fails",
                        "tool": "symbol.relationships",
                        "arguments": {"symbol_ids": [symbol()], "relations": ["calls"]}
                    },
                    {
                        "id": "succeeds",
                        "tool": "code.locate",
                        "arguments": {"query": "publish"}
                    }
                ]
            }),
        )
        .await
        .expect("independent work continues after a child failure"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected pinned batch envelope");
    };

    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(output.repository.repository_id, repository());
    assert_eq!(output.generation.generation_id, generation());
    assert_eq!(output.data.generation_id, generation());
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Error
    );
    assert_eq!(
        output.data.operation_results[1].status,
        BatchOperationStatus::Ok
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, ObservedCall::RepositoryStatus(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn query_batch_identity_rejects_wrong_child_without_mixed_aggregate() {
    let mut wrong_repository = locate_response();
    wrong_repository.result.context.repository = alternate_repository();
    let mut wrong_generation = locate_response();
    wrong_generation.result.context.generation = alternate_generation();

    for wrong_response in [wrong_repository, wrong_generation] {
        let responses = Arc::new(Mutex::new(VecDeque::from([
            Ok(wrong_response),
            Ok(locate_response()),
        ])));
        let harness = Harness::new(FakeOutcome::BatchLocateSequence {
            status: Box::new(Ok(repository_status_response())),
            locate: Arc::clone(&responses),
        });
        let error = execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "operations": [
                    {"id": "wrong", "tool": "code.locate", "arguments": {"query": "publish"}},
                    {"id": "would_be_valid", "tool": "code.locate", "arguments": {"query": "stage"}}
                ]
            }),
        )
        .await
        .expect_err("a mismatched child identity fails the complete aggregate");

        assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
        assert!(error.public_error().is_none());
        assert_eq!(
            responses
                .lock()
                .expect("fake batch locate sequence is not poisoned")
                .len(),
            1,
            "no later child is published after an identity-integrity failure"
        );
        let calls = harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], ObservedCall::RepositoryStatus(_)));
        assert!(matches!(calls[1], ObservedCall::CodeLocate(_)));
    }
}

#[tokio::test]
async fn query_batch_identity_preserves_retirement_and_corruption_errors() {
    for code in [ErrorCode::StaleGeneration, ErrorCode::IndexCorrupt] {
        let public = PublicError::builder(code, error_definition(code).message)
            .repository(repository())
            .generation(generation())
            .build()
            .expect("registered generation failure is a valid public error");
        let harness = Harness::new(FakeOutcome::Batch {
            status: Box::new(Ok(repository_status_response())),
            locate: Err(ClientPortError::Public(Box::new(public))),
        });
        let output: QueryBatchOutput = decode(
            execute(
                &harness.executor,
                VerticalTool::QueryBatch,
                json!({
                    "repository": {"repository_id": repository()},
                    "operations": [
                        {"id": "read", "tool": "code.locate", "arguments": {"query": "publish"}}
                    ]
                }),
            )
            .await
            .expect("a checked pinned-generation failure remains in the batch envelope"),
        );
        let ToolResponse::Success(output) = output else {
            panic!("expected checked batch envelope");
        };

        assert_eq!(output.data.batch_status, BatchStatus::Error);
        assert_eq!(output.repository.repository_id, repository());
        assert_eq!(output.generation.generation_id, generation());
        assert_eq!(
            output.data.operation_results[0]
                .error
                .as_ref()
                .map(PublicError::code),
            Some(code)
        );
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 2);
    }

    let retired = PublicError::builder(
        ErrorCode::StaleGeneration,
        error_definition(ErrorCode::StaleGeneration).message,
    )
    .repository(repository())
    .generation(generation())
    .build()
    .expect("registered stale-generation failure is valid");
    let harness = Harness::new(FakeOutcome::Batch {
        status: Box::new(Err(ClientPortError::Public(Box::new(retired)))),
        locate: Ok(locate_response()),
    });
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "not_run", "tool": "code.locate", "arguments": {"query": "publish"}}
            ]
        }),
    )
    .await
    .expect_err("a retired generation at preflight prevents child execution");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::StaleGeneration)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn query_batch_executes_plan_change_under_the_pinned_identity() {
    let harness = Harness::new(FakeOutcome::BatchPlanChange {
        status: Box::new(Ok(repository_status_response())),
        locate: Err(ClientPortError::Executor),
        plan_change: Box::new(Ok(batch_plan_change_response())),
    });
    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": "active",
                "budget": {"max_tokens": 16000},
                "operations": [{
                    "id": "plan",
                    "tool": "plan.change",
                    "arguments": {
                        "objective": "bug_fix",
                        "objective_text": "fix the defect",
                        "targets": [{"symbol_id": symbol()}]
                    }
                }]
            }),
        )
        .await
        .expect("batch change planning succeeds"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };

    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(output.data.operation_results.len(), 1);
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Ok
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 6);
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert!(matches!(calls[0], ObservedCall::RepositoryStatus(_)));
    assert!(matches!(calls[1], ObservedCall::ChangeImpact(_)));
    assert!(matches!(calls[2], ObservedCall::SymbolRelationships(_)));
    assert!(matches!(calls[3], ObservedCall::TestsSelect(_)));
    assert!(matches!(calls[4], ObservedCall::ArchitectureOverview(_)));
    let ObservedCall::PlanChange(request) = &calls[5] else {
        panic!("expected plan change call");
    };
    assert_eq!(
        request.generation(),
        &GenerationSelector::Explicit(generation())
    );
    assert_eq!(request.max_steps(), Some(100));
}

#[tokio::test]
async fn plan_change_data_is_identical_in_standalone_and_batch_execution() {
    let standalone_harness =
        Harness::new(FakeOutcome::PlanChange(Ok(batch_plan_change_response())));
    let standalone: PlanChangeOutput = decode(
        execute(
            &standalone_harness.executor,
            VerticalTool::PlanChange,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation(),
                "objective": "bug_fix",
                "objective_text": "fix the defect",
                "targets": [{"symbol_id": symbol()}],
                "budget": {"max_tokens": 16000}
            }),
        )
        .await
        .expect("standalone change planning succeeds"),
    );
    let ToolResponse::Success(standalone) = standalone else {
        panic!("expected standalone plan success");
    };

    let batch_harness = Harness::new(FakeOutcome::BatchPlanChange {
        status: Box::new(Ok(repository_status_response())),
        locate: Err(ClientPortError::Executor),
        plan_change: Box::new(Ok(batch_plan_change_response())),
    });
    let batch: QueryBatchOutput = decode(
        execute(
            &batch_harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation(),
                "budget": {"max_tokens": 16000},
                "operations": [{
                    "id": "plan",
                    "tool": "plan.change",
                    "arguments": {
                        "objective": "bug_fix",
                        "objective_text": "fix the defect",
                        "targets": [{"symbol_id": symbol()}]
                    }
                }]
            }),
        )
        .await
        .expect("batch change planning succeeds"),
    );
    let ToolResponse::Success(batch) = batch else {
        panic!("expected batch plan success");
    };

    assert_eq!(
        batch.data.operation_results[0].data,
        Some(serde_json::to_value(standalone.data).expect("standalone plan data serializes"))
    );
}

#[tokio::test]
async fn query_batch_context_pack_reuses_the_pinned_identity() {
    let harness = Harness::new(FakeOutcome::BatchContextPack {
        status: Box::new(Ok(repository_status_response())),
        explain: Ok(explain_response(source_reference(4, 12, 2, 2))),
    });
    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": "active",
                "operations": [{
                    "id": "context",
                    "tool": "context.pack",
                    "arguments": {
                        "task": "fix the duplicate payment bug",
                        "seeds": {"symbols": [symbol()]},
                        "token_budget": 4_500
                    }
                }]
            }),
        )
        .await
        .expect("batch context assembly succeeds"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };

    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Ok
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, ObservedCall::RepositoryStatus(_)))
            .count(),
        1,
        "the batch identity is resolved exactly once"
    );
    assert!(matches!(
        calls.first(),
        Some(ObservedCall::RepositoryStatus(_))
    ));
    assert!(
        calls
            .iter()
            .skip(1)
            .all(|call| !matches!(call, ObservedCall::RepositoryStatus(_))),
        "context evidence collection must not resolve identity again"
    );
    assert!(
        calls
            .iter()
            .any(|call| matches!(call, ObservedCall::SymbolExplain(_))),
        "context assembly retrieves symbol evidence"
    );
}

#[tokio::test]
async fn query_batch_preserves_child_page_continuations() {
    let harness = Harness::with_cursor_key(
        FakeOutcome::Batch {
            status: Box::new(Ok(repository_status_response())),
            locate: Ok(locate_page(symbol(), "publish", Some(1))),
        },
        [0x71; 32],
    );
    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "generation": "active",
                "operations": [
                    {
                        "id": "find",
                        "tool": "code.locate",
                        "arguments": {"query": "publish", "max_results": 1}
                    }
                ]
            }),
        )
        .await
        .expect("batch page succeeds"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };
    let child = &output.data.operation_results[0];
    assert_eq!(child.status, BatchOperationStatus::Ok);
    assert!(child.truncated);
    assert!(
        child.next_cursor.0.is_some(),
        "batch shaping must preserve the child continuation"
    );

    let offsets: Vec<_> = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned")
        .iter()
        .filter_map(|call| match call {
            ObservedCall::CodeLocate(request) => Some(request.page_offset()),
            _ => None,
        })
        .collect();
    assert_eq!(offsets, [0]);
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

    assert_canonical_budget_error(
        error
            .public_error()
            .expect("aggregate budget exhaustion is a checked public error"),
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "identity is pinned before the minimum publish envelope rejects child dispatch"
    );
}

#[tokio::test]
async fn query_batch_admits_known_plan_minima_before_identity_resolution() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "first", "tool": "context.pack", "arguments": {
                    "task": "first",
                    "seeds": {"symbols": [symbol()]},
                    "token_budget": 500
                }},
                {"id": "second", "tool": "context.pack", "arguments": {
                    "task": "second",
                    "seeds": {"symbols": [symbol()]},
                    "token_budget": 500
                }}
            ],
            "budget": {"max_tokens": 900}
        }),
    )
    .await
    .expect_err("known child minima exceed the shared static token ceiling");
    assert_canonical_budget_error(
        error
            .public_error()
            .expect("static plan admission returns a checked budget error"),
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "static shared-budget rejection must precede identity resolution"
    );
}

#[tokio::test]
async fn query_batch_keeps_plan_failures_top_level() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "first", "tool": "code.locate", "depends_on": ["second"], "arguments": {
                    "query": "publish"
                }},
                {"id": "second", "tool": "code.locate", "depends_on": ["first"], "arguments": {
                    "query": "stage"
                }}
            ]
        }),
    )
    .await
    .expect_err("a cyclic batch plan is rejected at the request boundary");
    let public = error
        .public_error()
        .expect("batch plan rejection is a checked public error");
    assert_eq!(public.code(), ErrorCode::InvalidArgument);
    assert_eq!(
        public.message(),
        error_definition(ErrorCode::InvalidArgument).message
    );
    assert_eq!(
        public.next_actions(),
        &[NextAction::CorrectField {
            field: DetailKey::parse("operations").expect("static detail key is valid"),
        }]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "invalid plans remain top-level and start no repository work"
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
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
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
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
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
    .with_local_budget(Some(local_budget))
    .with_pinned_identity(agent_identity_from_status(repository_status_response()));

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
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::LocalTimeout)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([("query.batch", "operations[].local_budget.timeout_ms")])
    );

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
            invalid_cursor: harness.executor.invalid_cursor.clone(),
            exposure_profile: ExposureProfile::Developer,
            cursor_key: harness.executor.cursor_key,
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
        .with_pinned_identity(agent_identity_from_status(repository_status_response()))
        .with_local_deadline(local);
        let error = adapter
            .execute(request, context)
            .await
            .expect_err("the pending child reaches its effective deadline");
        let (error, usage) = error.into_parts();
        assert_eq!(error, expected);
        assert!(
            usage
                .expect("deadline failures retain measured work")
                .wall_time_ms
                > 0
        );
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
        (McpTool::PlanChange, BatchTool::PlanChange),
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
}

#[tokio::test]
async fn query_batch_defers_allowed_bindings_until_runtime_materialization() {
    let harness = Harness::new(FakeOutcome::BatchPlanChange {
        status: Box::new(Ok(repository_status_response())),
        locate: Ok(locate_response()),
        plan_change: Box::new(Ok(batch_plan_change_response())),
    });
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "budget": {"max_tokens": 16000},
        "operations": [
            {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
            {"id": "refine", "tool": "plan.change", "depends_on": ["find"], "arguments": {
                "objective": "bug_fix",
                "objective_text": "fix the defect",
                "targets": [{
                    "symbol_id": {"$from": "find", "source": "symbol_id", "index": 0}
                }]
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
        7,
        "identity, dependency, evidence, and plan calls prove deferred execution"
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let ObservedCall::PlanChange(request) = calls.last().expect("plan call is recorded") else {
        panic!("expected the final call to plan the change");
    };
    assert_eq!(request.target_symbols(), &[symbol()]);
}

#[tokio::test]
async fn query_batch_skips_dependents_of_an_unavailable_subtool() {
    let harness = batch_harness();
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": "active",
        "operations": [
            {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
            {"id": "rels", "tool": "symbol.relationships", "arguments": {
                "symbol_ids": [symbol()],
                "relations": ["calls"]
            }},
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
    // Identity and both scheduled children reach the port; the dependent does not.
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn query_batch_keeps_invalid_binding_inside_the_operation_result() {
    let harness = Harness::new(FakeOutcome::BatchPlanChange {
        status: Box::new(Ok(repository_status_response())),
        locate: Ok(locate_response()),
        plan_change: Box::new(Ok(batch_plan_change_response())),
    });
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "refine", "tool": "plan.change", "depends_on": ["find"], "arguments": {
                    "objective": "bug_fix",
                    "objective_text": "fix the defect",
                    "targets": [{
                        "symbol_id": {"$from": "find", "source": "symbol_id", "index": 99}
                    }]
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
async fn query_batch_skips_target_dependents_after_runtime_binding_failure() {
    let harness = Harness::new(FakeOutcome::BatchPlanChange {
        status: Box::new(Ok(repository_status_response())),
        locate: Ok(locate_response()),
        plan_change: Box::new(Ok(batch_plan_change_response())),
    });
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "refine", "tool": "plan.change", "depends_on": ["find"], "arguments": {
                    "objective": "bug_fix",
                    "objective_text": "fix the defect",
                    "targets": [{
                        "symbol_id": {
                            "$from": "find",
                            "source": "symbol_id",
                            "index": 99
                        }
                    }]
                }},
                {"id": "after", "tool": "code.locate", "depends_on": ["refine"], "arguments": {
                    "query": "stage"
                }}
            ]
        }),
    )
    .await
    .expect("runtime binding failure remains in the ordered batch envelope");
    let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
        panic!("expected batch success envelope");
    };
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .map(|result| (result.id.as_str(), result.status))
            .collect::<Vec<_>>(),
        [
            ("find", BatchOperationStatus::Ok),
            ("refine", BatchOperationStatus::Error),
            ("after", BatchOperationStatus::SkippedDependency),
        ]
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        2,
        "only identity and the binding source may reach the client port"
    );
}

#[tokio::test]
async fn query_batch_preserves_request_order_when_execution_order_differs() {
    let responses = Arc::new(Mutex::new(VecDeque::from([
        Ok(locate_response()),
        Ok(locate_response()),
    ])));
    let harness = Harness::new(FakeOutcome::BatchLocateSequence {
        status: Box::new(Ok(repository_status_response())),
        locate: Arc::clone(&responses),
    });
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "requested_first", "tool": "code.locate", "depends_on": ["source"], "arguments": {
                    "query": "requested-first"
                }},
                {"id": "source", "tool": "code.locate", "arguments": {
                    "query": "executed-first"
                }}
            ]
        }),
    )
    .await
    .expect("reverse request and execution order is valid");
    let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
        panic!("expected batch success envelope");
    };
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .map(|result| result.id.as_str())
            .collect::<Vec<_>>(),
        ["requested_first", "source"]
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let queries = calls
        .iter()
        .filter_map(|call| match call {
            ObservedCall::CodeLocate(call) => Some(call.request.query()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(queries, ["executed-first", "requested-first"]);
}

#[tokio::test]
async fn query_batch_rejects_incompatible_binding_types_during_static_preflight() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "refine", "tool": "code.locate", "depends_on": ["find"], "arguments": {
                    "query": "publish",
                    "search_modes": {"$from": "find", "source": "symbol_id", "index": 0}
                }},
                {"id": "later", "tool": "code.locate", "depends_on": ["refine"], "arguments": {
                    "query": "stage"
                }}
            ]
        }),
    )
    .await
    .expect_err("an incompatible binding pair fails static preflight");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::InvalidArgument)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
    assert!(
        harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned")
            .is_empty()
    );
}

#[tokio::test]
async fn query_batch_does_not_launder_malformed_limits_through_budget_lowering() {
    let overflow =
        serde_json::from_str::<Value>("18446744073709551616").expect("JSON number is valid");
    for malformed in [json!("bad"), json!(-1), json!(1.5), Value::Null, overflow] {
        let harness = batch_harness();
        let error = execute(
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
        .expect_err("deterministic child validation fails the complete static plan");
        assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::InvalidArgument)
        );
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "static child errors must fail before identity or child dispatch"
        );
    }
}

#[tokio::test]
async fn query_batch_rejects_later_static_arguments_before_any_call() {
    let harness = batch_harness();
    let error = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}},
                {"id": "mixed", "tool": "plan.change", "depends_on": ["find"], "arguments": {
                    "objective": "not_an_objective",
                    "objective_text": "fix the defect",
                    "targets": [{
                        "symbol_id": {
                            "$from": "find",
                            "source": "symbol_id",
                            "index": 0
                        }
                    }]
                }}
            ]
        }),
    )
    .await
    .expect_err("a later deterministic child defect rejects the complete static plan");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        0,
        "neither identity nor the harmless first operation may run"
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
async fn query_batch_forwards_source_context_fields() {
    let requested = source_reference(4, 12, 2, 2);
    let harness = Harness::new(FakeOutcome::BatchSourceRead {
        status: Box::new(Ok(repository_status_response())),
        source: Ok(source_read_response(requested.clone())),
    });
    let output: QueryBatchOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "operations": [{
                    "id": "read",
                    "tool": "source.read",
                    "arguments": {
                        "references": [{
                            "source_ref": wire_source_reference(4, 12, 2, 2)
                        }],
                        "context_lines_before": 2,
                        "context_lines_after": 3
                    }
                }]
            }),
        )
        .await
        .expect("source context fields are accepted inside a batch"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch success");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Ok
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let [
        ObservedCall::RepositoryStatus(_),
        ObservedCall::SourceRead(request),
    ] = calls.as_slice()
    else {
        panic!("batch pins identity once before the source read");
    };
    assert_eq!(request.context_lines_before(), 2);
    assert_eq!(request.context_lines_after(), 3);
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
                        "source": "symbol_id",
                        "index": 0
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
async fn query_batch_rejects_legacy_child_profile_overrides_before_identity_resolution() {
    for (tool, arguments) in [
        (
            "change.impact",
            json!({
                "change": {"symbol_ids": [symbol()]},
                "profile": "standard"
            }),
        ),
        (
            "tests.select",
            json!({
                "seeds": {"symbols": [symbol()]},
                "profile": "evidence"
            }),
        ),
    ] {
        let harness = batch_harness();
        let error = execute(
            &harness.executor,
            VerticalTool::QueryBatch,
            json!({
                "repository": {"repository_id": repository()},
                "operations": [{
                    "id": "profile_override",
                    "tool": tool,
                    "arguments": arguments
                }]
            }),
        )
        .await
        .expect_err("a child response-profile override rejects the batch");

        assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::InvalidArgument),
            "{tool} returned the wrong child-profile error"
        );
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "{tool} resolved batch identity before rejecting its child profile"
        );
    }
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
                        "source": "symbol_id",
                        "index": 0
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
async fn query_batch_keeps_local_deadline_budget_error_inside_the_operation_result() {
    let harness = Harness::new(FakeOutcome::BatchPendingLocate {
        status: Box::new(Ok(repository_status_response())),
    });
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "timed", "tool": "code.locate", "arguments": {
                    "query": "publish"
                }, "local_budget": {"timeout_ms": 1}}
            ]
        }),
    )
    .await
    .expect("a local child deadline remains inside the batch envelope");
    let ToolResponse::Success(output) = decode::<QueryBatchOutput>(output) else {
        panic!("expected batch success envelope");
    };
    assert_eq!(output.data.batch_status, BatchStatus::Error);
    assert_eq!(output.data.generation_id, generation());
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Error
    );
    assert_canonical_budget_error(
        output.data.operation_results[0]
            .error
            .as_ref()
            .expect("local deadline records a child budget error"),
    );
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        2,
        "identity and the pending child cross the client port"
    );
}

#[tokio::test]
async fn query_batch_root_gate_admits_child_budget_dimensions() {
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

    let token_harness = batch_harness();
    let token_calls = Arc::clone(&token_harness.call_count);
    let token_router = ToolRouter::new(
        token_harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let token_response = token_router
        .handle(
            operating_request(json!({
                "name": "query.batch",
                "arguments": {
                    "repository": {"repository_id": repository()},
                    "budget": {"max_tokens": 1000},
                    "operations": [{
                        "id": "bounded",
                        "tool": "code.locate",
                        "arguments": {"query": "publish"},
                        "local_budget": {"max_tokens": 500}
                    }]
                }
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(token_result) = token_response else {
        panic!("max_tokens case returns an MCP tool result");
    };
    assert_eq!(token_result["isError"], false);
    assert_eq!(
        token_result["structuredContent"]["data"]["operation_results"][0]["status"],
        "ok"
    );
    let structured = &token_result["structuredContent"];
    let encoded = serde_json::to_vec(structured).expect("batch response serializes");
    assert_eq!(
        structured["usage"]["json_bytes"],
        json!(encoded.len()),
        "batch usage must report the exact final serialized response"
    );
    assert_eq!(
        structured["usage"]["estimated_tokens"],
        json!(rootlight_mcp_contract::accounting::estimate_tokens(
            encoded.len()
        ))
    );
    assert!(
        structured["usage"]["estimated_tokens"]
            .as_u64()
            .expect("batch usage contains estimated tokens")
            <= 1_000
    );
    assert_eq!(token_calls.load(Ordering::Relaxed), 2);
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
    assert!(request.include_provenance());
    assert_eq!(request.provenance_level(), ProvenanceLevel::Compact);
    assert_eq!(request.symbols, [symbol(), missing_symbol()]);
}

#[tokio::test]
async fn maps_truncated_symbol_explain_without_reclassifying_omitted_ids() {
    let mut response = explain_response(source_reference(4, 12, 2, 2));
    response.result.unresolved_symbols.clear();
    response.result.truncated = true;
    response.result.execution_completeness = truncated_execution(
        client::LimitingResourceKind::Results,
        client::ContinuationGuidance::SplitRequest,
    );
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(response)));
    let output: SymbolExplainOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::SymbolExplain,
            json!({
                "repository": {"repository_id": repository()},
                "symbol_ids": [symbol(), missing_symbol()]
            }),
        )
        .await
        .expect("bounded symbol explanation maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected bounded symbol explanation success");
    };
    assert!(output.truncated);
    assert_eq!(output.data.symbols.len(), 1);
    assert!(output.data.unresolved_ids.is_empty());
    assert_eq!(output.completeness.state, CompletenessState::Truncated);
}

#[tokio::test]
async fn context_pack_assembles_definition_evidence_under_budget() {
    let mut response = explain_response(source_reference(4, 12, 2, 2));
    // The daemon's rich response can exceed the compact pack reservation even
    // when its projected definition is small enough for the caller's budget.
    response.result.context.usage.estimated_tokens = 2_914;
    response.result.context.usage.json_bytes = 2_914;
    response.result.context.usage.memory_bytes = Some(2_050);
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
    assert_eq!(output.data.repositories[0].display_name, "repository");
    assert_eq!(output.data.repositories[0].generation_count, 1);
    assert_eq!(output.usage.rows, 1);
    assert!(output.snapshot_id.as_str().starts_with("catalog1_"));
}

#[tokio::test]
async fn repo_list_empty_catalog_is_a_success_with_exact_accounting() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: Vec::new(),
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::RepoList,
        json!({"max_results": 200}),
    )
    .await
    .expect("an empty catalog is a successful page");
    let serialized =
        serde_json::to_vec(&Value::Object(output.clone())).expect("catalog response serializes");
    let json_bytes = output["usage"]["json_bytes"]
        .as_u64()
        .expect("json byte count is numeric");
    let estimated_tokens = output["usage"]["estimated_tokens"]
        .as_u64()
        .expect("token estimate is numeric");

    assert_eq!(output["schema_version"], "2.0");
    assert_eq!(output["data"]["repositories"], json!([]));
    assert_eq!(output["data"]["total_count"], 0);
    assert_eq!(output["truncated"], false);
    assert!(output["next_cursor"].is_null());
    assert_eq!(output["usage"]["rows"], 0);
    assert!(
        output["usage"]["wall_time_ms"]
            .as_u64()
            .is_some_and(|elapsed| elapsed >= 1),
        "elapsed time is measured through mapping and accounting stabilization"
    );
    assert_eq!(
        json_bytes,
        u64::try_from(serialized.len()).expect("test response size fits u64")
    );
    assert!(
        json_bytes >= 100,
        "the fixed point must cross the one-to-three digit byte boundary"
    );
    assert_eq!(
        estimated_tokens,
        rootlight_mcp_contract::accounting::estimate_tokens(serialized.len())
    );
    assert!(output.get("repository").is_none());
    assert!(output.get("generation").is_none());
    assert!(output.get("coverage").is_none());
    assert_eq!(output["usage"]["trace_id"], "catalog-page");
    assert!(!String::from_utf8_lossy(&serialized).contains("C:\\"));
    assert!(!String::from_utf8_lossy(&serialized).contains("/Users/"));
}

#[tokio::test]
async fn repo_list_exact_page_boundary_has_no_continuation() {
    let entries = (1..=2)
        .map(|marker| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([marker; 16]),
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
    let output: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2}),
        )
        .await
        .expect("exact-boundary page maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repo list success");
    };
    assert_eq!(output.data.repositories.len(), 2);
    assert_eq!(output.data.total_count, 2);
    assert!(!output.truncated);
    assert!(output.next_cursor.0.is_none());
    assert_eq!(output.usage.rows, 2);
}

#[tokio::test]
async fn repo_list_paginates_with_authenticated_cursor() {
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::OutputSelection)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            ("repo.list", "cursor"),
            ("repo.list", "max_results"),
            ("repo.list", "query"),
            ("repo.list", "states")
        ])
    );

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
    assert_ne!(
        first.data.repositories[0].repository_id,
        second.data.repositories[0].repository_id
    );
    assert!(!second.truncated);
    assert!(second.next_cursor.0.is_none());
    assert_eq!(second.usage.rows, 1);
}

#[tokio::test]
async fn pinned_catalog_continuation_ignores_a_new_live_order() {
    let snapshot = [17; 32];
    let responses = VecDeque::from([
        Ok(catalog_page(
            snapshot,
            vec![catalog_entry(1, "same"), catalog_entry(2, "same")],
            3,
            true,
        )),
        // A newly inserted live entry that sorts before marker 3 is absent:
        // the daemon responds from the snapshot pinned by page one.
        Ok(catalog_page(
            snapshot,
            vec![catalog_entry(3, "same")],
            3,
            false,
        )),
    ]);
    let harness = Harness::with_cursor_key(
        FakeOutcome::RepositoryCatalogPageSequence(Arc::new(Mutex::new(responses))),
        [7; 32],
    );
    let first: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2}),
        )
        .await
        .expect("first snapshot page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    let cursor = first.next_cursor.0.expect("first page continues");
    let second: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({"max_results": 2, "cursor": cursor.as_str()}),
        )
        .await
        .expect("pinned continuation maps"),
    );
    let ToolResponse::Success(second) = second else {
        panic!("expected final page success");
    };
    let ids: Vec<_> = first
        .data
        .repositories
        .iter()
        .chain(&second.data.repositories)
        .map(|entry| entry.repository_id)
        .collect();
    assert_eq!(
        ids,
        vec![
            RepositoryId::from_bytes([1; 16]),
            RepositoryId::from_bytes([2; 16]),
            RepositoryId::from_bytes([3; 16])
        ],
        "equal display names remain ordered by repository identity without duplicates"
    );
    assert_eq!(first.snapshot_id, second.snapshot_id);

    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let Some(ObservedCall::RepositoryList(continuation)) = calls.get(1) else {
        panic!("second call is a catalog continuation");
    };
    assert_eq!(
        continuation.snapshot_id(),
        Some(RepositoryCatalogSnapshotId::from_bytes(snapshot))
    );
    assert!(continuation.after().is_some());
}

#[test]
fn repo_list_query_normalization_is_case_folded_and_canonical() {
    let normalized = |query: Option<&str>| {
        RepositoryCatalogPageRequest::new(20, query, None, None, None)
            .expect("test query is valid")
            .normalized_query()
            .map(str::to_owned)
    };
    assert_eq!(normalized(Some("Straße")), Some("strasse".to_owned()));
    assert_eq!(normalized(Some("STRASSE")), Some("strasse".to_owned()));
    assert_eq!(normalized(Some("e\u{301}")), normalized(Some("é")));
    assert_eq!(normalized(Some("")), None);
    assert_eq!(normalized(None), None);
}

#[tokio::test]
async fn repo_list_forwards_canonical_query_and_state_filters() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: vec![
            RepositoryListEntry {
                repository_id: RepositoryId::from_bytes([1; 16]),
                active_generation: generation(),
                languages: vec!["rust".to_owned()],
                structural_freshness: "current".to_owned(),
                semantic_freshness: "current".to_owned(),
                state: "ready".to_owned(),
            },
            RepositoryListEntry {
                repository_id: RepositoryId::from_bytes([2; 16]),
                active_generation: generation(),
                languages: vec!["rust".to_owned()],
                structural_freshness: "current".to_owned(),
                semantic_freshness: "stale".to_owned(),
                state: "degraded".to_owned(),
            },
        ],
    })));
    let output: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({
                "query": "Straße",
                "states": ["ready", "ready"],
                "max_results": 10
            }),
        )
        .await
        .expect("filtered catalog page maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected catalog success");
    };
    assert_eq!(output.data.total_count, 1);
    assert_eq!(output.data.repositories.len(), 1);
    assert_eq!(output.data.repositories[0].state, RepositoryState::Ready);
    assert_eq!(output.data.repositories[0].display_name, "strasse");
    let ObservedCall::RepositoryList(request) = harness.only_call() else {
        panic!("expected one catalog page call");
    };
    assert_eq!(request.normalized_query(), Some("strasse"));
    assert_eq!(
        request.states(),
        Some([RepositoryCatalogState::Ready].as_slice())
    );
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
        assert_eq!(request.normalized_query(), Some("strasse"));
    }
}

#[tokio::test]
async fn repo_list_cursor_binds_query_and_state_presence_before_daemon_work() {
    let entries: Vec<_> = (1..=3)
        .map(|marker| RepositoryListEntry {
            repository_id: RepositoryId::from_bytes([marker; 16]),
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
            json!({"max_results": 2}),
        )
        .await
        .expect("first page maps"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first page success");
    };
    let cursor = first.next_cursor.0.expect("first page continues");

    for (label, changed) in [
        (
            "query",
            json!({"max_results": 2, "query": "other", "cursor": cursor.as_str()}),
        ),
        (
            "explicit empty states",
            json!({"max_results": 2, "states": [], "cursor": cursor.as_str()}),
        ),
        (
            "state filter",
            json!({"max_results": 2, "states": ["ready"], "cursor": cursor.as_str()}),
        ),
        (
            "page size",
            json!({"max_results": 3, "cursor": cursor.as_str()}),
        ),
    ] {
        let error = execute(&harness.executor, VerticalTool::RepoList, changed)
            .await
            .expect_err(label);
        let public = error
            .public_error()
            .expect("cursor context failure is public");
        assert_eq!(public.code(), ErrorCode::InvalidCursor, "{label}");
        assert!(
            public
                .next_actions()
                .contains(&NextAction::RestartEnumeration),
            "{label}"
        );
    }
    assert_eq!(
        harness.call_count.load(Ordering::Relaxed),
        1,
        "context mismatches are authenticated before daemon lookup"
    );
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
    let public_json = serde_json::to_string(
        error
            .public_error()
            .expect("snapshot failure is a checked public error"),
    )
    .expect("public error serializes");
    assert!(!public_json.contains("C:\\"));
    assert!(!public_json.contains("/Users/"));
    assert!(!public_json.contains(&repository().to_string()));
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
    let snapshot = *fixture_catalog_snapshot(&entries).as_bytes();
    let plan_context =
        rootlight_agent::explain::RepoListPlanContext::new(2, false, [], ResponseProfile::Compact)
            .expect("test plan context is valid");
    let plan = rootlight_agent::explain::repo_list_plan(&plan_context);
    let base = repo_list_cursor_context(
        None,
        None,
        2,
        snapshot,
        &plan,
        ExposureProfile::Developer,
        signing_key.key_id,
    );
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
    wrong_tool_major.tool_major_version = 1;
    let mut wrong_tool = base.clone();
    wrong_tool.tool = McpTool::RepoStatus;
    let mut wrong_repository = base.clone();
    wrong_repository.repository = repository();
    let mut wrong_generation = base.clone();
    wrong_generation.generation = generation();
    let mut wrong_request = base.clone();
    wrong_request.query_fingerprint = repo_list_fingerprint(Some("other"), None, 2);
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
                repo_list_cursor_context(
                    None,
                    None,
                    2,
                    snapshot,
                    &plan,
                    ExposureProfile::Developer,
                    signing_key.key_id,
                ),
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
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "{label} must fail before a daemon catalog lookup"
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: Some(parent_generation()),
        active_parent_generation: Some(parent_generation()),
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
        ..repository_status_response()
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::RepoStatus,
        json!({
            "repository": {"repository_id": repository()},
            "coverage_detail": "language"
        }),
    )
    .await
    .expect("repo status maps");
    let serialized =
        serde_json::to_vec(&Value::Object(output.clone())).expect("repo status serializes");
    assert_eq!(
        output["usage"]["json_bytes"],
        u64::try_from(serialized.len()).expect("test response size fits u64")
    );
    assert_eq!(
        output["usage"]["estimated_tokens"],
        rootlight_mcp_contract::accounting::estimate_tokens(serialized.len())
    );
    assert!(
        output["usage"]["wall_time_ms"]
            .as_u64()
            .is_some_and(|elapsed| elapsed >= 1)
    );
    let output: RepoStatusOutput = decode(output);
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
    assert_eq!(output.repository.display_name, "fixture");
    assert_eq!(output.data.resolved_generation, generation());
    assert_eq!(output.data.requested_generation.0, None);
    assert_eq!(
        output.data.publication_state,
        GenerationPublicationState::Published
    );
    assert_eq!(output.usage.rows, 2);
    let ObservedCall::RepositoryStatus(request) = harness.only_call() else {
        panic!("expected repository status call");
    };
    assert_eq!(
        request.coverage_detail(),
        client::RepositoryStatusCoverageDetail::Language
    );
}

#[tokio::test]
async fn repo_status_projects_bounded_operations_and_freshness_controls() {
    let mut status = repository_status_response();
    status.operations.push(RepositoryStatusOperation {
        operation: operation(),
        kind: client::OperationKind::RepositoryIndex,
        state: client::OperationState::Running,
        completed_units: 2,
        total_units: 4,
        owned_by_client: true,
        started_unix_ms: 1,
    });
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(status)));
    let output: RepoStatusOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoStatus,
            json!({
                "repository": {"repository_id": repository()},
                "include_operations": true,
                "require_freshness": "structural"
            }),
        )
        .await
        .expect("repo status maps requested operations"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected repository status success");
    };

    assert_eq!(output.data.operations.len(), 1);
    assert_eq!(output.data.operations[0].operation_id, operation());
    assert_eq!(output.data.operations[0].state, OperationState::Running);
    assert_eq!(output.data.operations[0].progress_permille, 500);
    assert!(output.data.operations[0].owned_by_session);
    assert_eq!(output.usage.rows, 3);
    assert_eq!(
        output.data.recommended_actions[0].as_str(),
        "inspect operation"
    );
    let ObservedCall::RepositoryStatus(request) = harness.only_call() else {
        panic!("expected repository status call");
    };
    assert!(request.include_operations());
    assert_eq!(
        request.freshness_requirement(),
        client::RepositoryStatusFreshnessRequirement::Structural
    );
}

#[tokio::test]
async fn repo_status_preserves_exact_generation_when_active_changes() {
    let mut status = repository_status_response();
    status.resolved_generation = alternate_generation();
    status.parent_generation = None;
    status.structural_freshness = "superseded".to_owned();
    status.semantic_freshness = "superseded".to_owned();
    status.publication_state = "retained".to_owned();
    status.active_semantic_freshness = "stale".to_owned();
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(status)));
    let output: RepoStatusOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoStatus,
            json!({
                "repository": {"repository_id": repository()},
                "generation": alternate_generation()
            }),
        )
        .await
        .expect("retained exact generation reports status"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected exact repo status success");
    };

    assert_eq!(output.generation.generation_id, alternate_generation());
    assert_eq!(
        output.generation.structural_freshness,
        Freshness::Superseded
    );
    assert_eq!(
        output
            .data
            .active_generation
            .0
            .expect("active generation remains visible")
            .semantic_freshness,
        Freshness::Stale
    );
    assert_eq!(
        output.data.publication_state,
        GenerationPublicationState::Retained
    );
    assert_eq!(
        output.data.requested_generation.0,
        Some(alternate_generation())
    );
    let ObservedCall::RepositoryStatus(request) = harness.only_call() else {
        panic!("expected repository status call");
    };
    assert_eq!(
        request.generation(),
        ClientGenerationSelector::Generation(alternate_generation())
    );
}

#[tokio::test]
async fn repo_status_propagates_actionable_missing_generation() {
    let missing = alternate_generation();
    let public = PublicError::builder(ErrorCode::StaleGeneration, "generation is not retained")
        .repository(repository())
        .generation(missing)
        .next_action(NextAction::RestartEnumeration)
        .build()
        .expect("missing-generation fixture is valid");
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Err(ClientPortError::Public(
        Box::new(public),
    ))));
    let error = execute(
        &harness.executor,
        VerticalTool::RepoStatus,
        json!({
            "repository": {"repository_id": repository()},
            "generation": missing
        }),
    )
    .await
    .expect_err("missing exact generation is a checked error");
    let public = error
        .public_error()
        .expect("missing generation remains a public error");

    assert_eq!(public.code(), ErrorCode::StaleGeneration);
    assert_eq!(public.generation(), Some(missing));
    assert_eq!(public.next_actions(), &[NextAction::RestartEnumeration]);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn repo_status_lifecycle_states_and_errors_match_the_versioned_golden() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/mcp/repository-lifecycle/status-goldens-v1.json"
    ))
    .expect("repository status lifecycle golden is valid JSON");
    assert_eq!(
        fixture["schema"],
        "rootlight.repository-status-lifecycle-goldens/1"
    );

    for case in fixture["success_cases"]
        .as_array()
        .expect("success cases are an array")
    {
        let mut status = repository_status_response();
        status.state = case["state"]
            .as_str()
            .expect("success state is a string")
            .to_owned();
        status.structural_freshness = case["structural_freshness"]
            .as_str()
            .expect("structural freshness is a string")
            .to_owned();
        status.semantic_freshness = case["semantic_freshness"]
            .as_str()
            .expect("semantic freshness is a string")
            .to_owned();
        if let Some(operation_state) = case["operation_state"].as_str() {
            let state = match operation_state {
                "running" => client::OperationState::Running,
                _ => panic!("unsupported operation state in lifecycle golden"),
            };
            status.operations.push(RepositoryStatusOperation {
                operation: operation(),
                kind: client::OperationKind::RepositoryIndex,
                state,
                completed_units: 1,
                total_units: 2,
                owned_by_client: true,
                started_unix_ms: 1,
            });
        }
        let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(status)));
        let output: RepoStatusOutput = decode(
            execute(
                &harness.executor,
                VerticalTool::RepoStatus,
                json!({
                    "repository": {"repository_id": repository()},
                    "include_operations": true
                }),
            )
            .await
            .expect("golden lifecycle state maps"),
        );
        let ToolResponse::Success(output) = output else {
            panic!("expected repository status success");
        };
        let observed = json!({
            "repository_state": output.data.repository_state,
            "warnings": output.warnings,
            "recommended_actions": output.data.recommended_actions,
        });
        assert_eq!(
            observed,
            case["expected"],
            "lifecycle success golden differs for {}",
            case["name"].as_str().expect("case name is a string")
        );
    }

    for case in fixture["error_cases"]
        .as_array()
        .expect("error cases are an array")
    {
        let (code, message, action) =
            match case["error_kind"].as_str().expect("error kind is a string") {
                "missing" => (
                    ErrorCode::StaleGeneration,
                    "generation is not retained",
                    NextAction::RestartEnumeration,
                ),
                "corrupt" => (
                    ErrorCode::IndexCorrupt,
                    "repository index is corrupt",
                    NextAction::RebuildRepository,
                ),
                "incompatible" => (
                    ErrorCode::MigrationRequired,
                    "stored data requires migration",
                    NextAction::RebuildRepository,
                ),
                _ => panic!("unsupported error kind in lifecycle golden"),
            };
        let public = PublicError::builder(code, message)
            .repository(repository())
            .generation(generation())
            .next_action(action)
            .build()
            .expect("lifecycle error fixture is valid");
        let harness = Harness::new(FakeOutcome::RepositoryStatus(Err(ClientPortError::Public(
            Box::new(public),
        ))));
        let error = execute(
            &harness.executor,
            VerticalTool::RepoStatus,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation()
            }),
        )
        .await
        .expect_err("exceptional exact generation remains a checked error");
        let public = error
            .public_error()
            .expect("exceptional lifecycle failure remains public");
        let observed = json!({
            "code": public.code(),
            "message": public.message(),
            "retryable": public.retryable(),
            "repository_bound": public.repository() == Some(repository()),
            "generation_bound": public.generation() == Some(generation()),
            "next_actions": public.next_actions(),
        });
        assert_eq!(
            observed,
            case["expected"],
            "lifecycle error golden differs for {}",
            case["name"].as_str().expect("case name is a string")
        );
    }
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
            next_page_offset: None,
            execution_completeness: complete_execution(),
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
async fn symbol_relationships_rejects_unserved_relation_before_the_port() {
    let harness = Harness::new(FakeOutcome::SymbolRelationships(Err(
        ClientPortError::Executor,
    )));
    let error = execute(
        &harness.executor,
        VerticalTool::SymbolRelationships,
        json!({
            "repository": {"repository_id": repository()},
            "symbol_ids": [symbol()],
            "relations": ["data_flow"]
        }),
    )
    .await
    .expect_err("unserved relation is rejected before the port");
    let public = error
        .public_error()
        .expect("unserved relation is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
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
            execution_completeness: complete_execution(),
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
async fn flow_trace_rejects_noncanonical_called_by_relation_before_the_port() {
    let harness = Harness::new(FakeOutcome::FlowTrace(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::FlowTrace,
        json!({
            "repository": {"repository_id": repository()},
            "from": {"symbol_id": symbol()},
            "relations": ["called_by"]
        }),
    )
    .await
    .expect_err("noncanonical relation is rejected before the port");
    let public = error
        .public_error()
        .expect("noncanonical relation is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
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
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Results,
                client::ContinuationGuidance::ReduceRelations,
            ),
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
    assert_public_truncation(&output, ContractLimitingResourceKind::Results);
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
    let warning_codes: Vec<_> = output
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert!(warning_codes.contains(&"static_projection_only"));
    assert!(warning_codes.contains(&"dynamic_edges_unobserved"));
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
async fn architecture_cycles_rejects_unserved_relation_before_the_port() {
    let harness = Harness::new(FakeOutcome::ArchitectureCycles(Err(
        ClientPortError::Executor,
    )));
    let error = execute(
        &harness.executor,
        VerticalTool::ArchitectureCycles,
        json!({
            "repository": {"repository_id": repository()},
            "projection": {"relations": ["messaging"], "level": "symbol"}
        }),
    )
    .await
    .expect_err("unserved relation is rejected before the port");
    let public = error
        .public_error()
        .expect("unserved relation is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn code_dead_maps_candidates_entry_points_and_blind_spots() {
    let response = CodeDeadPortResponse::new(
        ClientCodeDead {
            context: context(1, 0),
            candidates: vec![ClientDeadCandidate {
                symbol_id: missing_symbol(),
                classification: "no_observed_incoming_references".to_owned(),
                confidence: 1_000,
                why: vec![
                    "no_incoming_references".to_owned(),
                    "not_observed_from_partial_entry_points".to_owned(),
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
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Rows,
                client::ContinuationGuidance::RefreshCoverage,
            ),
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
    assert_public_truncation(&output, ContractLimitingResourceKind::Rows);
    assert_eq!(output.data.candidates.len(), 1);
    let candidate = &output.data.candidates[0];
    assert_eq!(candidate.symbol_id, missing_symbol());
    assert_eq!(
        candidate.classification,
        DeadClassification::NoObservedIncomingReferences
    );
    assert_eq!(candidate.confidence, 1_000);
    assert_eq!(candidate.why, vec!["no_incoming_references".to_owned()]);
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
async fn code_dead_rejects_unserved_entry_point_policies_before_the_port() {
    for policy in ["library", "application"] {
        let harness = Harness::new(FakeOutcome::CodeDead(Err(ClientPortError::Executor)));
        let error = execute(
            &harness.executor,
            VerticalTool::CodeDead,
            json!({
                "repository": {"repository_id": repository()},
                "entry_point_policy": policy
            }),
        )
        .await
        .expect_err("unserved entry-point policy is rejected before the port");
        let public = error
            .public_error()
            .expect("unserved policy is a checked public error");
        assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
    }
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
            communities: vec![ClientArchitectureCommunity {
                id: "community:file-a".to_owned(),
                members: vec!["file-a".to_owned(), "file-b".to_owned()],
                internal_connection_weight: 2,
                ownership_truth: false,
            }],
            views: vec![
                ClientDerivedView {
                    view: "hotspots".to_owned(),
                    algorithm_version: "fan_in_out_v1".to_owned(),
                    parameters: std::collections::BTreeMap::from([(
                        "score_range".to_owned(),
                        "0..1000".to_owned(),
                    )]),
                },
                ClientDerivedView {
                    view: "communities".to_owned(),
                    algorithm_version: "weighted_label_propagation_v1".to_owned(),
                    parameters: std::collections::BTreeMap::from([
                        ("ownership_truth".to_owned(), "not_claimed".to_owned()),
                        ("seed".to_owned(), "524f4f544c494748".to_owned()),
                    ]),
                },
            ],
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Results,
                client::ContinuationGuidance::NarrowScope,
            ),
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
                "views": ["communities", "hotspots"]
            }),
        )
        .await
        .expect("architecture overview maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected architecture overview success");
    };
    assert_public_truncation(&output, ContractLimitingResourceKind::Results);
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
    assert_eq!(output.data.communities.len(), 1);
    let community = &output.data.communities[0];
    assert_eq!(community.id, "community:file-a");
    assert_eq!(community.members, ["file-a", "file-b"]);
    assert_eq!(community.internal_connection_weight, 2);
    assert!(!community.ownership_truth);
    assert_eq!(output.data.views.len(), 2);
    assert_eq!(output.data.views[0].view, ArchitectureView::Hotspots);
    assert_eq!(output.data.views[0].algorithm_version, "fan_in_out_v1");
    assert_eq!(
        output.data.views[0].parameters.get("score_range"),
        Some(&"0..1000".to_owned())
    );
    assert_eq!(output.data.views[1].view, ArchitectureView::Communities);
    assert_eq!(
        output.data.views[1].parameters.get("ownership_truth"),
        Some(&"not_claimed".to_owned())
    );
    let ObservedCall::ArchitectureOverview(request) = harness.only_call() else {
        panic!("expected architecture overview call");
    };
    assert_eq!(request.repository(), repository());
    assert_eq!(
        request.views(),
        &["communities".to_owned(), "hotspots".to_owned()]
    );
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
                file_colocation_signals: true,
            },
            gaps: vec![ClientTestGap {
                scope: "scope-1".to_owned(),
                reason: "no_related_test".to_owned(),
            }],
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Results,
                client::ContinuationGuidance::SplitRequest,
            ),
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
    assert_public_truncation(&output, ContractLimitingResourceKind::Results);
    assert_eq!(output.data.tests.len(), 1);
    let test = &output.data.tests[0];
    assert_eq!(test.test_id, "test-1");
    assert_eq!(test.kind, TestKind::Unit);
    assert_eq!(test.path, None);
    assert_eq!(test.score, 970);
    assert_eq!(test.why, vec!["direct_test_edge".to_owned()]);
    assert_eq!(test.estimated_cost_ms, None);
    assert_eq!(test.command_hint, None);
    assert!(output.data.coverage_strategy.direct_edges);
    assert!(!output.data.coverage_strategy.transitive_signals);
    assert!(!output.data.coverage_strategy.history_signals);
    assert!(output.data.coverage_strategy.file_colocation_signals);
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
async fn tests_select_rejects_unobserved_test_kinds() {
    let harness = Harness::new(FakeOutcome::TestsSelect(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::TestsSelect,
        json!({
            "repository": {"repository_id": repository()},
            "seeds": {"symbols": [symbol()]},
            "test_kinds": ["integration"]
        }),
    )
    .await
    .expect_err("unobserved test kinds are rejected before the port");
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
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Depth,
                client::ContinuationGuidance::ReduceDepth,
            ),
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
    assert_public_truncation(&output, ContractLimitingResourceKind::Depth);
    assert_eq!(output.data.resolved_changes.len(), 1);
    let change = &output.data.resolved_changes[0];
    assert_eq!(change.symbol_id, RequiredNullable(Some(symbol())));
    assert_eq!(change.file_id, RequiredNullable(Some(file())));
    assert_eq!(change.classification, ChangeClassification::Body);
    assert_eq!(change.kind, None);
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
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Results,
                client::ContinuationGuidance::NarrowScope,
            ),
        },
        metadata("plan-change-1"),
    );
    let harness = Harness::new(FakeOutcome::PlanChange(Ok(response)));
    let execution = execute(
        &harness.executor,
        VerticalTool::PlanChange,
        json!({
            "repository": {"repository_id": repository()},
            "objective": "bug_fix",
            "objective_text": "fix the defect",
            "targets": [{"symbol_id": symbol()}]
        }),
    )
    .await;
    let output: PlanChangeOutput = decode(execution.unwrap_or_else(|error| {
        panic!(
            "plan change maps: {error:?}; calls: {:?}",
            harness
                .calls
                .lock()
                .expect("fake call recorder is not poisoned")
        )
    }));
    let ToolResponse::Success(output) = output else {
        panic!("expected plan change success");
    };
    assert!(output.truncated);
    assert_eq!(
        output.completeness.state,
        CompletenessState::UnsupportedPartial
    );
    assert!(
        output
            .completeness
            .limiting_resources
            .iter()
            .any(|resource| resource.kind == ContractLimitingResourceKind::Results)
    );
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
    assert_eq!(output.data.provider_coverage.len(), 7);
    assert_eq!(
        output
            .data
            .provider_coverage
            .iter()
            .map(|coverage| coverage.provider)
            .collect::<Vec<_>>(),
        vec![
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceProvider::Relationships,
            PlanEvidenceProvider::Tests,
            PlanEvidenceProvider::Architecture,
            PlanEvidenceProvider::History,
            PlanEvidenceProvider::Source,
            PlanEvidenceProvider::Ownership,
        ]
    );
    for (provider, reason) in [
        (
            PlanEvidenceProvider::History,
            PlanEvidenceOmissionReason::HistoryBaselineUnavailable,
        ),
        (
            PlanEvidenceProvider::Source,
            PlanEvidenceOmissionReason::SourceReferencesUnavailable,
        ),
        (
            PlanEvidenceProvider::Ownership,
            PlanEvidenceOmissionReason::OwnershipProviderUnsupported,
        ),
    ] {
        let coverage = output
            .data
            .provider_coverage
            .iter()
            .find(|coverage| coverage.provider == provider)
            .expect("required provider coverage is present");
        assert_eq!(coverage.state, PlanProviderState::Unsupported);
        assert_eq!(
            coverage.omission.as_ref().map(|omission| omission.reason),
            Some(reason)
        );
    }
    assert_eq!(output.data.plan[0].evidence_refs.len(), 1);
    assert!(!output.data.plan[0].rationale.is_empty());
    let serialized = serde_json::to_vec(&ToolResponse::Success(output.clone()))
        .expect("checked plan response serializes");
    assert_eq!(
        output.usage.json_bytes,
        u64::try_from(serialized.len()).expect("test response length fits u64")
    );
    assert_eq!(
        output.usage.estimated_tokens,
        rootlight_mcp_contract::accounting::estimate_tokens(serialized.len())
    );
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    assert!(matches!(calls[0], ObservedCall::RepositoryStatus(_)));
    assert!(matches!(calls[1], ObservedCall::ChangeImpact(_)));
    assert!(matches!(calls[2], ObservedCall::SymbolRelationships(_)));
    assert!(matches!(calls[3], ObservedCall::TestsSelect(_)));
    assert!(matches!(calls[4], ObservedCall::ArchitectureOverview(_)));
    let ObservedCall::PlanChange(request) = &calls[5] else {
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
            execution_completeness: truncated_execution(
                client::LimitingResourceKind::Results,
                client::ContinuationGuidance::NarrowScope,
            ),
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
    assert_public_truncation(&output, ContractLimitingResourceKind::Results);
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
async fn history_compare_rejects_unobserved_change_kinds() {
    let harness = Harness::new(FakeOutcome::HistoryCompare(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::HistoryCompare,
        json!({
            "repository": {"repository_id": repository()},
            "base": parent_generation(),
            "head": generation(),
            "change_kinds": ["relations"]
        }),
    )
    .await
    .expect_err("unobserved change kinds are rejected before the port");
    let public = error
        .public_error()
        .expect("unsupported option is a checked public error");
    assert_eq!(public.code(), ErrorCode::UnsupportedCapability);
    assert_eq!(public.message(), UNSUPPORTED_MESSAGE);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn history_compare_rejects_mismatched_matched_states() {
    let response = HistoryComparePortResponse::new(
        ClientHistoryCompare {
            context: context(1, 0),
            matched_states: ClientHistoryMatchedStates {
                base_generation: generation(),
                head_generation: generation(),
                coverage: "bounded".to_owned(),
            },
            changes: Vec::new(),
            architecture_delta: ClientHistoryArchitectureDelta {
                new_cross_service_edges: 0,
                removed_cross_service_edges: 0,
                new_boundaries: 0,
                removed_boundaries: 0,
            },
            breaking_candidates: Vec::new(),
            lineage: Vec::new(),
            execution_completeness: complete_execution(),
        },
        metadata("history-compare-mismatch"),
    );
    let harness = Harness::new(FakeOutcome::HistoryCompare(Ok(response)));
    let error = execute(
        &harness.executor,
        VerticalTool::HistoryCompare,
        json!({
            "repository": {"repository_id": repository()},
            "base": parent_generation(),
            "head": generation()
        }),
    )
    .await
    .expect_err("mismatched matched states are rejected");
    assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
    assert!(error.public_error().is_none());
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
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
            next_page_offset: None,
            execution_completeness: complete_execution(),
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

fn advanced_plan_response(
    resolved_generation: GenerationId,
    active_generation: bool,
) -> QueryAdvancedPortResponse {
    let mut query_context = context(0, 0);
    query_context.generation = resolved_generation;
    query_context.active_generation = active_generation;
    QueryAdvancedPortResponse::new(
        ClientAdvancedQuery {
            context: query_context,
            columns: vec![ClientAdvancedColumn {
                name: "id".to_owned(),
                column_type: "symbol_id".to_owned(),
            }],
            rows: Vec::new(),
            plan: Some(ClientAdvancedPlan {
                estimated_cost: 222,
                operators: vec!["Scan".to_owned(), "Filter".to_owned(), "Limit".to_owned()],
                applied_limits: vec![
                    "max_results: 20".to_owned(),
                    "max_traversal: 100000".to_owned(),
                ],
            }),
            completeness: "complete".to_owned(),
            next_page_offset: None,
            execution_completeness: complete_execution(),
        },
        metadata("query-advanced-plan"),
    )
}

async fn advanced_plan_fingerprint(
    harness: &Harness,
    generation_selector: Option<GenerationId>,
) -> String {
    let mut arguments = json!({
        "repository": {"repository_id": repository()},
        "query": {"op": "scan", "entity": "function"},
        "explain": true
    });
    if let Some(generation_selector) = generation_selector {
        arguments["generation"] = json!(generation_selector);
    }
    let output: QueryAdvancedOutput = decode(
        execute(&harness.executor, VerticalTool::QueryAdvanced, arguments)
            .await
            .expect("advanced explain maps"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected query advanced success");
    };
    output
        .data
        .plan
        .0
        .expect("explain response includes a plan")
        .fingerprint
}

#[tokio::test]
async fn query_advanced_plan_is_deterministic_for_the_same_resolved_generation() {
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced_plan_response(
        generation(),
        true,
    ))));

    let first = advanced_plan_fingerprint(&harness, None).await;
    let second = advanced_plan_fingerprint(&harness, None).await;

    assert_eq!(first, second);
}

#[tokio::test]
async fn query_advanced_plan_binds_the_resolved_active_generation() {
    let first = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced_plan_response(
        generation(),
        true,
    ))));
    let second = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced_plan_response(
        alternate_generation(),
        true,
    ))));

    let first_fingerprint = advanced_plan_fingerprint(&first, None).await;
    let second_fingerprint = advanced_plan_fingerprint(&second, None).await;

    assert_ne!(first_fingerprint, second_fingerprint);
}

#[tokio::test]
async fn query_advanced_explicit_generation_uses_the_canonical_resolved_identity() {
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced_plan_response(
        generation(),
        false,
    ))));

    let fingerprint = advanced_plan_fingerprint(&harness, Some(generation())).await;
    let expected = rootlight_agent::explain::finalize_plan(
        PlanExplanation::new(
            222,
            vec!["Scan".to_owned(), "Filter".to_owned(), "Limit".to_owned()],
            vec![
                "max_results: 20".to_owned(),
                "max_traversal: 100000".to_owned(),
            ],
        ),
        &generation().to_string(),
    );

    assert_eq!(fingerprint, expected.fingerprint);
    let ObservedCall::QueryAdvanced(request) = harness.only_call() else {
        panic!("expected query advanced call");
    };
    assert_eq!(
        request.generation(),
        client::GenerationSelector::Generation(generation())
    );
}

#[tokio::test]
async fn query_advanced_generation_mismatch_fails_closed_before_plan_mapping() {
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(advanced_plan_response(
        alternate_generation(),
        false,
    ))));

    let error = execute(
        &harness.executor,
        VerticalTool::QueryAdvanced,
        json!({
            "repository": {"repository_id": repository()},
            "generation": generation(),
            "query": {"op": "scan", "entity": "function"},
            "explain": true
        }),
    )
    .await
    .expect_err("a mismatched daemon generation is rejected");

    assert_eq!(error.failure(), Some(ToolExecutionFailure::InvalidResponse));
    assert!(error.public_error().is_none());
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn query_advanced_rejects_a_malformed_paging_cursor() {
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
    .expect_err("malformed paging cursor is rejected before the port");
    let public = error
        .public_error()
        .expect("cursor failure is a checked public error");
    assert_eq!(public.code(), ErrorCode::InvalidCursor);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn query_advanced_corpus_is_rejected_before_daemon_publication() {
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/adversarial/query-advanced/v1/corpus.json"
    ))
    .expect("advanced adversarial corpus is valid");
    for case in corpus["cases"]
        .as_array()
        .expect("advanced adversarial cases are an array")
    {
        let id = case["id"].as_str().expect("corpus case has an id");
        let boundary = case["boundary"]
            .as_str()
            .expect("corpus case has a boundary");
        let payload = case["payload"].as_str().expect("corpus case has a payload");
        let arguments = match boundary {
            "ast_deserialize" => {
                let Ok(query) = serde_json::from_str::<Value>(payload) else {
                    assert_eq!(id, "malformed-json");
                    continue;
                };
                json!({
                    "repository": {"repository_id": repository()},
                    "query": query
                })
            }
            "ast_plan" => json!({
                "repository": {"repository_id": repository()},
                "query": serde_json::from_str::<Value>(payload)
                    .expect("planner corpus case is valid JSON")
            }),
            "cursor_decode" => json!({
                "repository": {"repository_id": repository()},
                "query": {"op": "scan", "entity": "function"},
                "cursor": payload
            }),
            other => panic!("unexpected advanced corpus boundary {other}"),
        };
        let harness = Harness::new(FakeOutcome::QueryAdvanced(Err(ClientPortError::Executor)));
        let error = match execute(&harness.executor, VerticalTool::QueryAdvanced, arguments).await {
            Ok(_) => panic!("{id} must fail before the daemon"),
            Err(error) => error,
        };
        let public = error
            .public_error()
            .unwrap_or_else(|| panic!("{id} must produce a checked public error"));
        if boundary == "cursor_decode" {
            assert_eq!(public.code(), ErrorCode::InvalidCursor, "{id}");
        } else {
            assert_ne!(public.code(), ErrorCode::Internal, "{id}");
        }
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "{id} reached the daemon"
        );
    }
}

#[tokio::test]
async fn query_advanced_cursor_binds_parameters_and_effective_limits() {
    const KEY_MATERIAL: [u8; 32] = [0x4A; 32];
    let base = json!({
        "repository": {"repository_id": repository()},
        "query": {
            "op": "scan",
            "entity": "function",
            "filter": {
                "pred": "equals",
                "field": "name",
                "value": {"parameter": {"name": "needle"}}
            }
        },
        "parameters": {"needle": {"text": "alpha"}},
        "max_results": 2,
        "max_depth": 3,
        "cost_limit": 10_000
    });
    let cursor = issue_pagination_cursor(
        VerticalTool::QueryAdvanced,
        base.clone(),
        ExposureProfile::Developer,
        CursorSigningKey::deterministic(KEY_MATERIAL).expect("test signing key is valid"),
        now_unix_ms(),
        false,
    );
    let cases = [
        (
            "typed parameter",
            with_argument(
                base.clone(),
                "parameters",
                json!({"needle": {"text": "beta"}}),
            ),
        ),
        (
            "maximum depth",
            with_argument(base.clone(), "max_depth", json!(4)),
        ),
        (
            "cost limit",
            with_argument(base, "cost_limit", json!(10_001)),
        ),
    ];

    for (dimension, arguments) in cases {
        let harness = Harness::with_cursor_key(
            FakeOutcome::QueryAdvanced(Err(ClientPortError::Executor)),
            KEY_MATERIAL,
        );
        let error = execute(
            &harness.executor,
            VerticalTool::QueryAdvanced,
            with_argument(arguments, "cursor", json!(cursor.clone())),
        )
        .await
        .expect_err("changed advanced cursor context is rejected");
        assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::InvalidCursor),
            "{dimension} was not cursor-bound"
        );
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            0,
            "{dimension} reached the daemon"
        );
    }
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
    assert_eq!(
        public
            .details()
            .get(&DetailKey::parse("cost_limit").expect("static key is valid")),
        Some(&PublicValue::Unsigned(1))
    );
    assert_eq!(
        public.next_actions(),
        &[NextAction::CorrectField {
            field: DetailKey::parse("cost_limit").expect("static key is valid"),
        }]
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
                "symbol_ids": [symbol(), missing_symbol()]
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
                start_line: Some(1),
                end_line: Some(3),
                content: b"0123456789abcde".to_vec(),
                encoding: client::SourceEncoding::Utf8,
                content_hash: content_hash(),
                language: "rust".to_owned(),
                generated: false,
            }],
            total_source_bytes: 15,
            truncated: false,
            execution_completeness: complete_execution(),
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
                "context_lines_before": 1,
                "context_lines_after": 1,
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
    assert_eq!(request.context_lines_before(), 1);
    assert_eq!(request.context_lines_after(), 1);
    assert!(!request.merge_overlaps());
    assert!(request.include_line_numbers());
    assert_eq!(
        request.encoding(),
        SourceEncodingRequest::Utf8LosslessWhenValid
    );
}

#[tokio::test]
async fn maps_exact_binary_source_as_canonical_base64_without_line_metadata() {
    let requested = source_reference(4, 6, 2, 2);
    let response = SourceReadPortResponse::new(
        client::SourceRead {
            context: context(1, 2),
            chunks: vec![ClientSourceChunk {
                source: requested,
                path: "assets/raw.bin".to_owned(),
                start_byte: 4,
                end_byte: 6,
                start_line: None,
                end_line: None,
                content: vec![0xff, 0xfe],
                encoding: client::SourceEncoding::Bytes,
                content_hash: content_hash(),
                language: "binary".to_owned(),
                generated: false,
            }],
            total_source_bytes: 2,
            truncated: false,
            execution_completeness: complete_execution(),
        },
        metadata("trace-source-bytes"),
        Vec::new(),
        Vec::new(),
    );
    let harness = Harness::new(FakeOutcome::SourceRead(Ok(response)));
    let output: SourceReadOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "references": [{"source_ref": wire_source_reference(4, 6, 2, 2)}],
                "encoding": "bytes_base64"
            }),
        )
        .await
        .expect("binary source maps"),
    );

    let ToolResponse::Success(output) = output else {
        panic!("expected source read success");
    };
    assert_eq!(output.data.chunks[0].content, "//4=");
    assert_eq!(output.data.chunks[0].encoding, SourceEncoding::Base64);
    assert_eq!(output.data.chunks[0].start_line, None);
    assert_eq!(output.data.chunks[0].end_line, None);
    assert!(output.data.chunks[0].source_ref.line_hint().is_none());
    let ObservedCall::SourceRead(request) = harness.only_call() else {
        panic!("expected source read request");
    };
    assert!(!request.include_line_numbers());
}

#[tokio::test]
async fn retrieval_mappers_reject_untrusted_identity_and_path_drift() {
    let mut locate = locate_response();
    locate.result.hits[0].path = "../outside.rs".to_owned();
    let locate_harness = Harness::new(FakeOutcome::CodeLocate(Ok(locate)));
    let locate_error = execute(
        &locate_harness.executor,
        VerticalTool::CodeLocate,
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish"
        }),
    )
    .await
    .expect_err("root-escaping locate paths fail closed");
    assert_eq!(
        locate_error.failure(),
        Some(ToolExecutionFailure::InvalidResponse)
    );

    let mut duplicate_locate = locate_response();
    duplicate_locate
        .result
        .hits
        .push(duplicate_locate.result.hits[0].clone());
    duplicate_locate.result.matched_candidates = 2;
    let duplicate_harness = Harness::new(FakeOutcome::CodeLocate(Ok(duplicate_locate)));
    let duplicate_error = execute(
        &duplicate_harness.executor,
        VerticalTool::CodeLocate,
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish",
            "max_results": 2
        }),
    )
    .await
    .expect_err("duplicate locate identities fail closed");
    assert_eq!(
        duplicate_error.failure(),
        Some(ToolExecutionFailure::InvalidResponse)
    );

    let mut explain = explain_response(source_reference(4, 12, 2, 2));
    explain.result.unresolved_symbols.clear();
    let explain_harness = Harness::new(FakeOutcome::SymbolExplain(Ok(explain)));
    let explain_error = execute(
        &explain_harness.executor,
        VerticalTool::SymbolExplain,
        json!({
            "repository": {"repository_id": repository()},
            "symbol_ids": [symbol(), missing_symbol()]
        }),
    )
    .await
    .expect_err("incomplete resolved and unresolved identity partitions fail closed");
    assert_eq!(
        explain_error.failure(),
        Some(ToolExecutionFailure::InvalidResponse)
    );

    let foreign_definition = client::SourceReference::new(
        alternate_repository(),
        generation(),
        file(),
        4..12,
        content_hash(),
        Some(2..=2),
    )
    .expect("foreign definition fixture is structurally valid");
    let foreign_explain = explain_response(foreign_definition);
    let foreign_harness = Harness::new(FakeOutcome::SymbolExplain(Ok(foreign_explain)));
    let foreign_error = execute(
        &foreign_harness.executor,
        VerticalTool::SymbolExplain,
        json!({
            "repository": {"repository_id": repository()},
            "symbol_ids": [symbol(), missing_symbol()]
        }),
    )
    .await
    .expect_err("foreign definition identities fail closed");
    assert_eq!(
        foreign_error.failure(),
        Some(ToolExecutionFailure::InvalidResponse)
    );

    let source = source_reference(4, 12, 2, 2);
    let mut source_response = source_read_response(source);
    source_response.result.total_source_bytes = 7;
    let source_harness = Harness::new(FakeOutcome::SourceRead(Ok(source_response)));
    let source_error = execute(
        &source_harness.executor,
        VerticalTool::SourceRead,
        json!({
            "repository": {"repository_id": repository()},
            "references": [{"source_ref": wire_source_reference(4, 12, 2, 2)}]
        }),
    )
    .await
    .expect_err("source byte accounting drift fails closed");
    assert_eq!(
        source_error.failure(),
        Some(ToolExecutionFailure::InvalidResponse)
    );
}

#[tokio::test]
async fn source_read_rejects_line_context_for_raw_bytes_before_the_port() {
    let harness = Harness::new(FakeOutcome::SourceRead(Err(ClientPortError::Executor)));
    let error = execute(
        &harness.executor,
        VerticalTool::SourceRead,
        json!({
            "repository": {"repository_id": repository()},
            "references": [{"source_ref": wire_source_reference(5, 10, 2, 2)}],
            "context_lines_before": 1,
            "encoding": "bytes_base64"
        }),
    )
    .await
    .expect_err("raw bytes cannot request UTF-8 line expansion");
    assert_eq!(
        error.public_error().map(PublicError::code),
        Some(ErrorCode::UnsupportedCapability)
    );
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn repository_index_rejects_explicit_scope_before_the_port() {
    let harness = Harness::new(FakeOutcome::RepositoryIndex(Err(ClientPortError::Executor)));
    let call_count = Arc::clone(&harness.call_count);
    let router = ToolRouter::new(
        harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    let response = router
        .handle(
            operating_request(json!({
                "name": "repo.index",
                "arguments": {
                    "root": "C:/fixture",
                    "scope": {"repository": "whole"}
                }
            })),
            cancellation(),
        )
        .await;
    let HandlerResponse::Success(result) = response else {
        panic!("capability rejection is an MCP tool result");
    };
    assert_eq!(result["isError"], true);
    assert_eq!(
        result["structuredContent"]["error"]["code"],
        "UNSUPPORTED_CAPABILITY"
    );
    assert_eq!(
        result["structuredContent"]["error"]["details"]["field_path"]["value"],
        "scope.repository"
    );
    assert_eq!(
        result["structuredContent"]["error"]["details"]["capability_reason"]["value"],
        "unsupported_field"
    );
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        0,
        "capability rejection must happen before indexing"
    );
}

#[tokio::test]
async fn source_read_rejects_non_reference_selectors_before_the_port() {
    let harness = Harness::new(FakeOutcome::SourceRead(Err(ClientPortError::Executor)));
    let call_count = Arc::clone(&harness.call_count);
    let router = ToolRouter::new(
        harness.executor,
        rootlight_mcp_contract::ExposureProfile::Developer,
    )
    .expect("router compiles");
    for (selector, field_path) in [
        (json!({"symbol_id": symbol()}), "references.0.symbol_id"),
        (
            json!({"file_id": file(), "start_byte": 0, "end_byte": 1}),
            "references.0.end_byte",
        ),
    ] {
        let response = router
            .handle(
                operating_request(json!({
                    "name": "source.read",
                    "arguments": {
                        "repository": {"repository_id": repository()},
                        "references": [selector]
                    }
                })),
                cancellation(),
            )
            .await;
        let HandlerResponse::Success(result) = response else {
            panic!("capability rejection is an MCP tool result");
        };
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "UNSUPPORTED_CAPABILITY"
        );
        assert_eq!(
            result["structuredContent"]["error"]["details"]["field_path"]["value"],
            field_path
        );
        assert_eq!(
            result["structuredContent"]["error"]["details"]["capability_reason"]["value"],
            "unsupported_field"
        );
    }
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        0,
        "capability rejection must happen before source retrieval"
    );
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
            VerticalTool::RepoIndex,
            json!({"repository_id": repository()}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "scope": {"paths": ["src"]}}),
        ),
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture", "scope": {"repository": "whole"}}),
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
            json!({"repository": {"repository_id": repository()}, "query": "x", "budget": {"evidence_level": "compact"}}),
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
            json!({"repository": {"repository_id": repository()}, "references": [{"source_ref": source}], "response_profile": "standard"}),
        ),
    ];

    for (tool, arguments) in cases {
        let label = format!("{}:{arguments}", tool.name());
        let error = match execute(&harness.executor, tool, arguments).await {
            Ok(_) => panic!("{label} unexpectedly reached execution"),
            Err(error) => error,
        };
        let public = error
            .public_error()
            .unwrap_or_else(|| panic!("{label} did not produce a checked public error"));
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
        resolved_generation: generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
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
    let snapshot = *fixture_catalog_snapshot(&entries).as_bytes();
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
    assert_eq!(
        explanation.operators,
        vec![
            "catalog_snapshot".to_owned(),
            "catalog_sort".to_owned(),
            "page_window".to_owned()
        ]
    );
    let plan_context =
        rootlight_agent::explain::RepoListPlanContext::new(10, false, [], ResponseProfile::Compact)
            .expect("test plan context is valid");
    let plan = rootlight_agent::explain::repo_list_plan(&plan_context);
    let full_fingerprint = repo_list_plan_fingerprint(&plan, &snapshot);
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
async fn repo_list_explain_empty_catalog_uses_one_bounded_catalog_call() {
    let harness = Harness::new(FakeOutcome::RepositoryList(Ok(RepositoryList {
        repositories: Vec::new(),
    })));
    let output: RepoListOutput = decode(
        execute(
            &harness.executor,
            VerticalTool::RepoList,
            json!({
                "query": "Straße",
                "states": [],
                "max_results": 200,
                "explain": true
            }),
        )
        .await
        .expect("empty catalog explain succeeds"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected explain success");
    };
    assert!(output.data.repositories.is_empty());
    assert_eq!(output.data.total_count, 0);
    assert!(!output.truncated);
    assert!(output.next_cursor.0.is_none());
    assert_eq!(output.usage.rows, 0);
    assert!(output.data.explanation.is_some());
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
    let ObservedCall::RepositoryList(request) = harness.only_call() else {
        panic!("explain performs only the catalog metadata call");
    };
    assert_eq!(request.page_size(), 200);
    assert_eq!(request.normalized_query(), Some("strasse"));
    assert_eq!(request.states(), Some(&[] as &[RepositoryCatalogState]));
}

#[tokio::test]
async fn query_batch_explain_returns_a_plan_without_retrieval() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(RepositoryStatus {
        repository_id: repository(),
        resolved_generation: alternate_generation(),
        active_generation: generation(),
        parent_generation: None,
        active_parent_generation: None,
        structural_freshness: "current".to_owned(),
        semantic_freshness: "current".to_owned(),
        state: "ready".to_owned(),
        coverage: vec![],
        ..repository_status_response()
    })));
    let output = execute(
        &harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": repository()},
            "generation": alternate_generation(),
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
    assert_eq!(output.data.generation_id, alternate_generation());
    assert_eq!(output.generation.generation_id, alternate_generation());
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

async fn batch_explain_fingerprint(harness: &Harness, arguments: Value) -> String {
    let output: QueryBatchOutput = decode(
        execute(&harness.executor, VerticalTool::QueryBatch, arguments)
            .await
            .expect("batch explain succeeds"),
    );
    let ToolResponse::Success(output) = output else {
        panic!("expected batch explain success");
    };
    output
        .data
        .explanation
        .expect("batch explain returns a physical plan")
        .fingerprint
}

#[tokio::test]
async fn query_batch_identity_fingerprint_binds_and_normalizes_equivalent_json() {
    let base = Harness::new(FakeOutcome::RepositoryStatus(Ok(
        repository_status_response(),
    )));
    let first = batch_explain_fingerprint(
        &base,
        json!({
            "repository": {"repository_id": repository()},
            "operations": [{
                "id": "find",
                "tool": "code.locate",
                "arguments": {"query": "publish", "max_results": 20}
            }],
            "explain": true
        }),
    )
    .await;
    let equivalent = batch_explain_fingerprint(
        &base,
        json!({
            "explain": true,
            "generation": "active",
            "failure_policy": "continue_independent",
            "response_profile": "compact",
            "budget": {"max_tokens": 3000, "timeout_ms": 30000},
            "operations": [{
                "arguments": {"max_results": 20, "query": "publish"},
                "tool": "code.locate",
                "id": "find"
            }],
            "repository": {"repository_id": repository()}
        }),
    )
    .await;

    let mut other_repository_status = repository_status_response();
    other_repository_status.repository_id = alternate_repository();
    let other_repository = Harness::new(FakeOutcome::RepositoryStatus(Ok(other_repository_status)));
    let other_repository_fingerprint = batch_explain_fingerprint(
        &other_repository,
        json!({
            "repository": {"repository_id": alternate_repository()},
            "operations": [{
                "id": "find",
                "tool": "code.locate",
                "arguments": {"query": "publish", "max_results": 20}
            }],
            "explain": true
        }),
    )
    .await;

    let mut other_generation_status = repository_status_response();
    other_generation_status.resolved_generation = alternate_generation();
    other_generation_status.active_generation = alternate_generation();
    let other_generation = Harness::new(FakeOutcome::RepositoryStatus(Ok(other_generation_status)));
    let other_generation_fingerprint = batch_explain_fingerprint(
        &other_generation,
        json!({
            "repository": {"repository_id": repository()},
            "generation": alternate_generation(),
            "operations": [{
                "id": "find",
                "tool": "code.locate",
                "arguments": {"query": "publish", "max_results": 20}
            }],
            "explain": true
        }),
    )
    .await;

    assert!(first.starts_with("plan1_"));
    assert_eq!(
        first, equivalent,
        "field ordering and an explicit active default are canonicalized"
    );
    assert_ne!(first, other_repository_fingerprint);
    assert_ne!(first, other_generation_fingerprint);
}

#[tokio::test]
async fn query_batch_fingerprint_binds_arguments_dependencies_and_typed_bindings() {
    let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(
        repository_status_response(),
    )));
    let arguments = |query: &str, index: u16| {
        json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {"id": "find", "tool": "code.locate", "arguments": {"query": query}},
                {"id": "plan", "tool": "plan.change", "depends_on": ["find"], "arguments": {
                    "objective": "bug_fix",
                    "objective_text": "fix the defect",
                    "targets": [{
                        "symbol_id": {"$from": "find", "source": "symbol_id", "index": index}
                    }]
                }}
            ],
            "explain": true
        })
    };
    let base = batch_explain_fingerprint(&harness, arguments("publish", 0)).await;
    let other_argument = batch_explain_fingerprint(&harness, arguments("stage", 0)).await;
    let other_binding = batch_explain_fingerprint(&harness, arguments("publish", 1)).await;

    assert_ne!(base, other_argument);
    assert_ne!(base, other_binding);
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
            resolved_generation: generation(),
            active_generation: generation(),
            parent_generation: None,
            active_parent_generation: None,
            structural_freshness: structural.to_owned(),
            semantic_freshness: semantic.to_owned(),
            state: state.to_owned(),
            coverage,
            ..repository_status_response()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AcceptedFieldOracle {
    NormalizedDelta,
    BudgetRuntime,
    StructuredVariant,
    StructuredQueryAst,
    DefaultEquivalent,
    ExplainPlan,
    OutputSelection,
    ContextRuntime,
    BatchRuntime,
    LocalTimeout,
    FixedDiscriminator,
    CursorContinuation,
}

#[derive(Debug)]
struct AcceptedFieldEvidence {
    tool: VerticalTool,
    fields: &'static [&'static str],
    excluded_descendants: &'static [&'static str],
    oracle: AcceptedFieldOracle,
}

fn accepted_field_evidence() -> Vec<AcceptedFieldEvidence> {
    let mut evidence = Vec::new();
    macro_rules! group {
        ($tool:ident, $oracle:ident, [$($field:literal),+ $(,)?]) => {{
            evidence.push(AcceptedFieldEvidence {
                tool: VerticalTool::$tool,
                fields: &[$($field),+],
                excluded_descendants: &[],
                oracle: AcceptedFieldOracle::$oracle,
            });
        }};
    }
    macro_rules! group_excluding {
        ($tool:ident, $oracle:ident, [$($field:literal),+ $(,)?], [$($excluded:literal),+ $(,)?]) => {{
            evidence.push(AcceptedFieldEvidence {
                tool: VerticalTool::$tool,
                fields: &[$($field),+],
                excluded_descendants: &[$($excluded),+],
                oracle: AcceptedFieldOracle::$oracle,
            });
        }};
    }

    group!(RepoIndex, NormalizedDelta, ["mode", "root"]);
    group!(RepoIndex, DefaultEquivalent, ["detached"]);
    group!(RepoStatus, NormalizedDelta, ["generation", "repository"]);
    group!(RepoStatus, ExplainPlan, ["explain"]);
    group!(
        RepoStatus,
        DefaultEquivalent,
        [
            "coverage_detail",
            "include_operations",
            "require_freshness",
            "response_profile"
        ]
    );
    group!(
        RepoList,
        OutputSelection,
        ["cursor", "max_results", "query", "states"]
    );
    group!(RepoList, ExplainPlan, ["explain"]);
    group!(RepoList, DefaultEquivalent, ["response_profile"]);
    group!(
        OperationStatus,
        NormalizedDelta,
        ["action", "after_revision", "operation_id", "wait_ms"]
    );
    group!(
        CodeLocate,
        NormalizedDelta,
        [
            "generation",
            "max_results",
            "query",
            "repository",
            "search_modes"
        ]
    );
    group!(CodeLocate, BudgetRuntime, ["budget"]);
    group!(CodeLocate, ExplainPlan, ["explain"]);
    group!(CodeLocate, DefaultEquivalent, ["response_profile"]);
    group!(CodeLocate, CursorContinuation, ["cursor"]);
    group!(
        SymbolExplain,
        NormalizedDelta,
        [
            "generation",
            "include_provenance",
            "repository",
            "symbol_ids"
        ]
    );
    group!(SymbolExplain, ExplainPlan, ["explain"]);
    group!(SymbolExplain, DefaultEquivalent, ["response_profile"]);
    group!(SymbolExplain, BudgetRuntime, ["budget"]);
    group!(
        SymbolRelationships,
        NormalizedDelta,
        [
            "direction",
            "generation",
            "max_results",
            "min_confidence",
            "relations",
            "repository",
            "symbol_ids"
        ]
    );
    group!(SymbolRelationships, ExplainPlan, ["explain"]);
    group!(SymbolRelationships, BudgetRuntime, ["budget"]);
    group!(
        SymbolRelationships,
        DefaultEquivalent,
        ["include_candidates", "response_profile"]
    );
    group!(SymbolRelationships, CursorContinuation, ["cursor"]);
    group!(
        FlowTrace,
        NormalizedDelta,
        [
            "direction",
            "from",
            "generation",
            "max_depth",
            "max_paths",
            "min_confidence",
            "relations",
            "repository",
            "to"
        ]
    );
    group!(FlowTrace, ExplainPlan, ["explain"]);
    group!(FlowTrace, BudgetRuntime, ["budget"]);
    group!(
        FlowTrace,
        DefaultEquivalent,
        ["cross_repository", "response_profile"]
    );
    group_excluding!(
        ChangeImpact,
        NormalizedDelta,
        [
            "change",
            "generation",
            "include_tests",
            "max_depth",
            "min_confidence",
            "relation_policy",
            "repository"
        ],
        ["change.paths"]
    );
    group!(ChangeImpact, StructuredVariant, ["change.paths"]);
    group!(ChangeImpact, ExplainPlan, ["explain"]);
    group!(ChangeImpact, BudgetRuntime, ["budget"]);
    group!(
        ChangeImpact,
        DefaultEquivalent,
        ["include_history", "profile"]
    );
    group!(
        TestsSelect,
        NormalizedDelta,
        [
            "generation",
            "include_commands",
            "max_tests",
            "repository",
            "seeds",
            "test_kinds"
        ]
    );
    group!(TestsSelect, ExplainPlan, ["explain"]);
    group!(TestsSelect, DefaultEquivalent, ["profile"]);
    group!(TestsSelect, BudgetRuntime, ["budget"]);
    group!(
        ArchitectureOverview,
        NormalizedDelta,
        [
            "generation",
            "include_edges",
            "max_components",
            "min_confidence",
            "repository",
            "views"
        ]
    );
    group!(ArchitectureOverview, ExplainPlan, ["explain"]);
    group!(ArchitectureOverview, BudgetRuntime, ["budget"]);
    group!(
        ArchitectureOverview,
        DefaultEquivalent,
        ["response_profile"]
    );
    group!(
        ArchitectureCycles,
        NormalizedDelta,
        [
            "generation",
            "include_self_cycles",
            "max_cycles",
            "min_size",
            "repository"
        ]
    );
    group_excluding!(
        ArchitectureCycles,
        NormalizedDelta,
        ["projection"],
        ["projection.level"]
    );
    group!(ArchitectureCycles, FixedDiscriminator, ["projection.level"]);
    group!(ArchitectureCycles, ExplainPlan, ["explain"]);
    group!(ArchitectureCycles, DefaultEquivalent, ["response_profile"]);
    group!(ArchitectureCycles, BudgetRuntime, ["budget"]);
    group!(
        CodeDead,
        NormalizedDelta,
        [
            "generation",
            "include_exported",
            "include_tests",
            "max_candidates",
            "min_confidence",
            "repository"
        ]
    );
    group!(CodeDead, ExplainPlan, ["explain"]);
    group!(
        CodeDead,
        DefaultEquivalent,
        ["entry_point_policy", "response_profile"]
    );
    group!(CodeDead, BudgetRuntime, ["budget"]);
    group!(
        HistoryCompare,
        NormalizedDelta,
        ["base", "change_kinds", "head", "max_results", "repository"]
    );
    group!(HistoryCompare, ExplainPlan, ["explain"]);
    group!(HistoryCompare, BudgetRuntime, ["budget"]);
    group!(
        HistoryCompare,
        DefaultEquivalent,
        ["include_unchanged_context", "profile"]
    );
    group_excluding!(
        PlanChange,
        NormalizedDelta,
        [
            "generation",
            "max_steps",
            "objective",
            "objective_text",
            "repository",
            "targets"
        ],
        ["targets[].file_id"]
    );
    group!(PlanChange, StructuredVariant, ["targets[].file_id"]);
    group!(PlanChange, ExplainPlan, ["explain"]);
    group!(PlanChange, DefaultEquivalent, ["profile"]);
    group!(PlanChange, BudgetRuntime, ["budget"]);
    group!(
        ContextPack,
        ContextRuntime,
        [
            "diversity",
            "generation",
            "min_confidence",
            "repository",
            "response_profile",
            "sections",
            "seeds",
            "source_policy",
            "task",
            "token_budget"
        ]
    );
    group!(ContextPack, CursorContinuation, ["continuation"]);
    group!(ContextPack, ExplainPlan, ["explain"]);
    group!(
        SourceRead,
        NormalizedDelta,
        [
            "context_lines_after",
            "context_lines_before",
            "generation",
            "references",
            "repository"
        ]
    );
    group!(SourceRead, BudgetRuntime, ["budget", "max_source_bytes"]);
    group!(SourceRead, ExplainPlan, ["explain"]);
    group!(
        SourceRead,
        DefaultEquivalent,
        [
            "encoding",
            "include_line_numbers",
            "merge_overlaps",
            "response_profile"
        ]
    );
    group!(
        QueryAdvanced,
        NormalizedDelta,
        [
            "cost_limit",
            "explain",
            "generation",
            "max_depth",
            "max_results",
            "repository"
        ]
    );
    group!(QueryAdvanced, StructuredQueryAst, ["query", "parameters"]);
    group!(QueryAdvanced, CursorContinuation, ["cursor"]);
    group_excluding!(
        QueryBatch,
        BatchRuntime,
        ["budget", "failure_policy", "operations", "repository"],
        ["operations[].local_budget.timeout_ms"]
    );
    group!(
        QueryBatch,
        LocalTimeout,
        ["operations[].local_budget.timeout_ms"]
    );
    group!(QueryBatch, ExplainPlan, ["explain"]);
    group!(
        QueryBatch,
        DefaultEquivalent,
        ["generation", "response_profile"]
    );
    evidence
}

#[test]
fn accepted_schema_paths_have_effect_evidence() {
    use rootlight_mcp_contract::capability::{CapabilityStatus, capability_for};

    let is_accepted = |status| {
        matches!(
            status,
            CapabilityStatus::Implemented | CapabilityStatus::FallbackLimited
        )
    };
    let groups = accepted_field_evidence();
    let mut registered = Vec::new();
    for group in &groups {
        registered.extend(group.fields.iter().map(|field| (group.tool.name(), *field)));
    }
    registered.sort_unstable();
    let duplicate = registered.windows(2).find(|pair| {
        let [(left_tool, left_field), (right_tool, right_field)] = pair else {
            return false;
        };
        left_tool == right_tool && left_field == right_field
    });
    assert!(
        duplicate.is_none(),
        "duplicate field evidence: {duplicate:?}"
    );

    let mut accepted = Vec::new();
    for (tool, catalog_tool) in VerticalTool::ALL.into_iter().zip(McpTool::ALL) {
        let schema: Value =
            serde_json::from_str(tool.input_schema_json()).expect("built-in schema is valid");
        let capability = capability_for(catalog_tool);
        accepted.extend(
            generated_schema_paths(&schema)
                .into_iter()
                .filter_map(|path| {
                    let path_accepted = is_accepted(capability.disposition(&path, None).status)
                        || capability.rules.iter().any(|rule| {
                            rule.path == path && rule.value.is_some() && is_accepted(rule.status)
                        });
                    path_accepted.then_some((tool.name(), path))
                }),
        );
    }
    accepted.sort_unstable();
    let accepted_snapshot = accepted
        .iter()
        .map(|(tool, path)| format!("{tool}:{path}"))
        .collect::<Vec<_>>()
        .join("\n");
    let accepted_digest = blake3::hash(accepted_snapshot.as_bytes()).to_hex();
    assert_eq!(
        accepted_digest.as_str(),
        "2e4079ef2c5ef2c8797a23b28b8e98a7534663135cc5b9bd1557f3ee1c94764d",
        "accepted path universe changed"
    );
    let categorized: Vec<_> = accepted
        .iter()
        .map(|(tool, path)| {
            let matches: Vec<_> = groups
                .iter()
                .filter(|group| {
                    group.tool.name() == *tool
                        && group
                            .fields
                            .iter()
                            .any(|ancestor| capability_path_is_within(path, ancestor))
                        && !group
                            .excluded_descendants
                            .iter()
                            .any(|excluded| capability_path_is_within(path, excluded))
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{tool}:{path} must have exactly one explicit oracle ancestor, found {matches:?}"
            );
            (*tool, path.as_str(), matches[0].oracle)
        })
        .collect();

    let counts = [
        AcceptedFieldOracle::NormalizedDelta,
        AcceptedFieldOracle::BudgetRuntime,
        AcceptedFieldOracle::StructuredVariant,
        AcceptedFieldOracle::StructuredQueryAst,
        AcceptedFieldOracle::DefaultEquivalent,
        AcceptedFieldOracle::ExplainPlan,
        AcceptedFieldOracle::OutputSelection,
        AcceptedFieldOracle::ContextRuntime,
        AcceptedFieldOracle::BatchRuntime,
        AcceptedFieldOracle::LocalTimeout,
        AcceptedFieldOracle::FixedDiscriminator,
        AcceptedFieldOracle::CursorContinuation,
    ]
    .map(|oracle| {
        categorized
            .iter()
            .filter(|(_, _, registered_oracle)| *registered_oracle == oracle)
            .count()
    });
    println!(
        "accepted_paths={} normalized_delta={} budget_runtime={} structured_variant={} structured_query_ast={} default_equivalent={} explain_plan={} output_selection={} context_runtime={} batch_runtime={} local_timeout={} fixed_discriminator={} cursor_continuation={}",
        categorized.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        counts[5],
        counts[6],
        counts[7],
        counts[8],
        counts[9],
        counts[10],
        counts[11],
    );
    assert_eq!(counts, [130, 97, 3, 69, 28, 16, 5, 16, 25, 1, 1, 4]);
    assert_eq!(categorized.len(), 395);
}

fn capability_path_is_within(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with("[]"))
}

fn generated_schema_paths(schema: &Value) -> Vec<String> {
    fn visit(
        node: &Value,
        path: &str,
        definitions: &Map<String, Value>,
        active_references: &mut std::collections::BTreeSet<String>,
        paths: &mut std::collections::BTreeSet<String>,
    ) {
        if !path.is_empty() {
            paths.insert(path.to_owned());
        }
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            if active_references.insert(reference.to_owned()) {
                let name = reference
                    .strip_prefix("#/$defs/")
                    .expect("built-in schemas use local definitions")
                    .replace("~1", "/")
                    .replace("~0", "~");
                let resolved = definitions
                    .get(&name)
                    .unwrap_or_else(|| panic!("schema definition {name} exists"));
                visit(resolved, path, definitions, active_references, paths);
                active_references.remove(reference);
            }
            return;
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = node.get(keyword).and_then(Value::as_array) {
                for branch in branches {
                    visit(branch, path, definitions, active_references, paths);
                }
            }
        }
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (name, property) in properties {
                let child_path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                visit(property, &child_path, definitions, active_references, paths);
            }
        }
        if let Some(items) = node.get("items") {
            visit(
                items,
                &format!("{path}[]"),
                definitions,
                active_references,
                paths,
            );
        }
    }

    let definitions = schema["$defs"]
        .as_object()
        .expect("tool input schema has definitions");
    let mut paths = std::collections::BTreeSet::new();
    visit(
        schema,
        "",
        definitions,
        &mut std::collections::BTreeSet::new(),
        &mut paths,
    );
    paths.into_iter().collect()
}

#[derive(Debug)]
struct NormalizedDeltaCase {
    tool: VerticalTool,
    field: &'static str,
    first: Value,
    second: Value,
    optional: bool,
}

fn alternate_repository() -> RepositoryId {
    RepositoryId::from_bytes([9; 16])
}

fn alternate_generation() -> GenerationId {
    GenerationId::from_bytes([9; 20])
}

fn replace_top_level_field(mut arguments: Value, field: &str, value: Value) -> Value {
    arguments[field] = value;
    arguments
}

fn normalized_base_arguments(tool: VerticalTool) -> Value {
    match tool {
        VerticalTool::RepoIndex => json!({"root": "C:/fixture-a"}),
        VerticalTool::RepoStatus => {
            json!({"repository": {"repository_id": repository()}})
        }
        VerticalTool::OperationStatus => json!({"operation_id": operation()}),
        VerticalTool::CodeLocate => {
            json!({"repository": {"repository_id": repository()}, "query": "publish"})
        }
        VerticalTool::SymbolExplain => {
            json!({"repository": {"repository_id": repository()}, "symbol_ids": [symbol()]})
        }
        VerticalTool::SymbolRelationships => json!({
            "repository": {"repository_id": repository()},
            "symbol_ids": [symbol()],
            "relations": ["calls"]
        }),
        VerticalTool::FlowTrace => json!({
            "repository": {"repository_id": repository()},
            "from": {"symbol_id": symbol()},
            "relations": ["calls"]
        }),
        VerticalTool::ChangeImpact => json!({
            "repository": {"repository_id": repository()},
            "change": {"symbol_ids": [symbol()]}
        }),
        VerticalTool::TestsSelect => json!({
            "repository": {"repository_id": repository()},
            "seeds": {"symbols": [symbol()]}
        }),
        VerticalTool::ArchitectureOverview => {
            json!({"repository": {"repository_id": repository()}})
        }
        VerticalTool::ArchitectureCycles => json!({
            "repository": {"repository_id": repository()},
            "projection": {"relations": ["calls"], "level": "symbol"}
        }),
        VerticalTool::CodeDead => {
            json!({"repository": {"repository_id": repository()}})
        }
        VerticalTool::HistoryCompare => json!({
            "repository": {"repository_id": repository()},
            "base": parent_generation(),
            "head": generation()
        }),
        VerticalTool::PlanChange => json!({
            "repository": {"repository_id": repository()},
            "objective": "bug_fix",
            "objective_text": "fix the defect",
            "targets": [{"symbol_id": symbol()}]
        }),
        VerticalTool::SourceRead => json!({
            "repository": {"repository_id": repository()},
            "references": [{"source_ref": wire_source_reference(5, 10, 2, 2)}]
        }),
        VerticalTool::QueryAdvanced => json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"}
        }),
        VerticalTool::RepoList | VerticalTool::ContextPack | VerticalTool::QueryBatch => {
            panic!("tool uses a service-level effect oracle")
        }
    }
}

fn normalized_delta_cases(seed: u8) -> Vec<NormalizedDeltaCase> {
    let number = u64::from(seed % 5) + 2;
    let bounded = json!(number);
    let confidence = json!(u16::from(seed % 200) + 700);
    let mut cases = Vec::new();
    let mut add = |tool, field, second, optional| {
        let first = normalized_base_arguments(tool);
        let second = replace_top_level_field(first.clone(), field, second);
        cases.push(NormalizedDeltaCase {
            tool,
            field,
            first,
            second,
            optional,
        });
    };

    add(
        VerticalTool::RepoIndex,
        "root",
        json!("C:/fixture-b"),
        false,
    );
    add(VerticalTool::RepoIndex, "mode", json!("structural"), true);
    add(
        VerticalTool::RepoStatus,
        "repository",
        json!({"repository_id": alternate_repository()}),
        false,
    );
    add(
        VerticalTool::RepoStatus,
        "generation",
        json!(alternate_generation()),
        true,
    );
    add(
        VerticalTool::OperationStatus,
        "operation_id",
        json!(second_operation()),
        false,
    );
    add(
        VerticalTool::OperationStatus,
        "action",
        json!("cancel"),
        true,
    );
    add(
        VerticalTool::OperationStatus,
        "wait_ms",
        bounded.clone(),
        true,
    );
    add(
        VerticalTool::OperationStatus,
        "after_revision",
        bounded.clone(),
        true,
    );
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("query", json!(format!("publish-{seed}")), false),
        ("search_modes", json!(["exact"]), true),
        ("max_results", bounded.clone(), true),
    ] {
        add(VerticalTool::CodeLocate, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("symbol_ids", json!([missing_symbol()]), false),
        ("include_provenance", json!("none"), true),
    ] {
        add(VerticalTool::SymbolExplain, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("symbol_ids", json!([missing_symbol()]), false),
        ("relations", json!(["imports"]), false),
        ("direction", json!("inbound"), true),
        ("min_confidence", confidence.clone(), true),
        ("max_results", bounded.clone(), true),
    ] {
        add(VerticalTool::SymbolRelationships, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("from", json!({"symbol_id": missing_symbol()}), false),
        ("to", json!({"symbol_id": missing_symbol()}), true),
        ("relations", json!(["imports"]), false),
        ("direction", json!("both"), true),
        ("max_depth", bounded.clone(), true),
        ("max_paths", bounded.clone(), true),
        ("min_confidence", confidence.clone(), true),
    ] {
        add(VerticalTool::FlowTrace, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        (
            "change",
            json!({"paths": [format!("src/file-{seed}.rs")]}),
            false,
        ),
        ("include_tests", json!(true), true),
        ("max_depth", bounded.clone(), true),
        ("min_confidence", confidence.clone(), true),
        ("relation_policy", json!("direct_only"), true),
    ] {
        add(VerticalTool::ChangeImpact, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("seeds", json!({"symbols": [missing_symbol()]}), false),
        ("test_kinds", json!(["unit"]), true),
        ("max_tests", bounded.clone(), true),
        ("include_commands", json!(true), true),
    ] {
        add(VerticalTool::TestsSelect, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("views", json!(["hotspots"]), true),
        ("max_components", bounded.clone(), true),
        ("include_edges", json!(true), true),
        ("min_confidence", confidence.clone(), true),
    ] {
        add(VerticalTool::ArchitectureOverview, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        (
            "projection",
            json!({"relations": ["imports"], "level": "symbol"}),
            false,
        ),
        ("min_size", bounded.clone(), true),
        ("max_cycles", bounded.clone(), true),
        ("include_self_cycles", json!(true), true),
    ] {
        add(VerticalTool::ArchitectureCycles, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("include_exported", json!(true), true),
        ("include_tests", json!(true), true),
        ("min_confidence", confidence.clone(), true),
        ("max_candidates", bounded.clone(), true),
    ] {
        add(VerticalTool::CodeDead, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("base", json!(alternate_generation()), false),
        ("head", json!(GenerationId::from_bytes([8; 20])), false),
        ("change_kinds", json!(["entities"]), true),
        ("max_results", bounded.clone(), true),
    ] {
        add(VerticalTool::HistoryCompare, field, value, optional);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("objective", json!("refactor"), false),
        (
            "objective_text",
            json!(format!("refactor safely {seed}")),
            false,
        ),
        ("targets", json!([{"file_id": file()}]), false),
        ("max_steps", bounded.clone(), true),
    ] {
        add(VerticalTool::PlanChange, field, value, optional);
    }
    add(
        VerticalTool::SourceRead,
        "references",
        json!([{"source_ref": wire_source_reference(12, 20, 3, 3)}]),
        false,
    );
    for field in ["context_lines_before", "context_lines_after"] {
        add(VerticalTool::SourceRead, field, json!(2), true);
    }
    for (field, value, optional) in [
        (
            "repository",
            json!({"repository_id": alternate_repository()}),
            false,
        ),
        ("generation", json!(alternate_generation()), true),
        ("cost_limit", json!(10_000_000), true),
        ("max_depth", json!((seed % 4) + 1), true),
        ("max_results", bounded, true),
        ("explain", json!(true), true),
    ] {
        add(VerticalTool::QueryAdvanced, field, value, optional);
    }
    let source_base = normalized_base_arguments(VerticalTool::SourceRead);
    cases.push(NormalizedDeltaCase {
        tool: VerticalTool::SourceRead,
        field: "repository",
        first: source_base,
        second: json!({
            "repository": {"repository_id": alternate_repository()},
            "references": [{
                "source_ref": wire_source_reference_for(
                    alternate_repository(),
                    generation(),
                    5,
                    10,
                    2,
                    2
                )
            }]
        }),
        optional: false,
    });
    cases.push(NormalizedDeltaCase {
        tool: VerticalTool::SourceRead,
        field: "generation",
        first: normalized_base_arguments(VerticalTool::SourceRead),
        second: json!({
            "repository": {"repository_id": repository()},
            "generation": alternate_generation(),
            "references": [{
                "source_ref": wire_source_reference_for(
                    repository(),
                    alternate_generation(),
                    5,
                    10,
                    2,
                    2
                )
            }]
        }),
        optional: true,
    });
    cases
}

fn decode_arguments<T: DeserializeOwned>(arguments: Value) -> T {
    serde_json::from_value(arguments).expect("effect fixture satisfies the typed input")
}

fn normalization_error() -> PublicError {
    PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
        .build()
        .expect("static normalization error is valid")
}

fn normalized_field_observation(tool: VerticalTool, field: &str, arguments: Value) -> Value {
    let unsupported = normalization_error();
    match tool {
        VerticalTool::RepoIndex => {
            let request = normalize_repository_index(
                decode_arguments(arguments),
                &unsupported,
                &normalization_error(),
            )
            .expect("accepted repository-index fixture normalizes");
            match field {
                "root" => json!(request.root()),
                "mode" => json!(format!("{:?}", request.mode())),
                "detached" => json!(request.detached()),
                _ => panic!("unknown repo.index observation field"),
            }
        }
        VerticalTool::RepoStatus => {
            let input: RepoStatusInput = decode_arguments(arguments);
            let request = RepositoryStatusPortRequest::new(
                repository_id(input.repository, &unsupported)
                    .expect("accepted repository selector normalizes"),
                client_generation(input.generation),
            );
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                _ => panic!("unknown repo.status observation field"),
            }
        }
        VerticalTool::OperationStatus => {
            let input: OperationStatusInput = decode_arguments(arguments);
            let request = OperationStatusPortRequest {
                operation: input.operation_id,
                action: match input.action.unwrap_or(OperationAction::Get) {
                    OperationAction::Get => RepositoryOperationAction::Get,
                    OperationAction::Cancel => RepositoryOperationAction::Cancel,
                },
                wait_ms: input.wait_ms,
                after_revision: input.after_revision,
            };
            match field {
                "operation_id" => json!(request.operation()),
                "action" => json!(format!("{:?}", request.action())),
                "wait_ms" => json!(request.wait_ms()),
                "after_revision" => json!(request.after_revision()),
                _ => panic!("unknown operation.status observation field"),
            }
        }
        VerticalTool::CodeLocate => {
            let request = normalize_code_locate(decode_arguments(arguments), &unsupported)
                .expect("accepted locate fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "query" => json!(request.query()),
                "search_modes" => json!(format!("{:?}", request.mode())),
                "max_results" => json!(request.maximum_results()),
                _ => panic!("unknown code.locate observation field"),
            }
        }
        VerticalTool::SymbolExplain => {
            let request = normalize_symbol_explain(decode_arguments(arguments), &unsupported)
                .expect("accepted symbol explanation fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "symbol_ids" => json!(request.symbols()),
                "include_provenance" => json!(request.include_provenance()),
                _ => panic!("unknown symbol.explain observation field"),
            }
        }
        VerticalTool::SymbolRelationships => {
            let request = normalize_symbol_relationships(decode_arguments(arguments), &unsupported)
                .expect("accepted relationship fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "symbol_ids" => json!(request.seeds()),
                "relations" => json!(request.relations()),
                "direction" => json!(request.direction()),
                "min_confidence" => json!(request.min_confidence()),
                "max_results" => json!(request.max_results()),
                _ => panic!("unknown symbol.relationships observation field"),
            }
        }
        VerticalTool::FlowTrace => {
            let request = normalize_flow_trace(decode_arguments(arguments), &unsupported)
                .expect("accepted flow trace fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "from" => json!(request.from()),
                "to" => json!(request.to()),
                "relations" => json!(request.relations()),
                "direction" => json!(request.direction()),
                "max_depth" => json!(request.max_depth()),
                "max_paths" => json!(request.max_paths()),
                "min_confidence" => json!(request.min_confidence()),
                _ => panic!("unknown flow.trace observation field"),
            }
        }
        VerticalTool::ChangeImpact => {
            let request = normalize_change_impact(decode_arguments(arguments), &unsupported)
                .expect("accepted impact fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "change" => json!({
                    "symbols": request.changed_symbols(),
                    "paths": request.changed_paths()
                }),
                "max_depth" | "relation_policy" => json!(request.max_depth()),
                "min_confidence" => json!(request.min_confidence()),
                "include_tests" => json!(request.include_tests()),
                _ => panic!("unknown change.impact observation field"),
            }
        }
        VerticalTool::TestsSelect => {
            let request = normalize_tests_select(decode_arguments(arguments), &unsupported)
                .expect("accepted test-selection fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "seeds" => json!(request.seeds()),
                "test_kinds" => json!(request.test_kinds()),
                "max_tests" => json!(request.max_tests()),
                "include_commands" => json!(request.include_commands()),
                _ => panic!("unknown tests.select observation field"),
            }
        }
        VerticalTool::ArchitectureOverview => {
            let request =
                normalize_architecture_overview(decode_arguments(arguments), &unsupported)
                    .expect("accepted architecture overview fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "views" => json!(request.views()),
                "max_components" => json!(request.max_components()),
                "include_edges" => json!(request.include_edges()),
                "min_confidence" => json!(request.min_confidence()),
                _ => panic!("unknown architecture.overview observation field"),
            }
        }
        VerticalTool::ArchitectureCycles => {
            let request = normalize_architecture_cycles(decode_arguments(arguments), &unsupported)
                .expect("accepted architecture-cycle fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "projection" => json!(request.relations()),
                "min_size" => json!(request.min_size()),
                "max_cycles" => json!(request.max_cycles()),
                "include_self_cycles" => json!(request.include_self_cycles()),
                _ => panic!("unknown architecture.cycles observation field"),
            }
        }
        VerticalTool::CodeDead => {
            let request = normalize_code_dead(decode_arguments(arguments), &unsupported)
                .expect("accepted dead-code fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "entry_point_policy" => json!(request.entry_point_policy()),
                "include_exported" => json!(request.include_exported()),
                "include_tests" => json!(request.include_tests()),
                "min_confidence" => json!(request.min_confidence()),
                "max_candidates" => json!(request.max_candidates()),
                _ => panic!("unknown code.dead observation field"),
            }
        }
        VerticalTool::HistoryCompare => {
            let request = normalize_history_compare(decode_arguments(arguments), &unsupported)
                .expect("accepted history comparison fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "base" => json!(request.base()),
                "head" => json!(request.head()),
                "change_kinds" => json!(request.change_kinds()),
                "max_results" => json!(request.max_results()),
                _ => panic!("unknown history.compare observation field"),
            }
        }
        VerticalTool::PlanChange => {
            let request =
                rootlight_agent::change::normalize_plan_change(decode_arguments(arguments))
                    .expect("accepted change-plan fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(request.generation()),
                "objective" => json!(request.objective()),
                "objective_text" => json!(request.objective_text()),
                "targets" => json!({
                    "symbols": request.target_symbols(),
                    "files": request.target_files()
                }),
                "max_steps" => json!(request.max_steps()),
                _ => panic!("unknown plan.change observation field"),
            }
        }
        VerticalTool::SourceRead => {
            let request = normalize_source_read(
                decode_arguments(arguments),
                &unsupported,
                &normalization_error(),
            )
            .expect("accepted source-read fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "references" => json!(format!("{:?}", request.references())),
                "context_lines_before" => json!(request.context_lines_before()),
                "context_lines_after" => json!(request.context_lines_after()),
                _ => panic!("unknown source.read observation field"),
            }
        }
        VerticalTool::QueryAdvanced => {
            let request = normalize_query_advanced(decode_arguments(arguments), &unsupported)
                .expect("accepted advanced-query fixture normalizes");
            match field {
                "repository" => json!(request.repository()),
                "generation" => json!(format!("{:?}", request.generation())),
                "query" => serde_json::from_str(request.query_ast())
                    .expect("normalized query AST remains JSON"),
                "explain" => json!(request.explain()),
                "max_results" => json!(request.max_results()),
                "max_depth" => json!(request.max_depth()),
                "cost_limit" => json!(request.cost_limit()),
                _ => panic!("unknown query.advanced observation field"),
            }
        }
        VerticalTool::RepoList | VerticalTool::ContextPack | VerticalTool::QueryBatch => {
            panic!("tool uses a service-level effect oracle")
        }
    }
}

#[test]
fn every_normalized_field_case_changes_its_exact_observation() {
    let cases = normalized_delta_cases(0);
    for case in &cases {
        let first = normalized_field_observation(case.tool, case.field, case.first.clone());
        let second = normalized_field_observation(case.tool, case.field, case.second.clone());
        assert_ne!(
            first,
            second,
            "{}:{} produced no normalized delta",
            case.tool.name(),
            case.field
        );
    }

    let expected: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::NormalizedDelta)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    let observed: std::collections::BTreeSet<_> = cases
        .iter()
        .map(|case| (case.tool.name(), case.field))
        .collect();
    assert_eq!(observed, expected);
    assert_eq!(cases.len(), observed.len(), "duplicate normalized case");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn optional_normalized_fields_preserve_a_property_level_delta(
        selected in any::<usize>(),
        seed in any::<u8>(),
    ) {
        let cases: Vec<_> = normalized_delta_cases(seed)
            .into_iter()
            .filter(|case| case.optional)
            .collect();
        let case = &cases[selected % cases.len()];
        let first = normalized_field_observation(case.tool, case.field, case.first.clone());
        let second = normalized_field_observation(case.tool, case.field, case.second.clone());
        prop_assert_ne!(
            first,
            second,
            "{}:{} produced no normalized delta",
            case.tool.name(),
            case.field
        );
    }

    #[test]
    fn any_lower_layer_truncation_survives_final_serialization(
        resource in prop_oneof![
            Just(client::LimitingResourceKind::Rows),
            Just(client::LimitingResourceKind::Edges),
            Just(client::LimitingResourceKind::Results),
            Just(client::LimitingResourceKind::Depth),
            Just(client::LimitingResourceKind::Paths),
            Just(client::LimitingResourceKind::SourceBytes),
            Just(client::LimitingResourceKind::ResponseBytes),
            Just(client::LimitingResourceKind::MemoryBytes),
            Just(client::LimitingResourceKind::Deadline),
            Just(client::LimitingResourceKind::EstimatedTokens),
            Just(client::LimitingResourceKind::Cancellation),
            Just(client::LimitingResourceKind::Capability),
            Just(client::LimitingResourceKind::Coverage),
            Just(client::LimitingResourceKind::PageSize),
        ],
    ) {
        let envelope = map_read_envelope(
            context(0, 0),
            metadata("completeness-property"),
            (),
            truncated_execution(resource, client::ContinuationGuidance::NarrowScope),
            None,
        )
        .expect("valid lower-layer truncation maps");
        let encoded = serde_json::to_value(envelope).expect("final envelope serializes");

        prop_assert_eq!(&encoded["truncated"], &json!(true));
        prop_assert_eq!(&encoded["completeness"]["state"], &json!("truncated"));
        prop_assert_eq!(
            encoded["completeness"]["limiting_resources"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        prop_assert!(encoded["next_cursor"].is_null());
    }
}

struct AdvancedAstCase {
    label: &'static str,
    query: Value,
}

fn scan_query() -> Value {
    json!({"op": "scan", "entity": "function"})
}

fn advanced_ast_cases() -> Vec<AdvancedAstCase> {
    let predicate_case = |label, predicate| AdvancedAstCase {
        label,
        query: json!({
            "op": "filter",
            "input": scan_query(),
            "predicate": predicate
        }),
    };
    let aggregate_case = |label, aggregation| AdvancedAstCase {
        label,
        query: json!({
            "op": "aggregate",
            "input": scan_query(),
            "group_by": ["module"],
            "aggregations": [aggregation]
        }),
    };

    vec![
        AdvancedAstCase {
            label: "operator.scan.filter",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "name",
                    "value": {"text": "entry"}
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.predicates",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "and",
                    "predicates": [{
                        "pred": "equals",
                        "field": "name",
                        "value": {"text": "entry"}
                    }]
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.value.boolean",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "exported",
                    "value": {"boolean": true}
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.value.file",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "file",
                    "value": {"file": file()}
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.value.integer",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "line",
                    "value": {"integer": 7}
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.value.symbol",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "symbol",
                    "value": {"symbol": symbol()}
                }
            }),
        },
        AdvancedAstCase {
            label: "operator.scan.filter.values",
            query: json!({
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "in",
                    "field": "value",
                    "values": [
                        {"text": "entry"},
                        {"integer": 7},
                        {"boolean": true},
                        {"symbol": symbol()},
                        {"file": file()}
                    ]
                }
            }),
        },
        predicate_case(
            "predicate.equals.value.text",
            json!({
                "pred": "equals",
                "field": "name",
                "value": {"text": "entry"}
            }),
        ),
        predicate_case(
            "predicate.not_equals.value.integer",
            json!({
                "pred": "not_equals",
                "field": "line",
                "value": {"integer": 7}
            }),
        ),
        predicate_case(
            "predicate.in.value.boolean",
            json!({
                "pred": "in",
                "field": "value",
                "values": [
                    {"text": "entry"},
                    {"integer": 7},
                    {"boolean": true},
                    {"symbol": symbol()},
                    {"file": file()}
                ]
            }),
        ),
        predicate_case(
            "predicate.equals.value.boolean",
            json!({
                "pred": "equals",
                "field": "exported",
                "value": {"boolean": true}
            }),
        ),
        predicate_case(
            "predicate.equals.value.file",
            json!({
                "pred": "equals",
                "field": "file",
                "value": {"file": file()}
            }),
        ),
        predicate_case(
            "predicate.equals.value.symbol",
            json!({
                "pred": "equals",
                "field": "symbol",
                "value": {"symbol": symbol()}
            }),
        ),
        predicate_case(
            "predicate.and.value.symbol",
            json!({
                "pred": "and",
                "predicates": [{
                    "pred": "equals",
                    "field": "symbol",
                    "value": {"symbol": symbol()}
                }]
            }),
        ),
        predicate_case(
            "predicate.or.value.file",
            json!({
                "pred": "or",
                "predicates": [{
                    "pred": "equals",
                    "field": "file",
                    "value": {"file": file()}
                }]
            }),
        ),
        AdvancedAstCase {
            label: "operator.project",
            query: json!({
                "op": "project",
                "input": scan_query(),
                "columns": ["name", "symbol_id"]
            }),
        },
        AdvancedAstCase {
            label: "operator.join",
            query: json!({
                "op": "join",
                "left": scan_query(),
                "right": {"op": "scan", "entity": "file"},
                "on": "file_id"
            }),
        },
        aggregate_case("aggregate.count", json!({"fn": "count"})),
        aggregate_case("aggregate.sum", json!({"fn": "sum", "field": "line_count"})),
        aggregate_case("aggregate.min", json!({"fn": "min", "field": "line"})),
        aggregate_case("aggregate.max", json!({"fn": "max", "field": "line"})),
        AdvancedAstCase {
            label: "operator.traverse.seed",
            query: json!({
                "op": "traverse",
                "seed": symbol(),
                "relation": "calls",
                "direction": "outbound",
                "max_depth": 2
            }),
        },
        AdvancedAstCase {
            label: "operator.sort",
            query: json!({
                "op": "sort",
                "input": scan_query(),
                "by": [{"field": "name", "descending": true}]
            }),
        },
        AdvancedAstCase {
            label: "operator.limit",
            query: json!({
                "op": "limit",
                "input": scan_query(),
                "max_rows": 19
            }),
        },
    ]
}

#[test]
fn advanced_query_ast_branches_are_losslessly_normalized() {
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::StructuredQueryAst)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            ("query.advanced", "parameters"),
            ("query.advanced", "query"),
        ])
    );

    let unsupported = normalization_error();
    let mut observed_labels = std::collections::BTreeSet::new();
    for case in advanced_ast_cases() {
        let input: QueryAdvancedInput = decode_arguments(json!({
            "repository": {"repository_id": repository()},
            "query": case.query
        }));
        let expected =
            serde_json::to_value(&input.query).expect("typed query AST serializes canonically");
        let request = normalize_query_advanced(input, &unsupported)
            .expect("accepted query AST branch normalizes");
        let observed: Value =
            serde_json::from_str(request.query_ast()).expect("normalized query AST remains JSON");
        assert_eq!(
            observed, expected,
            "typed query AST serialization changed for {}",
            case.label
        );
        assert!(
            observed_labels.insert(case.label),
            "duplicate AST case label"
        );
    }

    let expected_labels = std::collections::BTreeSet::from([
        "aggregate.count",
        "aggregate.max",
        "aggregate.min",
        "aggregate.sum",
        "operator.join",
        "operator.limit",
        "operator.project",
        "operator.scan.filter",
        "operator.scan.filter.predicates",
        "operator.scan.filter.value.boolean",
        "operator.scan.filter.value.file",
        "operator.scan.filter.value.integer",
        "operator.scan.filter.value.symbol",
        "operator.scan.filter.values",
        "operator.sort",
        "operator.traverse.seed",
        "predicate.and.value.symbol",
        "predicate.equals.value.boolean",
        "predicate.equals.value.file",
        "predicate.equals.value.symbol",
        "predicate.equals.value.text",
        "predicate.in.value.boolean",
        "predicate.not_equals.value.integer",
        "predicate.or.value.file",
    ]);
    assert_eq!(observed_labels, expected_labels);

    let schema: Value = serde_json::from_str(VerticalTool::QueryAdvanced.input_schema_json())
        .expect("built-in query schema is valid");
    let query_paths: std::collections::BTreeSet<_> = generated_schema_paths(&schema)
        .into_iter()
        .filter(|path| capability_path_is_within(path, "query"))
        .collect();
    let structural_containers = std::collections::BTreeSet::from([
        "query",
        "query.aggregations",
        "query.aggregations[]",
        "query.by",
        "query.by[]",
        "query.columns",
        "query.filter",
        "query.filter.predicates",
        "query.filter.predicates[]",
        "query.filter.value",
        "query.filter.value.parameter",
        "query.filter.value.parameter.name",
        "query.filter.values",
        "query.filter.values[]",
        "query.filter.values[].parameter",
        "query.filter.values[].parameter.name",
        "query.group_by",
        "query.input",
        "query.left",
        "query.predicate",
        "query.predicate.predicates",
        "query.predicate.predicates[]",
        "query.predicate.value",
        "query.predicate.value.parameter",
        "query.predicate.value.parameter.name",
        "query.predicate.values",
        "query.predicate.values[]",
        "query.predicate.values[].parameter",
        "query.predicate.values[].parameter.name",
        "query.right",
    ]);
    let expected_descendants: std::collections::BTreeSet<_> = [
        "query.aggregations[].field",
        "query.aggregations[].fn",
        "query.by[].descending",
        "query.by[].field",
        "query.columns[]",
        "query.direction",
        "query.entity",
        "query.filter.field",
        "query.filter.pred",
        "query.filter.value.boolean",
        "query.filter.value.file",
        "query.filter.value.integer",
        "query.filter.value.symbol",
        "query.filter.value.text",
        "query.filter.values[].boolean",
        "query.filter.values[].file",
        "query.filter.values[].integer",
        "query.filter.values[].symbol",
        "query.filter.values[].text",
        "query.group_by[]",
        "query.max_depth",
        "query.max_rows",
        "query.on",
        "query.op",
        "query.predicate.field",
        "query.predicate.pred",
        "query.predicate.value.boolean",
        "query.predicate.value.file",
        "query.predicate.value.integer",
        "query.predicate.value.symbol",
        "query.predicate.value.text",
        "query.predicate.values[].boolean",
        "query.predicate.values[].file",
        "query.predicate.values[].integer",
        "query.predicate.values[].symbol",
        "query.predicate.values[].text",
        "query.relation",
        "query.seed",
    ]
    .into_iter()
    .collect();
    let schema_descendants: std::collections::BTreeSet<_> = query_paths
        .iter()
        .map(String::as_str)
        .filter(|path| !structural_containers.contains(path))
        .collect();
    assert_eq!(schema_descendants, expected_descendants);

    fn collect_value_paths(
        value: &Value,
        path: &str,
        paths: &mut std::collections::BTreeSet<String>,
    ) {
        match value {
            Value::Object(object) => {
                for (field, child) in object {
                    let child_path = format!("{path}.{field}");
                    paths.insert(child_path.clone());
                    collect_value_paths(child, &child_path, paths);
                }
            }
            Value::Array(items) => {
                let item_path = format!("{path}[]");
                paths.insert(item_path.clone());
                for item in items {
                    collect_value_paths(item, &item_path, paths);
                }
            }
            _ => {}
        }
    }

    let mut exercised_paths = std::collections::BTreeSet::new();
    for case in advanced_ast_cases() {
        collect_value_paths(&case.query, "query", &mut exercised_paths);
    }
    let exercised_descendants: std::collections::BTreeSet<_> = exercised_paths
        .iter()
        .map(String::as_str)
        .filter(|path| query_paths.contains(*path) && !structural_containers.contains(*path))
        .collect();
    assert_eq!(exercised_descendants, expected_descendants);
}

#[test]
fn advanced_query_parameters_are_bound_before_the_daemon_boundary() {
    let input: QueryAdvancedInput = decode_arguments(json!({
        "repository": {"repository_id": repository()},
        "query": {
            "op": "scan",
            "entity": "function",
            "filter": {
                "pred": "equals",
                "field": "name",
                "value": {"parameter": {"name": "needle"}}
            }
        },
        "parameters": {
            "needle": {"text": "handle_request"}
        }
    }));

    let request = normalize_query_advanced(input, &normalization_error())
        .expect("typed value parameter normalizes");
    let observed: Value =
        serde_json::from_str(request.query_ast()).expect("bound AST remains JSON");

    assert_eq!(
        observed["filter"]["value"],
        json!({"text": "handle_request"})
    );
    assert!(
        !request.query_ast().contains("parameter"),
        "parameter references must not cross the daemon boundary"
    );
}

#[test]
fn advanced_literal_and_parameter_forms_share_canonical_plan_identity() {
    let literal: QueryAdvancedInput = decode_arguments(json!({
        "repository": {"repository_id": repository()},
        "query": {
            "op": "scan",
            "entity": "function",
            "filter": {
                "pred": "equals",
                "field": "name",
                "value": {"text": "handle_request"}
            }
        },
        "max_results": 25,
        "max_depth": 3,
        "cost_limit": 50_000
    }));
    let parameterized: QueryAdvancedInput = decode_arguments(json!({
        "repository": {"repository_id": repository()},
        "query": {
            "op": "scan",
            "entity": "function",
            "filter": {
                "pred": "equals",
                "field": "name",
                "value": {"parameter": {"name": "needle"}}
            }
        },
        "parameters": {"needle": {"text": "handle_request"}},
        "max_results": 25,
        "max_depth": 3,
        "cost_limit": 50_000
    }));
    let literal = normalize_query_advanced(literal, &normalization_error())
        .expect("literal advanced query normalizes");
    let parameterized = normalize_query_advanced(parameterized, &normalization_error())
        .expect("parameterized advanced query normalizes");

    assert_eq!(parameterized.query_ast(), literal.query_ast());
    let literal_context =
        query_advanced_cursor_context(&literal, generation(), ExposureProfile::Developer, 7);
    let parameterized_context =
        query_advanced_cursor_context(&parameterized, generation(), ExposureProfile::Developer, 7);
    assert_eq!(
        parameterized_context.query_fingerprint,
        literal_context.query_fingerprint
    );
    assert_eq!(
        parameterized_context.plan_fingerprint,
        literal_context.plan_fingerprint
    );
}

fn adversarial_parameter_value() -> impl Strategy<Value = (Value, Value)> {
    let suspicious_text = prop_oneof![
        prop::sample::select(vec![
            "SELECT * FROM symbols".to_owned(),
            "MATCH (n) RETURN n".to_owned(),
            "rm -rf /".to_owned(),
            ".*.*".to_owned(),
            "{\"op\":\"shell\"}".to_owned(),
            "{\"field\":{\"parameter\":\"field\"}}".to_owned(),
        ]),
        "[ -~]{1,128}",
    ];
    suspicious_text.prop_map(|text| {
        let value = json!({"text": text});
        (value.clone(), value)
    })
}

fn mismatched_parameter_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(|integer| json!({"integer": integer})),
        any::<bool>().prop_map(|boolean| json!({"boolean": boolean})),
        Just(json!({"symbol": symbol()})),
        Just(json!({"file": file()})),
    ]
}

fn advanced_campaign_cases() -> u32 {
    std::env::var("ROOTLIGHT_ADVANCED_GATE_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|cases| (1..=4_096).contains(cases))
        .unwrap_or(96)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: advanced_campaign_cases(),
        max_shrink_iters: 256,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(202_607_220_042),
        ..ProptestConfig::default()
    })]

    #[test]
    fn advanced_typed_parameters_cannot_replace_fields_operators_or_functions(
        (wire_value, expected_value) in adversarial_parameter_value(),
    ) {
        let input: QueryAdvancedInput = decode_arguments(json!({
            "repository": {"repository_id": repository()},
            "query": {
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "name",
                    "value": {"parameter": {"name": "needle"}}
                }
            },
            "parameters": {"needle": wire_value}
        }));
        let request = normalize_query_advanced(input, &normalization_error())
            .expect("generated typed parameter normalizes");
        let observed: Value =
            serde_json::from_str(request.query_ast()).expect("bound AST remains JSON");

        prop_assert_eq!(&observed["op"], "scan");
        prop_assert_eq!(&observed["entity"], "function");
        prop_assert_eq!(&observed["filter"]["pred"], "equals");
        prop_assert_eq!(&observed["filter"]["field"], "name");
        prop_assert_eq!(&observed["filter"]["value"], &expected_value);
    }

    #[test]
    fn advanced_mismatched_typed_parameters_fail_before_serialization(
        wire_value in mismatched_parameter_value(),
    ) {
        let input: QueryAdvancedInput = decode_arguments(json!({
            "repository": {"repository_id": repository()},
            "query": {
                "op": "scan",
                "entity": "function",
                "filter": {
                    "pred": "equals",
                    "field": "name",
                    "value": {"parameter": {"name": "needle"}}
                }
            },
            "parameters": {"needle": wire_value}
        }));
        let error = normalize_query_advanced(input, &normalization_error())
            .expect_err("a non-text name predicate is rejected");

        prop_assert_eq!(
            error.public_error().map(PublicError::code),
            Some(ErrorCode::TypeMismatch),
        );
    }

    #[test]
    fn advanced_cursor_decoder_replays_bounded_wire_inputs_deterministically(
        wire in "[A-Za-z0-9_%-]{0,512}",
    ) {
        let first = AuthenticatedCursor::from_wire(&wire);
        let replay = AuthenticatedCursor::from_wire(&wire);

        prop_assert_eq!(replay.is_ok(), first.is_ok());
        if let Ok(cursor) = first {
            prop_assert_eq!(cursor.to_wire(), wire);
        }
    }
}

#[test]
fn structural_change_and_plan_variants_reach_distinct_normalized_targets() {
    let unsupported = normalization_error();
    let impact = normalize_change_impact(
        decode_arguments(json!({
            "repository": {"repository_id": repository()},
            "change": {"paths": ["src/lib.rs"]}
        })),
        &unsupported,
    )
    .expect("path-based change normalizes");
    assert!(impact.changed_symbols().is_empty());
    assert_eq!(impact.changed_paths(), &["src/lib.rs"]);

    let plan = rootlight_agent::change::normalize_plan_change(decode_arguments(json!({
        "repository": {"repository_id": repository()},
        "objective": "refactor",
        "objective_text": "refactor safely",
        "targets": [{"file_id": file()}]
    })))
    .expect("file-target change plan normalizes");
    assert!(plan.target_symbols().is_empty());
    assert_eq!(plan.target_files(), &[file()]);

    let expected: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::StructuredVariant)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        expected,
        std::collections::BTreeSet::from([
            ("change.impact", "change.paths"),
            ("plan.change", "targets[].file_id")
        ])
    );
}

#[tokio::test]
async fn cycle_projection_level_accepts_only_the_executable_symbol_discriminator() {
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::FixedDiscriminator)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([("architecture.cycles", "projection.level")])
    );

    let accepted = json!({
        "repository": {"repository_id": repository()},
        "projection": {"relations": ["calls"], "level": "symbol"}
    });
    validate_capability_input(
        VerticalTool::ArchitectureCycles,
        &accepted,
        CapabilityBindingPolicy::Materialized,
    )
    .expect("symbol projection is admitted");

    let rejected = json!({
        "repository": {"repository_id": repository()},
        "projection": {"relations": ["calls"], "level": "module"}
    });
    validate_capability_input(
        VerticalTool::ArchitectureCycles,
        &rejected,
        CapabilityBindingPolicy::Materialized,
    )
    .expect_err("non-symbol projection is rejected");

    let harness = admission_harness(VerticalTool::ArchitectureCycles);
    execute(
        &harness.executor,
        VerticalTool::ArchitectureCycles,
        accepted,
    )
    .await
    .expect_err("admitted projection reaches the failing fake port");
    assert!(matches!(
        harness.only_call(),
        ObservedCall::ArchitectureCycles(_)
    ));
}

struct DefaultEquivalentCase {
    tool: VerticalTool,
    arguments: Value,
    field: &'static str,
    explicit_default: Value,
}

fn default_equivalent_cases() -> Vec<DefaultEquivalentCase> {
    let repository_selector = || json!({"repository_id": repository()});
    let source = wire_source_reference(5, 10, 2, 2);
    let cases = [
        (
            VerticalTool::RepoIndex,
            json!({"root": "C:/fixture"}),
            &[("detached", json!(false))][..],
        ),
        (
            VerticalTool::RepoStatus,
            json!({"repository": repository_selector(), "explain": true}),
            &[
                ("coverage_detail", json!("summary")),
                ("include_operations", json!(false)),
                ("require_freshness", json!("none")),
                ("response_profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::RepoList,
            json!({"explain": true}),
            &[("response_profile", json!("compact"))][..],
        ),
        (
            VerticalTool::CodeLocate,
            json!({"repository": repository_selector(), "query": "publish", "explain": true}),
            &[("response_profile", json!("compact"))][..],
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": repository_selector(), "symbol_ids": [symbol()], "explain": true}),
            &[("response_profile", json!("compact"))][..],
        ),
        (
            VerticalTool::SymbolRelationships,
            json!({"repository": repository_selector(), "symbol_ids": [symbol()], "relations": ["calls"], "explain": true}),
            &[
                ("include_candidates", json!(false)),
                ("response_profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::FlowTrace,
            json!({"repository": repository_selector(), "from": {"symbol_id": symbol()}, "relations": ["calls"], "explain": true}),
            &[
                ("cross_repository", json!(false)),
                ("response_profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::ChangeImpact,
            json!({"repository": repository_selector(), "change": {"symbol_ids": [symbol()]}, "explain": true}),
            &[
                ("include_history", json!(false)),
                ("profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::TestsSelect,
            json!({"repository": repository_selector(), "seeds": {"symbols": [symbol()]}, "explain": true}),
            &[("profile", json!("compact"))][..],
        ),
        (
            VerticalTool::ArchitectureOverview,
            json!({"repository": repository_selector(), "explain": true}),
            &[("response_profile", json!("compact"))][..],
        ),
        (
            VerticalTool::ArchitectureCycles,
            json!({"repository": repository_selector(), "projection": {"relations": ["calls"], "level": "symbol"}, "explain": true}),
            &[("response_profile", json!("compact"))][..],
        ),
        (
            VerticalTool::CodeDead,
            json!({"repository": repository_selector(), "explain": true}),
            &[
                ("entry_point_policy", json!("standard")),
                ("response_profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::HistoryCompare,
            json!({"repository": repository_selector(), "base": parent_generation(), "head": generation(), "explain": true}),
            &[
                ("include_unchanged_context", json!(false)),
                ("profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::PlanChange,
            json!({"repository": repository_selector(), "objective": "bug_fix", "objective_text": "fix the defect", "targets": [{"symbol_id": symbol()}], "explain": true}),
            &[("profile", json!("compact"))][..],
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": repository_selector(), "references": [{"source_ref": source}], "explain": true}),
            &[
                ("encoding", json!("utf8_lossless_when_valid")),
                ("include_line_numbers", json!(true)),
                ("merge_overlaps", json!(false)),
                ("response_profile", json!("compact")),
            ][..],
        ),
        (
            VerticalTool::QueryBatch,
            json!({
                "repository": repository_selector(),
                "operations": [{"id": "find", "tool": "code.locate", "arguments": {"query": "publish"}}],
                "explain": true
            }),
            &[
                ("generation", json!("active")),
                ("response_profile", json!("compact")),
            ][..],
        ),
    ];
    cases
        .into_iter()
        .flat_map(|(tool, arguments, fields)| {
            fields
                .iter()
                .cloned()
                .map(move |(field, explicit_default)| DefaultEquivalentCase {
                    tool,
                    arguments: arguments.clone(),
                    field,
                    explicit_default,
                })
        })
        .collect()
}

#[tokio::test]
async fn accepted_effect_defaults_match_omitted_values() {
    let cases = default_equivalent_cases();
    let observed: std::collections::BTreeSet<_> = cases
        .iter()
        .map(|case| (case.tool.name(), case.field))
        .collect();
    let expected: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::DefaultEquivalent)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(observed, expected);
    assert_eq!(cases.len(), observed.len(), "duplicate default oracle case");

    for case in cases {
        let outcome = match case.tool {
            VerticalTool::RepoIndex => {
                FakeOutcome::RepositoryIndex(Ok(RepositoryIndexPortResponse::new(
                    RepositoryIndex {
                        repository: repository(),
                        operation: operation(),
                        mode: client::RepositoryIndexMode::Structural,
                        state: ClientOperationState::Succeeded,
                        revision: 8,
                        parent_generation: None,
                        published_generation: Some(generation()),
                        discovered_inputs: 1,
                        indexed_files: 1,
                        entities: 1,
                        elapsed_micros: 1,
                        estimated_disk_bytes: 1,
                        diagnostics: Vec::new(),
                    },
                    IndexPlanSummary {
                        scope: IndexPlanScope::Repository,
                        mode: IndexMode::Structural,
                        providers: vec!["treesitter-rust".to_owned()],
                        parent_generation: RequiredNullable(None),
                        estimated_disk_bytes: 1,
                    },
                    Vec::new(),
                )))
            }
            VerticalTool::RepoList => FakeOutcome::RepositoryList(Ok(RepositoryList {
                repositories: vec![RepositoryListEntry {
                    repository_id: repository(),
                    active_generation: generation(),
                    languages: vec!["rust".to_owned()],
                    structural_freshness: "current".to_owned(),
                    semantic_freshness: "current".to_owned(),
                    state: "ready".to_owned(),
                }],
            })),
            _ => FakeOutcome::RepositoryStatus(Ok(repository_status_response())),
        };
        let harness = Harness::new(outcome);
        let omitted = execute(&harness.executor, case.tool, case.arguments.clone())
            .await
            .unwrap_or_else(|error| {
                panic!("{} omitted default failed: {error:?}", case.tool.name())
            });
        let Value::Object(mut explicit_arguments) = case.arguments else {
            panic!("default-equivalence arguments are objects");
        };
        explicit_arguments.insert(case.field.to_owned(), case.explicit_default);
        let mut explicit = execute(
            &harness.executor,
            case.tool,
            Value::Object(explicit_arguments),
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "{} explicit default {} failed: {error:?}",
                case.tool.name(),
                case.field
            )
        });
        let mut omitted = omitted;
        normalize_measured_usage(&mut explicit);
        normalize_measured_usage(&mut omitted);
        assert_eq!(
            explicit,
            omitted,
            "{} explicit default {} must normalize like omission",
            case.tool.name(),
            case.field
        );
    }
}

fn accepted_explain_cases() -> Vec<(VerticalTool, Value)> {
    let repository_selector = || json!({"repository_id": repository()});
    vec![
        (
            VerticalTool::RepoStatus,
            json!({"repository": repository_selector(), "explain": true}),
        ),
        (VerticalTool::RepoList, json!({"explain": true})),
        (
            VerticalTool::CodeLocate,
            json!({"repository": repository_selector(), "query": "publish", "explain": true}),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({"repository": repository_selector(), "symbol_ids": [symbol()], "explain": true}),
        ),
        (
            VerticalTool::SymbolRelationships,
            json!({"repository": repository_selector(), "symbol_ids": [symbol()], "relations": ["calls"], "explain": true}),
        ),
        (
            VerticalTool::FlowTrace,
            json!({"repository": repository_selector(), "from": {"symbol_id": symbol()}, "relations": ["calls"], "explain": true}),
        ),
        (
            VerticalTool::ChangeImpact,
            json!({"repository": repository_selector(), "change": {"symbol_ids": [symbol()]}, "explain": true}),
        ),
        (
            VerticalTool::TestsSelect,
            json!({"repository": repository_selector(), "seeds": {"symbols": [symbol()]}, "explain": true}),
        ),
        (
            VerticalTool::ArchitectureOverview,
            json!({"repository": repository_selector(), "explain": true}),
        ),
        (
            VerticalTool::ArchitectureCycles,
            json!({"repository": repository_selector(), "projection": {"relations": ["calls"], "level": "symbol"}, "explain": true}),
        ),
        (
            VerticalTool::CodeDead,
            json!({"repository": repository_selector(), "explain": true}),
        ),
        (
            VerticalTool::HistoryCompare,
            json!({"repository": repository_selector(), "base": parent_generation(), "head": generation(), "explain": true}),
        ),
        (
            VerticalTool::PlanChange,
            json!({"repository": repository_selector(), "objective": "bug_fix", "objective_text": "fix the defect", "targets": [{"symbol_id": symbol()}], "explain": true}),
        ),
        (
            VerticalTool::ContextPack,
            json!({"repository": repository_selector(), "task": "fix a bug", "seeds": {"symbols": [symbol()]}, "token_budget": 1000, "explain": true}),
        ),
        (
            VerticalTool::SourceRead,
            json!({"repository": repository_selector(), "references": [{"source_ref": wire_source_reference(5, 10, 2, 2)}], "explain": true}),
        ),
        (
            VerticalTool::QueryBatch,
            json!({
                "repository": repository_selector(),
                "operations": [{
                    "id": "find",
                    "tool": "code.locate",
                    "arguments": {"query": "publish"}
                }],
                "explain": true
            }),
        ),
    ]
}

#[tokio::test]
async fn every_explain_oracle_returns_a_plan_without_a_subtool_call() {
    let cases = accepted_explain_cases();
    let observed: std::collections::BTreeSet<_> = cases
        .iter()
        .map(|(tool, _)| (tool.name(), "explain"))
        .collect();
    let expected: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::ExplainPlan)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(observed, expected);
    assert_eq!(cases.len(), observed.len(), "duplicate explain oracle");

    for (tool, arguments) in cases {
        let outcome = if tool == VerticalTool::RepoList {
            FakeOutcome::RepositoryList(Ok(RepositoryList {
                repositories: vec![RepositoryListEntry {
                    repository_id: repository(),
                    active_generation: generation(),
                    languages: vec!["rust".to_owned()],
                    structural_freshness: "current".to_owned(),
                    semantic_freshness: "current".to_owned(),
                    state: "ready".to_owned(),
                }],
            }))
        } else {
            FakeOutcome::RepositoryStatus(Ok(repository_status_response()))
        };
        let harness = Harness::new(outcome);
        let output = execute(&harness.executor, tool, arguments)
            .await
            .unwrap_or_else(|error| panic!("{} explain failed: {error:?}", tool.name()));
        let output = Value::Object(output);
        assert!(
            output
                .pointer("/data/explanation")
                .is_some_and(Value::is_object),
            "{} explain output omitted the plan",
            tool.name()
        );
        assert_eq!(
            harness.call_count.load(Ordering::Relaxed),
            1,
            "{} explain performed more than its metadata lookup",
            tool.name()
        );
        let expected_metadata_call = if tool == VerticalTool::RepoList {
            matches!(harness.only_call(), ObservedCall::RepositoryList(_))
        } else {
            matches!(harness.only_call(), ObservedCall::RepositoryStatus(_))
        };
        assert!(
            expected_metadata_call,
            "{} explain invoked a subtool",
            tool.name()
        );
        if tool == VerticalTool::ContextPack {
            let serialized =
                serde_json::to_vec(&output).expect("context explain response serializes");
            let measured = rootlight_mcp_contract::accounting::estimate_tokens(serialized.len());
            assert_eq!(output["usage"]["json_bytes"], json!(serialized.len()));
            assert_eq!(output["usage"]["estimated_tokens"], json!(measured));
            assert_eq!(
                output["data"]["token_accounting"]["estimated_total"],
                json!(measured)
            );
            let by_section = output["data"]["token_accounting"]["by_section"]
                .as_object()
                .expect("context accounting exposes sections");
            let section_total = by_section
                .values()
                .map(|value| value.as_u64().expect("section tokens are integers"))
                .sum::<u64>();
            assert_eq!(section_total, measured);
            assert!(measured <= 1_000);
        }
    }
}

#[tokio::test]
async fn expanded_response_profiles_execute_with_exact_public_accounting() {
    let expanded_tools = [
        VerticalTool::CodeLocate,
        VerticalTool::SymbolExplain,
        VerticalTool::SymbolRelationships,
        VerticalTool::FlowTrace,
        VerticalTool::ChangeImpact,
        VerticalTool::TestsSelect,
        VerticalTool::ArchitectureOverview,
        VerticalTool::ArchitectureCycles,
        VerticalTool::CodeDead,
        VerticalTool::PlanChange,
    ];

    for (tool, arguments) in accepted_explain_cases()
        .into_iter()
        .filter(|(tool, _)| expanded_tools.contains(tool))
    {
        for profile in ["standard", "evidence"] {
            let Value::Object(mut arguments) = arguments.clone() else {
                panic!("accepted explain arguments are objects");
            };
            let field = if matches!(
                tool,
                VerticalTool::ChangeImpact | VerticalTool::TestsSelect | VerticalTool::PlanChange
            ) {
                "profile"
            } else {
                "response_profile"
            };
            arguments.insert(field.to_owned(), json!(profile));

            let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(
                repository_status_response(),
            )));
            let output = execute(&harness.executor, tool, Value::Object(arguments))
                .await
                .unwrap_or_else(|error| {
                    panic!("{} {profile} profile failed: {error:?}", tool.name())
                });
            let serialized = serde_json::to_vec(&Value::Object(output.clone()))
                .expect("profiled response serializes");

            assert_eq!(
                output["usage"]["json_bytes"],
                u64::try_from(serialized.len()).expect("test response size fits u64"),
                "{} {profile} byte accounting drifted",
                tool.name()
            );
            assert_eq!(
                output["usage"]["estimated_tokens"],
                rootlight_mcp_contract::accounting::estimate_tokens(serialized.len()),
                "{} {profile} token accounting drifted",
                tool.name()
            );
            assert_eq!(
                harness.call_count.load(Ordering::Relaxed),
                1,
                "{} {profile} explain performed more than its metadata lookup",
                tool.name()
            );
        }
    }
}

#[tokio::test]
async fn accepted_effect_code_locate_controls_change_the_normalized_request() {
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(locate_response())));
    let base = json!({
        "repository": {"repository_id": repository()},
        "query": "publish"
    });
    for arguments in [
        base.clone(),
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish",
            "search_modes": ["exact"]
        }),
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish",
            "max_results": 7
        }),
        json!({
            "repository": {"repository_id": repository()},
            "query": "publish",
            "budget": {"max_results": 5}
        }),
    ] {
        execute(&harness.executor, VerticalTool::CodeLocate, arguments)
            .await
            .expect("accepted locate controls execute");
    }
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let requests: Vec<_> = calls
        .iter()
        .map(|call| {
            let ObservedCall::CodeLocate(request) = call else {
                panic!("expected only locate requests");
            };
            (request.mode(), request.maximum_results())
        })
        .collect();
    assert_eq!(
        requests,
        [
            (LocateMode::Text, 20),
            (LocateMode::Exact, 20),
            (LocateMode::Text, 7),
            (LocateMode::Text, 5),
        ]
    );
}

#[tokio::test]
async fn accepted_effect_context_pack_token_budget_enforces_final_representation() {
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::ContextRuntime)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            ("context.pack", "diversity"),
            ("context.pack", "generation"),
            ("context.pack", "min_confidence"),
            ("context.pack", "repository"),
            ("context.pack", "response_profile"),
            ("context.pack", "sections"),
            ("context.pack", "seeds"),
            ("context.pack", "source_policy"),
            ("context.pack", "task"),
            ("context.pack", "token_budget"),
        ])
    );

    let mut response = explain_response(source_reference(4, 12, 2, 2));
    response.result.symbols[0].signature = Some("x".repeat(2_400));
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(response)));
    let execute_with_budget = |token_budget| {
        execute(
            &harness.executor,
            VerticalTool::ContextPack,
            json!({
                "repository": {"repository_id": repository()},
                "task": "fix the duplicate payment bug",
                "seeds": {"symbols": [symbol()]},
                "token_budget": token_budget
            }),
        )
    };
    let smaller = execute_with_budget(500)
        .await
        .expect_err("smaller budget cannot fit the truthful final envelope");
    assert_eq!(
        smaller
            .public_error()
            .expect("budget exhaustion is checked")
            .code(),
        ErrorCode::BudgetExceeded
    );
    let larger: ContextPackOutput = decode(
        execute_with_budget(4_500)
            .await
            .expect("larger accepted token budget executes"),
    );
    let ToolResponse::Success(larger) = larger else {
        panic!("expected larger context pack success");
    };
    assert_eq!(larger.data.items.len(), 1);
    assert!(!larger.truncated);
}

#[tokio::test]
async fn context_pack_public_cursor_resumes_once_without_duplicate_evidence() {
    let second_symbol = SymbolId::from_bytes([9; 20]);
    let mut response = explain_response(source_reference(4, 12, 2, 2));
    response.result.symbols[0].signature = Some("a".repeat(900));
    let mut second = response.result.symbols[0].clone();
    second.symbol = second_symbol;
    second.display_name = "publish_secondary".to_owned();
    second.signature = Some("b".repeat(900));
    second.definition = source_reference(20, 32, 4, 5);
    response.result.symbols.push(second);
    response.result.unresolved_symbols.clear();

    let harness = Harness::new(FakeOutcome::SymbolExplainPerRequest(Ok(response)));
    let base = json!({
        "repository": {"repository_id": repository()},
        "task": "explain the publishing path",
        "seeds": {"symbols": [symbol(), second_symbol]},
        "token_budget": 1_530
    });
    let first: ContextPackOutput = decode(
        execute(&harness.executor, VerticalTool::ContextPack, base.clone())
            .await
            .expect("first public context page succeeds"),
    );
    let ToolResponse::Success(first) = first else {
        panic!("expected first public context success");
    };
    let cursor = first
        .next_cursor
        .0
        .clone()
        .expect("first public page emits an authenticated cursor");
    assert_eq!(first.data.items.len(), 1);

    let mut resume = base;
    resume
        .as_object_mut()
        .expect("context arguments are an object")
        .insert("continuation".to_owned(), json!(cursor.as_str()));
    let second: ContextPackOutput = decode(
        execute(&harness.executor, VerticalTool::ContextPack, resume)
            .await
            .expect("public context continuation resumes"),
    );
    let ToolResponse::Success(second) = second else {
        panic!("expected second public context success");
    };
    assert_eq!(second.data.items.len(), 1);
    assert_ne!(
        first.data.items[0].symbol_id,
        second.data.items[0].symbol_id
    );
    assert!(second.next_cursor.0.is_none());
}

#[tokio::test]
async fn context_pack_identity_task_and_seed_branches_have_runtime_effects() {
    let context_arguments =
        |repository_id, generation_selector: Option<GenerationId>, task, seeds| {
            let mut arguments = json!({
                "repository": {"repository_id": repository_id},
                "task": task,
                "seeds": seeds,
                "token_budget": 4_500
            });
            if let Some(generation_selector) = generation_selector {
                arguments
                    .as_object_mut()
                    .expect("context fixture is an object")
                    .insert("generation".to_owned(), json!(generation_selector));
            }
            arguments
        };

    let base_identity = Harness::new(FakeOutcome::RepositoryStatus(Ok(
        repository_status_response(),
    )));
    execute(
        &base_identity.executor,
        VerticalTool::ContextPack,
        context_arguments(
            repository(),
            None,
            "fix a bug",
            json!({"symbols": [symbol()]}),
        ),
    )
    .await
    .expect("provider absence produces a truthful empty context pack");
    let base_identity_call = {
        let calls = base_identity
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        let ObservedCall::RepositoryStatus(request) = &calls[0] else {
            panic!("context identity starts with repository status");
        };
        request.clone()
    };

    let mut alternate_status = repository_status_response();
    alternate_status.repository_id = alternate_repository();
    let alternate_repository_harness =
        Harness::new(FakeOutcome::RepositoryStatus(Ok(alternate_status)));
    execute(
        &alternate_repository_harness.executor,
        VerticalTool::ContextPack,
        context_arguments(
            alternate_repository(),
            None,
            "fix a bug",
            json!({"symbols": [symbol()]}),
        ),
    )
    .await
    .expect("provider absence produces a truthful empty context pack");
    let alternate_repository_call = {
        let calls = alternate_repository_harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        let ObservedCall::RepositoryStatus(request) = &calls[0] else {
            panic!("context identity starts with repository status");
        };
        request.clone()
    };
    assert_ne!(
        base_identity_call.repository(),
        alternate_repository_call.repository()
    );

    let mut alternate_generation_status = repository_status_response();
    alternate_generation_status.resolved_generation = alternate_generation();
    alternate_generation_status.parent_generation = None;
    alternate_generation_status.structural_freshness = "superseded".to_owned();
    alternate_generation_status.semantic_freshness = "superseded".to_owned();
    alternate_generation_status.publication_state = "retained".to_owned();
    let alternate_generation_harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(
        alternate_generation_status,
    )));
    execute(
        &alternate_generation_harness.executor,
        VerticalTool::ContextPack,
        context_arguments(
            repository(),
            Some(alternate_generation()),
            "fix a bug",
            json!({"symbols": [symbol()]}),
        ),
    )
    .await
    .expect("provider absence produces a truthful empty context pack");
    let (alternate_generation_call, alternate_child_generation) = {
        let calls = alternate_generation_harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        let ObservedCall::RepositoryStatus(request) = &calls[0] else {
            panic!("context identity starts with repository status");
        };
        let ObservedCall::SymbolExplain(child) = &calls[1] else {
            panic!("resolved retained identity is passed to the retrieval child");
        };
        (request.clone(), child.generation())
    };
    assert_ne!(
        base_identity_call.generation(),
        alternate_generation_call.generation()
    );
    assert_eq!(
        alternate_child_generation,
        ClientGenerationSelector::Generation(alternate_generation()),
        "agent identity must pin the exact resolved retained generation"
    );

    let execute_explain = |task: &'static str, seeds: Value| async move {
        let harness = Harness::new(FakeOutcome::RepositoryStatus(Ok(
            repository_status_response(),
        )));
        let output: ContextPackOutput = decode(
            execute(
                &harness.executor,
                VerticalTool::ContextPack,
                json!({
                    "repository": {"repository_id": repository()},
                    "task": task,
                    "seeds": seeds,
                    "token_budget": 1_000,
                    "explain": true
                }),
            )
            .await
            .expect("context explain fixture executes"),
        );
        let ToolResponse::Success(output) = output else {
            panic!("expected context pack success");
        };
        output.data.pack_id
    };
    let first_task = execute_explain("fix a bug", json!({"symbols": [symbol()]})).await;
    let second_task = execute_explain("review a bug", json!({"symbols": [symbol()]})).await;
    assert_ne!(first_task, second_task);

    let symbol_seed_harness = Harness::new(FakeOutcome::SymbolExplain(Ok(explain_response(
        source_reference(4, 12, 2, 2),
    ))));
    execute(
        &symbol_seed_harness.executor,
        VerticalTool::ContextPack,
        context_arguments(
            repository(),
            None,
            "fix a bug",
            json!({"symbols": [symbol()]}),
        ),
    )
    .await
    .expect("symbol-seeded context pack executes");
    let symbol_request = {
        let calls = symbol_seed_harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        let ObservedCall::SymbolExplain(request) = &calls[1] else {
            panic!("context retrieval uses symbol explanation");
        };
        request.clone()
    };

    let test_seed_harness = Harness::new(FakeOutcome::SymbolExplain(Ok(explain_response(
        source_reference(4, 12, 2, 2),
    ))));
    execute(
        &test_seed_harness.executor,
        VerticalTool::ContextPack,
        context_arguments(
            repository(),
            None,
            "fix a bug",
            json!({"tests": [missing_symbol()]}),
        ),
    )
    .await
    .expect("test-seeded context pack executes");
    let test_request = {
        let calls = test_seed_harness
            .calls
            .lock()
            .expect("fake call recorder is not poisoned");
        let ObservedCall::SymbolExplain(request) = &calls[1] else {
            panic!("test seeds are materialized as symbol explanation anchors");
        };
        request.clone()
    };
    assert_eq!(symbol_request.symbols(), &[symbol()]);
    assert_eq!(test_request.symbols(), &[missing_symbol()]);
}

#[tokio::test]
async fn accepted_effect_query_advanced_controls_change_the_normalized_request() {
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
            next_page_offset: None,
            execution_completeness: complete_execution(),
        },
        metadata("query-advanced-controls"),
    );
    let harness = Harness::new(FakeOutcome::QueryAdvanced(Ok(response)));
    let base = json!({
        "repository": {"repository_id": repository()},
        "query": {"op": "scan", "entity": "function"}
    });
    for arguments in [
        base,
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "max_results": 7
        }),
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "max_depth": 2
        }),
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "cost_limit": 10_000_000
        }),
        json!({
            "repository": {"repository_id": repository()},
            "query": {"op": "scan", "entity": "function"},
            "explain": true
        }),
    ] {
        execute(&harness.executor, VerticalTool::QueryAdvanced, arguments)
            .await
            .expect("accepted advanced-query control executes");
    }
    let calls = harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let requests: Vec<_> = calls
        .iter()
        .map(|call| {
            let ObservedCall::QueryAdvanced(request) = call else {
                panic!("expected only advanced-query requests");
            };
            (
                request.max_results(),
                request.max_depth(),
                request.cost_limit(),
                request.explain(),
            )
        })
        .collect();
    assert_eq!(
        requests,
        [
            (None, None, None, None),
            (Some(7), None, None, None),
            (None, Some(2), None, None),
            (None, None, Some(10_000_000), None),
            (None, None, None, Some(true)),
        ]
    );
}

#[tokio::test]
async fn accepted_effect_query_batch_failure_policy_changes_scheduling() {
    let registered: std::collections::BTreeSet<_> = accepted_field_evidence()
        .into_iter()
        .filter(|evidence| evidence.oracle == AcceptedFieldOracle::BatchRuntime)
        .flat_map(|evidence| {
            evidence
                .fields
                .iter()
                .copied()
                .map(move |field| (evidence.tool.name(), field))
        })
        .collect();
    assert_eq!(
        registered,
        std::collections::BTreeSet::from([
            ("query.batch", "budget"),
            ("query.batch", "failure_policy"),
            ("query.batch", "operations"),
            ("query.batch", "repository"),
        ])
    );

    let arguments = |failure_policy: Option<&str>| {
        let mut arguments = json!({
            "repository": {"repository_id": repository()},
            "operations": [
                {
                    "id": "z_independent",
                    "tool": "code.locate",
                    "arguments": {"query": "publish"}
                },
                {
                    "id": "a_unavailable",
                    "tool": "symbol.relationships",
                    "arguments": {"symbol_ids": [symbol()], "relations": ["calls"]}
                }
            ]
        });
        if let Some(failure_policy) = failure_policy {
            arguments["failure_policy"] = json!(failure_policy);
        }
        arguments
    };
    let default_harness = batch_harness();
    let default: QueryBatchOutput = decode(
        execute(
            &default_harness.executor,
            VerticalTool::QueryBatch,
            arguments(None),
        )
        .await
        .expect("default batch policy executes"),
    );
    let fail_fast_harness = batch_harness();
    let fail_fast: QueryBatchOutput = decode(
        execute(
            &fail_fast_harness.executor,
            VerticalTool::QueryBatch,
            arguments(Some("fail_fast")),
        )
        .await
        .expect("fail-fast batch policy executes"),
    );
    let ToolResponse::Success(default) = default else {
        panic!("expected default batch success envelope");
    };
    let ToolResponse::Success(fail_fast) = fail_fast else {
        panic!("expected fail-fast batch success envelope");
    };
    assert_eq!(
        default.data.operation_results[0].status,
        BatchOperationStatus::Ok
    );
    assert_eq!(
        fail_fast.data.operation_results[0].status,
        BatchOperationStatus::NotRunFailFast
    );
    assert_eq!(default.data.operation_results.len(), 2);
    assert_eq!(fail_fast.data.operation_results.len(), 2);

    let mut alternate_status = repository_status_response();
    alternate_status.repository_id = alternate_repository();
    let alternate_harness = Harness::new(FakeOutcome::Batch {
        status: Box::new(Ok(alternate_status)),
        locate: executor_failure(),
    });
    execute(
        &alternate_harness.executor,
        VerticalTool::QueryBatch,
        json!({
            "repository": {"repository_id": alternate_repository()},
            "operations": [{
                "id": "find",
                "tool": "code.locate",
                "arguments": {"query": "publish"}
            }]
        }),
    )
    .await
    .expect("alternate batch identity remains operation-local");
    let calls = alternate_harness
        .calls
        .lock()
        .expect("fake call recorder is not poisoned");
    let ObservedCall::RepositoryStatus(identity) = &calls[0] else {
        panic!("batch identity resolution is the first daemon call");
    };
    assert_eq!(identity.repository(), alternate_repository());
}
