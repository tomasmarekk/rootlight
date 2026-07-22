//! Integration tests for transport-neutral batch orchestration.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use rootlight_agent::{
    batch::{BatchOrchestrationError, BatchPublicErrors, BatchService},
    policy::CancellationSignal,
    port::{AgentCallContext, AgentPortError, AgentPortFuture, AgentToolPort, AgentToolRequest},
};
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::CoverageStatus;
use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, SchemaVersion, TrustClassification,
    context::{BatchOperation, BatchOperationStatus, BatchStatus, BatchTool, QueryBatchInput},
    vertical::{
        CacheStatus, CoverageSummary, Freshness, GenerationSelector, GenerationSummary,
        ReadEnvelope, RepositoryIdSelector, RequiredNullable, ResolvedRepository, ResponseBudget,
        ResponseProfile, UsageSummary,
    },
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy)]
struct TestCancellation(bool);

impl CancellationSignal for TestCancellation {
    fn is_cancelled(&self) -> bool {
        self.0
    }
}

#[derive(Debug)]
struct RecordedCall {
    request: AgentToolRequest,
    budget: ResponseBudget,
    has_deadline: bool,
}

#[derive(Debug, Default)]
struct FakePort {
    responses: Mutex<VecDeque<Result<ReadEnvelope<Value>, AgentPortError>>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl FakePort {
    fn with_responses(
        responses: impl IntoIterator<Item = Result<ReadEnvelope<Value>, AgentPortError>>,
    ) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl AgentToolPort<TestCancellation> for FakePort {
    fn execute(
        &self,
        request: AgentToolRequest,
        context: AgentCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>> {
        self.calls
            .lock()
            .expect("call lock is available")
            .push(RecordedCall {
                request,
                budget: context.budget().clone(),
                has_deadline: context.deadline().is_some(),
            });
        let response = self
            .responses
            .lock()
            .expect("response lock is available")
            .pop_front()
            .expect("test configured one response per call");
        Box::pin(async move { response })
    }
}

fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn generation(byte: u8) -> GenerationId {
    GenerationId::from_bytes([byte; 20])
}

fn response(generation: GenerationId, tokens: u64, data: Value) -> ReadEnvelope<Value> {
    ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: ResolvedRepository {
            repository_id: repository(),
            display_name: "fixture".to_owned(),
        },
        generation: GenerationSummary {
            generation_id: generation,
            parent_generation: RequiredNullable(None),
            structural_freshness: Freshness::Current,
            semantic_freshness: Freshness::Current,
        },
        coverage: CoverageSummary {
            status: CoverageStatus::Bounded,
            languages: Vec::new(),
            skipped_inputs: 0,
        },
        data,
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage: UsageSummary {
            rows: 1,
            edges: 2,
            source_bytes: 3,
            json_bytes: 4,
            estimated_tokens: tokens,
            wall_time_ms: 5,
            cache_status: CacheStatus::Miss,
            trace_id: format!("trace-{tokens}"),
        },
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    }
}

fn budget(max_tokens: u16) -> ResponseBudget {
    ResponseBudget {
        max_results: None,
        max_tokens: Some(max_tokens),
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: Some(1_000),
        evidence_level: None,
    }
}

fn operation(
    id: &str,
    tool: BatchTool,
    arguments: Map<String, Value>,
    depends_on: Option<Vec<&str>>,
    local_budget: Option<ResponseBudget>,
) -> BatchOperation {
    BatchOperation {
        id: id.to_owned(),
        tool,
        depends_on: depends_on
            .map(|dependencies| dependencies.into_iter().map(str::to_owned).collect()),
        arguments,
        local_budget,
    }
}

fn input(operations: Vec<BatchOperation>, budget: ResponseBudget) -> QueryBatchInput {
    QueryBatchInput {
        repository: RepositorySelector::ById(RepositoryIdSelector {
            repository_id: repository(),
        }),
        generation: Some(GenerationSelector::Explicit(generation(2))),
        operations,
        failure_policy: None,
        budget: Some(budget),
        response_profile: Some(ResponseProfile::Compact),
        explain: None,
    }
}

fn errors() -> BatchPublicErrors {
    BatchPublicErrors::new(
        PublicError::builder(ErrorCode::BindingInvalid, "invalid binding")
            .build()
            .expect("static error is valid"),
        PublicError::builder(ErrorCode::BindingTypeMismatch, "binding type mismatch")
            .build()
            .expect("static error is valid"),
        PublicError::builder(ErrorCode::Internal, "child failed")
            .build()
            .expect("static error is valid"),
        PublicError::builder(ErrorCode::BudgetExceeded, "budget exceeded")
            .build()
            .expect("static error is valid"),
    )
}

#[tokio::test]
async fn service_materializes_bindings_and_propagates_policy() {
    let mut second_arguments = Map::new();
    second_arguments.insert(
        "symbol_ids".to_owned(),
        json!({"$from": "find", "pointer": "/data/symbol_ids"}),
    );
    let input = input(
        vec![
            operation("find", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "explain",
                BatchTool::SymbolExplain,
                second_arguments,
                Some(vec!["find"]),
                None,
            ),
        ],
        budget(1_000),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(response(generation(2), 100, json!({"symbol_ids": []}))),
        Ok(response(generation(2), 100, json!({"symbols": []}))),
    ]));

