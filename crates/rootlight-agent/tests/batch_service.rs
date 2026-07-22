//! Integration tests for transport-neutral batch orchestration.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use rootlight_agent::{
    batch::{
        BatchExecutionError, BatchOrchestrationError, BatchPublicErrors, BatchService,
        resolve_arguments,
    },
    policy::CancellationSignal,
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentPortFuture,
        AgentResolutionContext, AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{CoverageStatus, SourceRef, SourceSpan};
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
    pinned_generation: Option<GenerationId>,
    has_deadline: bool,
}

#[derive(Debug, Default)]
struct FakePort {
    responses: Mutex<VecDeque<Result<ReadEnvelope<Value>, AgentPortError>>>,
    calls: Mutex<Vec<RecordedCall>>,
    identity_calls: AtomicUsize,
}

impl FakePort {
    fn with_responses(
        responses: impl IntoIterator<Item = Result<ReadEnvelope<Value>, AgentPortError>>,
    ) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
            identity_calls: AtomicUsize::new(0),
        }
    }
}

impl AgentToolPort<TestCancellation> for FakePort {
    fn resolve_identity(
        &self,
        _request: AgentIdentityRequest,
        _context: AgentResolutionContext<TestCancellation>,
    ) -> AgentPortFuture<Result<AgentResolvedIdentity, AgentPortError>> {
        self.identity_calls.fetch_add(1, Ordering::Relaxed);
        Box::pin(async {
            Ok(AgentResolvedIdentity {
                repository: ResolvedRepository {
                    repository_id: repository(),
                    display_name: "fixture".to_owned(),
                },
                generation: GenerationSummary {
                    generation_id: generation(2),
                    parent_generation: RequiredNullable(None),
                    structural_freshness: Freshness::Current,
                    semantic_freshness: Freshness::Current,
                },
                coverage: CoverageSummary {
                    status: CoverageStatus::Bounded,
                    languages: Vec::new(),
                    skipped_inputs: 0,
                },
                warnings: Vec::new(),
            })
        })
    }

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
                pinned_generation: context
                    .pinned_identity()
                    .map(|identity| identity.generation.generation_id),
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

fn symbol(byte: u8) -> SymbolId {
    SymbolId::from_bytes([byte; 20])
}

fn source_ref(generation: GenerationId) -> SourceRef {
    SourceRef::new(
        repository(),
        generation,
        SourceSpan::new(FileId::from_bytes([4; 20]), 0, 32).expect("fixture source span is valid"),
        ContentHash::from_bytes([5; 32]),
        None,
    )
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
        completeness: rootlight_mcp_contract::completeness::ResultCompleteness::complete(),
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
        PublicError::builder(ErrorCode::Internal, "child failed")
            .build()
            .expect("static error is valid"),
        PublicError::builder(ErrorCode::BudgetExceeded, "budget exceeded")
            .build()
            .expect("static error is valid"),
    )
}

#[test]
fn typed_scalar_bindings_resolve_and_record_exact_destinations() {
    let mut arguments = Map::new();
    arguments.insert(
        "from".to_owned(),
        json!({
            "symbol_id": {"$from": "find", "pointer": "/data/matches/0/symbol_id"}
        }),
    );
    arguments.insert(
        "to".to_owned(),
        json!({
            "symbol_id": {"$from": "find", "pointer": "/data/matches/0/symbol_id"}
        }),
    );
    let request = input(
        vec![
            operation("find", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "refine",
                BatchTool::FlowTrace,
                arguments,
                Some(vec!["find"]),
                None,
            ),
        ],
        budget(500),
    );
    let envelopes = vec![
        Some(response(
            generation(2),
            100,
            json!({"matches": [{"symbol_id": symbol(3)}]}),
        )),
        None,
    ];

    let resolved = resolve_arguments(
        &request.operations[1],
        &envelopes,
        &request,
        &[0],
        repository(),
        generation(2),
    )
    .expect("typed bindings resolve from the dependency data value");
    assert_eq!(
        resolved.materialized_binding_paths,
        ["/from/symbol_id", "/to/symbol_id"]
    );
    assert_eq!(resolved.arguments["from"]["symbol_id"], json!(symbol(3)));
    assert_eq!(resolved.arguments["to"]["symbol_id"], json!(symbol(3)));
}

