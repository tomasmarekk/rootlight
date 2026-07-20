//! Focused fake-port tests for the production first-slice MCP executor.
//!
//! The fixtures assert wire-visible facts and keep daemon transport out of the
//! mapping suite so failures remain deterministic and source-free.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use rootlight_client::{
    AnalysisTier as ClientTier, CoverageStatus as ClientCoverage, LocateHit, OperationKind,
    OperationStage, OperationState as ClientOperationState, QueryContext, QueryUsage,
    RecoveryClass, SourceChunk as ClientSourceChunk, SymbolExplanation as ClientExplanation,
};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{CoverageStatus as IrCoverage, LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    CodeLocateOutput, ErrorCode, OperationStatusOutput, RepoIndexOutput, SourceReadOutput,
    SymbolExplainOutput,
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
    SymbolExplain(Result<SymbolExplainPortResponse, ClientPortError>),
    SourceRead(Result<SourceReadPortResponse, ClientPortError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservedCall {
    RepositoryIndex(RepositoryIndexPortRequest),
    OperationStatus(OperationStatusPortRequest),
    CodeLocate(CodeLocatePortRequest),
    SymbolExplain(SymbolExplainPortRequest),
    SourceRead(SourceReadPortRequest),
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
    let router = ToolRouter::new(harness.executor, rootlight_mcp_contract::ExposureProfile::Developer).expect("tool catalog compiles");
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
    let router = ToolRouter::new(harness.executor, rootlight_mcp_contract::ExposureProfile::Developer).expect("router compiles");

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
        let router =
            ToolRouter::new(Harness::new(FakeOutcome::RepositoryIndex(Err(error))).executor, rootlight_mcp_contract::ExposureProfile::Developer)
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