    let output = BatchService
        .execute(
            Arc::clone(&port),
            input,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("batch succeeds");

    assert_eq!(output.data.batch_status, BatchStatus::Ok);
    assert_eq!(output.usage.estimated_tokens, 200);
    let calls = port.calls.lock().expect("call lock is available");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].request.clone().into_arguments()["symbol_ids"],
        json!([])
    );
    assert_eq!(calls[0].budget.max_tokens, Some(1_000));
    assert!(calls.iter().all(|call| call.has_deadline));
}

#[tokio::test]
async fn local_budget_failure_preserves_independent_success_and_usage() {
    let input = input(
        vec![
            operation("first", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "second",
                BatchTool::SourceRead,
                Map::new(),
                Some(vec!["first"]),
                Some(budget(100)),
            ),
        ],
        budget(1_000),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(response(generation(2), 100, json!({"items": []}))),
        Ok(response(generation(2), 200, json!({"chunks": []}))),
    ]));

    let output = BatchService
        .execute(port, input, repository(), TestCancellation(false), errors())
        .await
        .expect("independent success preserves the batch");

    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output.data.operation_results[1].status,
        BatchOperationStatus::Error
    );
    assert_eq!(output.usage.estimated_tokens, 300);
}

#[tokio::test]
async fn aggregate_budget_is_charged_across_children() {
    let input = input(
        vec![
            operation("first", BatchTool::CodeLocate, Map::new(), None, None),
            operation("second", BatchTool::SourceRead, Map::new(), None, None),
        ],
        budget(250),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(response(generation(2), 100, json!({}))),
        Ok(response(generation(2), 200, json!({}))),
    ]));

    assert_eq!(
        BatchService
            .execute(port, input, repository(), TestCancellation(false), errors(),)
            .await,
        Err(BatchOrchestrationError::BudgetExceeded)
    );
}

#[tokio::test]
async fn cancellation_stops_before_child_dispatch() {
    let input = input(
        vec![operation(
            "first",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(500),
    );
    let port = Arc::new(FakePort::default());

    assert_eq!(
        BatchService
            .execute(
                Arc::clone(&port),
                input,
                repository(),
                TestCancellation(true),
                errors(),
            )
            .await,
        Err(BatchOrchestrationError::Cancelled)
    );
    assert!(
        port.calls
            .lock()
            .expect("call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn mismatched_child_generation_fails_closed() {
    let input = input(
        vec![
            operation("first", BatchTool::CodeLocate, Map::new(), None, None),
            operation("second", BatchTool::SourceRead, Map::new(), None, None),
        ],
        budget(500),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(response(generation(2), 100, json!({}))),
        Ok(response(generation(3), 100, json!({}))),
    ]));

    assert_eq!(
        BatchService
            .execute(port, input, repository(), TestCancellation(false), errors(),)
            .await,
        Err(BatchOrchestrationError::InvalidResponse)
    );
}

#[tokio::test]
async fn all_child_errors_preserve_complete_per_operation_outcome() {
    let input = input(
        vec![operation(
            "unsupported",
            BatchTool::SymbolRelationships,
            Map::new(),
            None,
            None,
        )],
        budget(500),
    );
    let unsupported = PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "capability is unavailable",
    )
    .build()
    .expect("static error is valid");
    let port = Arc::new(FakePort::with_responses([Err(AgentPortError::Public(
        Box::new(unsupported),
    ))]));

    let error = BatchService
        .execute(port, input, repository(), TestCancellation(false), errors())
        .await
        .expect_err("no successful child returns metadata-free operation outcomes");
    let BatchOrchestrationError::NoSuccessfulOperation(output) = error else {
        panic!("expected unsuccessful operation payload");
    };
    assert_eq!(output.operation_results.len(), 1);
    assert_eq!(
        output.operation_results[0]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::UnsupportedCapability)
    );
}

#[tokio::test]
async fn bound_child_type_error_uses_binding_specific_code() {
    let mut arguments = Map::new();
    arguments.insert(
        "query".to_owned(),
        json!({"$from": "find", "pointer": "/data/matches"}),
    );
    let input = input(
        vec![
            operation("find", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "refine",
                BatchTool::CodeLocate,
                arguments,
                Some(vec!["find"]),
                None,
            ),
        ],
        budget(500),
    );
    let type_mismatch = PublicError::builder(ErrorCode::TypeMismatch, "value has the wrong type")
        .build()
        .expect("static error is valid");
    let port = Arc::new(FakePort::with_responses([
        Ok(response(
            generation(2),
            100,
            json!({"matches": [{"symbol_id": "symbol"}]}),
        )),
        Err(AgentPortError::Public(Box::new(type_mismatch))),
    ]));

    let output = BatchService
        .execute(port, input, repository(), TestCancellation(false), errors())
        .await
        .expect("the independent successful child preserves the batch envelope");
    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output.data.operation_results[1]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::BindingTypeMismatch)
    );
}