#[test]
fn runtime_binding_values_enforce_type_cardinality_and_identity() {
    let cases = [
        (
            BatchTool::CodeLocate,
            json!({"matches": [{"symbol_id": 7}]}),
            BatchTool::FlowTrace,
            json!({
                "from": {
                    "symbol_id": {
                        "$from": "source",
                        "pointer": "/data/matches/0/symbol_id"
                    }
                }
            }),
            BatchExecutionError::BindingTypeMismatch,
        ),
        (
            BatchTool::FlowTrace,
            json!({"paths": [{"nodes": [symbol(3)]}]}),
            BatchTool::SymbolExplain,
            json!({
                "symbol_ids": {
                    "$from": "source",
                    "pointer": "/data/paths/0/nodes"
                }
            }),
            BatchExecutionError::BindingCardinalityMismatch,
        ),
        (
            BatchTool::SymbolExplain,
            json!({"symbols": [{"definition": source_ref(generation(3))}]}),
            BatchTool::SourceRead,
            json!({
                "references": [{
                    "source_ref": {
                        "$from": "source",
                        "pointer": "/data/symbols/0/definition"
                    }
                }]
            }),
            BatchExecutionError::BindingIdentityMismatch,
        ),
    ];

    for (source_tool, source_data, target_tool, arguments, expected) in cases {
        let Value::Object(arguments) = arguments else {
            panic!("fixture arguments are objects");
        };
        let request = input(
            vec![
                operation("source", source_tool, Map::new(), None, None),
                operation("target", target_tool, arguments, Some(vec!["source"]), None),
            ],
            budget(500),
        );
        let envelopes = vec![Some(response(generation(2), 100, source_data)), None];

        assert_eq!(
            resolve_arguments(
                &request.operations[1],
                &envelopes,
                &request,
                &[0],
                repository(),
                generation(2),
            ),
            Err(expected)
        );
    }
}

#[test]
fn missing_optional_and_empty_collection_are_distinct() {
    let binding = json!({
        "$from": "source",
        "pointer": "/data/plan/0/targets"
    });
    let Value::Object(empty_arguments) = json!({
        "seeds": {"symbols": binding}
    }) else {
        panic!("fixture arguments are objects");
    };
    let empty_request = input(
        vec![
            operation("source", BatchTool::PlanChange, Map::new(), None, None),
            operation(
                "target",
                BatchTool::ContextPack,
                empty_arguments,
                Some(vec!["source"]),
                None,
            ),
        ],
        budget(500),
    );
    let empty_envelopes = vec![
        Some(response(
            generation(2),
            100,
            json!({"plan": [{"targets": []}]}),
        )),
        None,
    ];
    let empty = resolve_arguments(
        &empty_request.operations[1],
        &empty_envelopes,
        &empty_request,
        &[0],
        repository(),
        generation(2),
    )
    .expect("an explicitly empty collection is a valid zero-cardinality seed");
    assert_eq!(empty.arguments["seeds"]["symbols"], json!([]));

    let Value::Object(missing_arguments) = json!({
        "from": {
            "symbol_id": {
                "$from": "source",
                "pointer": "/data/matches/0/symbol_id"
            }
        }
    }) else {
        panic!("fixture arguments are objects");
    };
    let missing_request = input(
        vec![
            operation("source", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "target",
                BatchTool::FlowTrace,
                missing_arguments,
                Some(vec!["source"]),
                None,
            ),
        ],
        budget(500),
    );
    let missing_envelopes = vec![
        Some(response(
            generation(2),
            100,
            json!({"matches": [{"symbol_id": null}]}),
        )),
        None,
    ];
    assert_eq!(
        resolve_arguments(
            &missing_request.operations[1],
            &missing_envelopes,
            &missing_request,
            &[0],
            repository(),
            generation(2),
        ),
        Err(BatchExecutionError::MissingBindingValue)
    );
}

#[tokio::test]
async fn service_materializes_bindings_and_propagates_policy() {
    let mut second_arguments = Map::new();
    second_arguments.insert(
        "symbol_ids".to_owned(),
        json!({"$from": "trace", "pointer": "/data/paths/0/nodes"}),
    );
    let input = input(
        vec![
            operation("trace", BatchTool::FlowTrace, Map::new(), None, None),
            operation(
                "explain",
                BatchTool::SymbolExplain,
                second_arguments,
                Some(vec!["trace"]),
                None,
            ),
        ],
        budget(1_000),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(response(
            generation(2),
            100,
            json!({"paths": [{"nodes": [symbol(3), symbol(4)]}]}),
        )),
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
        json!([symbol(3), symbol(4)])
    );
    assert_eq!(calls[0].budget.max_tokens, Some(1_000));
    assert_eq!(calls[1].budget.max_tokens, Some(900));
    assert!(
        calls
            .iter()
            .all(|call| call.pinned_generation == Some(generation(2)))
    );
    assert!(calls.iter().all(|call| call.has_deadline));
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        calls[0].request.clone().into_arguments()["generation"],
        json!(generation(2))
    );
}

#[tokio::test]
async fn omitted_timeout_receives_a_bounded_default_before_dispatch() {
    let mut request = input(
        vec![operation(
            "find",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(500),
    );
    request.budget = None;
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        100,
        json!({"matches": []}),
    ))]));

    BatchService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("default timeout keeps the bounded call admissible");

    let calls = port.calls.lock().expect("call lock is available");
    let timeout = calls[0]
        .budget
        .timeout_ms
        .expect("default timeout propagates");
    assert!((1..=rootlight_agent::batch::DEFAULT_BATCH_TIMEOUT_MS).contains(&timeout));
    assert!(calls[0].has_deadline);
}

#[tokio::test]
async fn reserved_child_control_keys_fail_before_identity_resolution() {
    for (tool, key, value) in [
        (BatchTool::CodeLocate, "repository", Value::Null),
        (BatchTool::CodeLocate, "generation", Value::Null),
        (BatchTool::CodeLocate, "budget", Value::Null),
        (BatchTool::CodeLocate, "cursor", Value::Null),
        (BatchTool::CodeLocate, "response_profile", json!("standard")),
        (BatchTool::ChangeImpact, "profile", json!("standard")),
        (BatchTool::TestsSelect, "profile", json!("evidence")),
    ] {
        let mut arguments = Map::new();
        arguments.insert(key.to_owned(), value);
        let port = Arc::new(FakePort::with_responses([]));
        let result = BatchService
            .execute(
                Arc::clone(&port),
                input(
                    vec![operation("reserved", tool, arguments, None, None)],
                    budget(500),
                ),
                repository(),
                TestCancellation(false),
                errors(),
            )
            .await;
        assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
        assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
        assert!(
            port.calls
                .lock()
                .expect("call lock is available")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn binding_objects_with_extra_keys_fail_before_identity_resolution() {
    let mut arguments = Map::new();
    arguments.insert(
        "query".to_owned(),
        json!({
            "$from": "find",
            "pointer": "/data/matches/0/symbol_id",
            "fallback": "publish"
        }),
    );
    let port = Arc::new(FakePort::with_responses([]));
    let result = BatchService
        .execute(
            Arc::clone(&port),
            input(
                vec![
                    operation("find", BatchTool::CodeLocate, Map::new(), None, None),
                    operation(
                        "invalid",
                        BatchTool::CodeLocate,
                        arguments,
                        Some(vec!["find"]),
                        None,
                    ),
                ],
                budget(500),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await;
    assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
    assert!(
        port.calls
            .lock()
            .expect("call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn malformed_binding_fields_fail_before_identity_resolution() {
    for binding in [
        json!({"$from": "x".repeat(33), "pointer": "/data/matches/0/symbol_id"}),
        json!({"$from": "find!", "pointer": "/data/matches/0/symbol_id"}),
        json!({"$from": "find", "pointer": format!("/data/{}/symbol_id", "0".repeat(1009))}),
        json!({"$from": "find", "pointer": "/warnings/0/code"}),
    ] {
        let mut arguments = Map::new();
        arguments.insert("query".to_owned(), binding);
        let port = Arc::new(FakePort::with_responses([]));
        let result = BatchService
            .execute(
                Arc::clone(&port),
                input(
                    vec![
                        operation("find", BatchTool::CodeLocate, Map::new(), None, None),
                        operation(
                            "invalid",
                            BatchTool::CodeLocate,
                            arguments,
                            Some(vec!["find"]),
                            None,
                        ),
                    ],
                    budget(500),
                ),
                repository(),
                TestCancellation(false),
                errors(),
            )
            .await;
        assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
        assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
        assert!(
            port.calls
                .lock()
                .expect("call lock is available")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn malformed_operation_and_dependency_ids_fail_before_identity_resolution() {
    for operations in [
        vec![operation(
            "invalid!",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        vec![operation(
            "valid",
            BatchTool::CodeLocate,
            Map::new(),
            Some(vec!["invalid!"]),
            None,
        )],
    ] {
        let port = Arc::new(FakePort::with_responses([]));
        let result = BatchService
            .execute(
                Arc::clone(&port),
                input(operations, budget(500)),
                repository(),
                TestCancellation(false),
                errors(),
            )
            .await;
        assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
        assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
        assert!(
            port.calls
                .lock()
                .expect("call lock is available")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn ordinary_pointer_keys_are_not_treated_as_bindings() {
    let mut arguments = Map::new();
    arguments.insert("query".to_owned(), json!({"pointer": "ordinary value"}));
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        100,
        json!({}),
    ))]));

    BatchService
        .execute(
            Arc::clone(&port),
            input(
                vec![operation(
                    "ordinary",
                    BatchTool::CodeLocate,
                    arguments,
                    None,
                    None,
                )],
                budget(500),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("an ordinary nested pointer field is not binding syntax");
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 1);
    assert_eq!(port.calls.lock().expect("call lock is available").len(), 1);
}

#[tokio::test]
async fn explicit_generation_mismatch_stops_before_child_dispatch() {
    let mut request = input(
        vec![operation(
            "find",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(500),
    );
    request.generation = Some(GenerationSelector::Explicit(generation(9)));
    let port = Arc::new(FakePort::with_responses([]));

    assert_eq!(
        BatchService
            .execute(
                Arc::clone(&port),
                request,
                repository(),
                TestCancellation(false),
                errors(),
            )
            .await,
        Err(BatchOrchestrationError::InvalidResponse)
    );
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 1);
    assert!(
        port.calls
            .lock()
            .expect("call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn max_results_overrun_is_preserved_as_an_operation_result() {
    let mut request = input(
        vec![operation(
            "find",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(500),
    );
    request.budget.as_mut().expect("test budget").max_results = Some(1);
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        100,
        json!({"matches": [{}, {}]}),
    ))]));

    let output = BatchService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("a child overrun remains in the ordered batch result");

    assert_eq!(output.data.batch_status, BatchStatus::Error);
    assert_eq!(
        output.data.operation_results[0]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::BudgetExceeded)
    );
    assert_eq!(port.calls.lock().expect("call lock").len(), 1);
}

#[tokio::test]
async fn relationship_results_charge_returned_edges_and_propagate_the_remainder() {
    let mut request = input(
        vec![
            operation(
                "first",
                BatchTool::SymbolRelationships,
                Map::new(),
                None,
                None,
            ),
            operation(
                "second",
                BatchTool::SymbolRelationships,
                Map::new(),
                None,
                None,
            ),
        ],
        budget(500),
    );
    request.budget.as_mut().expect("test budget").max_results = Some(5);
    let port = Arc::new(FakePort::with_responses([
        Ok(response(
            generation(2),
            100,
            json!({"groups": [{}], "totals": {"returned_edges": 4}}),
        )),
        Ok(response(
            generation(2),
            100,
            json!({"groups": [{}], "totals": {"returned_edges": 1}}),
        )),
    ]));

    BatchService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("five returned edges fit the aggregate budget");
    let calls = port.calls.lock().expect("call lock is available");
    assert_eq!(calls[0].budget.max_results, Some(5));
    assert_eq!(calls[1].budget.max_results, Some(1));
}

#[tokio::test]
async fn malformed_relationship_accounting_fails_closed() {
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        100,
        json!({"groups": [{}], "totals": {}}),
    ))]));
    assert_eq!(
        BatchService
            .execute(
                port,
                input(
                    vec![operation(
                        "relationships",
                        BatchTool::SymbolRelationships,
                        Map::new(),
                        None,
                        None,
                    )],
                    budget(500),
                ),
                repository(),
                TestCancellation(false),
                errors(),
            )
            .await,
        Err(BatchOrchestrationError::InvalidResponse)
    );
}

#[tokio::test]
async fn local_timeout_is_a_per_operation_budget_error_after_prior_success() {
    let local = ResponseBudget {
        max_results: None,
        max_tokens: None,
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: Some(100),
        evidence_level: None,
    };
    let port = Arc::new(FakePort::with_responses([
        Ok(response(
            generation(2),
            100,
            json!({"matches": [{"symbol_id": "first"}]}),
        )),
        Err(AgentPortError::LocalDeadlineExceeded),
    ]));
    let output = BatchService
        .execute(
            Arc::clone(&port),
            input(
                vec![
                    operation("first", BatchTool::CodeLocate, Map::new(), None, None),
                    operation(
                        "second",
                        BatchTool::CodeLocate,
                        Map::new(),
                        Some(vec!["first"]),
                        Some(local),
                    ),
                ],
                budget(500),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("a child-local timeout remains inside the batch envelope");
    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output.data.operation_results[0].status,
        BatchOperationStatus::Ok
    );
    assert_eq!(
        output.data.operation_results[1]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::BudgetExceeded)
    );
    assert_eq!(port.calls.lock().expect("call lock is available").len(), 2);
}

#[tokio::test]
async fn bindings_cannot_read_warnings_or_envelope_metadata_before_identity_resolution() {
    let mut metadata_arguments = Map::new();
    metadata_arguments.insert(
        "query".to_owned(),
        json!({"$from": "find", "pointer": "/warnings/0/code"}),
    );
    let mut nested_arguments = Map::new();
    nested_arguments.insert(
        "query".to_owned(),
        json!({"$from": "find", "pointer": "/data/warnings/0/symbol_id"}),
    );
    let request = input(
        vec![
            operation("find", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "metadata",
                BatchTool::CodeLocate,
                metadata_arguments,
                Some(vec!["find"]),
                None,
            ),
            operation(
                "nested",
                BatchTool::CodeLocate,
                nested_arguments,
                Some(vec!["find"]),
                None,
            ),
        ],
        budget(500),
    );
    let port = Arc::new(FakePort::with_responses([]));
    let result = BatchService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await;
    assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
    assert!(port.calls.lock().expect("call lock").is_empty());
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
async fn child_reservations_release_unused_capacity_and_reconcile_measured_use() {
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

    let output = BatchService
        .execute(
            Arc::clone(&port),
            input,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("a child overrun remains a structured batch result");

    assert_eq!(output.data.batch_status, BatchStatus::Partial);
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .filter(|result| result.status == BatchOperationStatus::Ok)
            .count(),
        1
    );
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .filter(|result| result.status == BatchOperationStatus::Error)
            .count(),
        1
    );
    let calls = port.calls.lock().expect("call lock");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].budget.max_tokens, Some(250));
    assert_eq!(calls[1].budget.max_tokens, Some(150));
}

#[tokio::test]
async fn exhausted_parent_capacity_prevents_later_child_dispatch() {
    let input = input(
        vec![
            operation("later", BatchTool::CodeLocate, Map::new(), None, None),
            operation("overrun", BatchTool::SourceRead, Map::new(), None, None),
        ],
        budget(100),
    );
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        200,
        json!({}),
    ))]));

    let output = BatchService
        .execute(
            Arc::clone(&port),
            input,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("budget exhaustion remains in the ordered batch result");

    assert_eq!(output.data.batch_status, BatchStatus::Error);
    assert!(
        output.data.operation_results.iter().all(|result| result
            .error
            .as_ref()
            .map(PublicError::code)
            == Some(ErrorCode::BudgetExceeded))
    );
    assert_eq!(port.calls.lock().expect("call lock").len(), 1);
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

    let output = BatchService
        .execute(port, input, repository(), TestCancellation(false), errors())
        .await
        .expect("pinned identity permits a complete all-error batch envelope");
    assert_eq!(output.data.batch_status, BatchStatus::Error);
    assert_eq!(output.data.operation_results.len(), 1);
    assert_eq!(
        output.data.operation_results[0]
            .error
            .as_ref()
            .map(PublicError::code),
        Some(ErrorCode::UnsupportedCapability)
    );
}

#[tokio::test]
async fn incompatible_binding_types_fail_before_identity_resolution() {
    let mut arguments = Map::new();
    arguments.insert(
        "search_modes".to_owned(),
        json!({"$from": "find", "pointer": "/data/matches/0/symbol_id"}),
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
    let port = Arc::new(FakePort::with_responses([]));

    let result = BatchService
        .execute(
            Arc::clone(&port),
            input,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await;
    assert_eq!(result, Err(BatchOrchestrationError::InvalidArguments));
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 0);
    assert!(port.calls.lock().expect("call lock").is_empty());
}
