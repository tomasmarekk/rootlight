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
use rootlight_ir::{CoverageStatus, LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, SchemaVersion, TrustClassification,
    context::{
        BatchArguments, BatchOperation, BatchOperationStatus, BatchStatus, BatchTool,
        FailurePolicy, QueryBatchData, QueryBatchInput,
    },
    vertical::{
        CacheStatus, CoverageSummary, Freshness, GenerationSelector, GenerationSummary,
        ReadEnvelope, RepositoryIdSelector, RequiredNullable, ResolvedRepository, ResponseBudget,
        ResponseProfile, UsageSummary,
    },
};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

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
    response_profile: ResponseProfile,
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
        Box::pin(async { Ok(resolved_identity()) })
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
                response_profile: context.response_profile(),
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

#[derive(Debug, Default)]
struct PendingFirstPort {
    calls: AtomicUsize,
    first_started: Arc<Notify>,
    release_first: Arc<Notify>,
}

impl AgentToolPort<TestCancellation> for PendingFirstPort {
    fn resolve_identity(
        &self,
        _request: AgentIdentityRequest,
        _context: AgentResolutionContext<TestCancellation>,
    ) -> AgentPortFuture<Result<AgentResolvedIdentity, AgentPortError>> {
        Box::pin(async { Ok(resolved_identity()) })
    }

    fn execute(
        &self,
        _request: AgentToolRequest,
        _context: AgentCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.first_started.notify_one();
            let release = Arc::clone(&self.release_first);
            Box::pin(async move {
                release.notified().await;
                Ok(typed_response(
                    BatchTool::CodeLocate,
                    generation(2),
                    10,
                    json!({"matches": []}),
                ))
            })
        } else {
            Box::pin(async {
                Ok(typed_response(
                    BatchTool::CodeLocate,
                    generation(2),
                    10,
                    json!({"matches": []}),
                ))
            })
        }
    }
}

fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn resolved_identity() -> AgentResolvedIdentity {
    AgentResolvedIdentity {
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
    }
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

fn source_ref_with_line(generation: GenerationId) -> SourceRef {
    SourceRef::new(
        repository(),
        generation,
        SourceSpan::new(FileId::from_bytes([4; 20]), 0, 32).expect("fixture source span is valid"),
        ContentHash::from_bytes([5; 32]),
        Some(LineRange::new(1, 2).expect("fixture line range is valid")),
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

fn typed_response(
    tool: BatchTool,
    generation: GenerationId,
    tokens: u64,
    data: Value,
) -> ReadEnvelope<Value> {
    response(generation, tokens, typed_data(tool, data))
}

fn typed_data(tool: BatchTool, data: Value) -> Value {
    let mut data = data.as_object().cloned().unwrap_or_default();
    match tool {
        BatchTool::CodeLocate => {
            let matches = data
                .entry("matches")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("code.locate matches fixture is an array");
            for (index, item) in matches.iter_mut().enumerate() {
                let item = item
                    .as_object_mut()
                    .expect("code.locate match fixture is an object");
                item.entry("symbol_id").or_insert(Value::Null);
                item.entry("file_id").or_insert(Value::Null);
                item.entry("kind").or_insert_with(|| json!("function"));
                item.entry("display_name")
                    .or_insert_with(|| json!(format!("match-{index}")));
                item.entry("signature").or_insert(Value::Null);
                item.entry("path")
                    .or_insert_with(|| json!(format!("src/match_{index}.rs")));
                item.entry("score").or_insert_with(|| json!(900));
                item.entry("why")
                    .or_insert_with(|| json!(["identifier_match"]));
                item.entry("source_ref").or_insert(Value::Null);
                item.entry("trust")
                    .or_insert_with(|| json!("untrusted_repository_data"));
            }
            data.entry("query_interpretation").or_insert_with(|| {
                json!({
                    "tokens": [],
                    "modes": ["exact"],
                    "semantic_available": false
                })
            });
            data.entry("suggested_next").or_insert_with(|| json!([]));
        }
        BatchTool::SymbolExplain => {
            data.entry("symbols").or_insert_with(|| json!([]));
            data.entry("unresolved_ids").or_insert_with(|| json!([]));
            data.entry("detail_handles").or_insert_with(|| json!([]));
        }
        BatchTool::FlowTrace => {
            let paths = data
                .entry("paths")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("flow.trace paths fixture is an array");
            for path in paths {
                let path = path
                    .as_object_mut()
                    .expect("flow.trace path fixture is an object");
                path.entry("confidence").or_insert_with(|| json!(900));
                path.entry("edges").or_insert_with(|| json!([]));
                path.entry("cyclic").or_insert_with(|| json!(false));
            }
            data.entry("frontier").or_insert_with(|| {
                json!({
                    "reached_nodes": 0,
                    "examined_edges": 0,
                    "truncated": false,
                    "unresolved_boundaries": 0
                })
            });
            data.entry("projection").or_insert_with(|| {
                json!({
                    "relations": ["calls"],
                    "min_confidence": 0
                })
            });
        }
        BatchTool::SymbolRelationships => {
            let returned_edges = data
                .get("totals")
                .and_then(|totals| totals.get("returned_edges"))
                .and_then(Value::as_u64);
            if let Some(returned_edges) = returned_edges {
                let groups = data
                    .entry("groups")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .expect("relationship groups fixture is an array");
                for group in groups {
                    let group = group
                        .as_object_mut()
                        .expect("relationship group fixture is an object");
                    group.entry("seed").or_insert_with(|| json!(symbol(3)));
                    group.entry("relation").or_insert_with(|| json!("calls"));
                    group
                        .entry("direction")
                        .or_insert_with(|| json!("outbound"));
                    group.entry("items").or_insert_with(|| json!([]));
                    group
                        .entry("total_count")
                        .or_insert_with(|| json!(returned_edges));
                }
                let totals = data
                    .get_mut("totals")
                    .and_then(Value::as_object_mut)
                    .expect("relationship totals fixture is an object");
                totals
                    .entry("total_edges")
                    .or_insert_with(|| json!(returned_edges));
                totals.entry("exact").or_insert_with(|| json!(true));
                data.entry("unresolved").or_insert_with(|| json!([]));
            }
        }
        BatchTool::ChangeImpact
        | BatchTool::TestsSelect
        | BatchTool::ArchitectureOverview
        | BatchTool::ArchitectureCycles
        | BatchTool::CodeDead
        | BatchTool::PlanChange
        | BatchTool::ContextPack
        | BatchTool::SourceRead => {}
    }
    Value::Object(data)
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
        arguments: arguments
            .try_into()
            .expect("test operation arguments contain valid bindings"),
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

fn unavailable_error() -> AgentPortError {
    AgentPortError::Public(Box::new(
        PublicError::builder(
            ErrorCode::UnsupportedCapability,
            "capability is unavailable",
        )
        .build()
        .expect("static error is valid"),
    ))
}

fn ordered_outcome_snapshot(output: &ReadEnvelope<QueryBatchData>) -> Value {
    json!({
        "batch_status": output.data.batch_status,
        "operation_results": output
            .data
            .operation_results
            .iter()
            .map(|result| json!({
                "id": result.id,
                "tool": result.tool,
                "status": result.status,
                "error_code": result.error.as_ref().map(PublicError::code)
            }))
            .collect::<Vec<_>>()
    })
}

#[test]
fn typed_scalar_bindings_resolve_and_record_exact_destinations() {
    let mut arguments = Map::new();
    arguments.insert(
        "from".to_owned(),
        json!({
            "symbol_id": {"$from": "find", "source": "symbol_id", "index": 0}
        }),
    );
    arguments.insert(
        "to".to_owned(),
        json!({
            "symbol_id": {"$from": "find", "source": "symbol_id", "index": 0}
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
                        "source": "symbol_id",
                        "index": 0
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
                    "source": "nodes",
                    "index": 0
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
                        "source": "definition",
                        "index": 0
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
        "source": "symbol_ids",
        "index": 0
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
                "source": "symbol_id",
                "index": 0
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
        json!({"$from": "trace", "source": "nodes", "index": 0}),
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
        Ok(typed_response(
            BatchTool::FlowTrace,
            generation(2),
            100,
            json!({"paths": [{"nodes": [symbol(3), symbol(4)]}]}),
        )),
        Ok(typed_response(
            BatchTool::SymbolExplain,
            generation(2),
            100,
            json!({"symbols": []}),
        )),
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
    let first_tokens = calls[0]
        .budget
        .max_tokens
        .expect("publication reservation leaves a bounded child budget");
    assert!(first_tokens < 1_000);
    assert_eq!(calls[1].budget.max_tokens, Some(first_tokens - 100));
    assert!(
        calls
            .iter()
            .all(|call| call.pinned_generation == Some(generation(2)))
    );
    assert!(calls.iter().all(|call| call.has_deadline));
    assert!(
        calls
            .iter()
            .all(|call| call.response_profile == ResponseProfile::Compact)
    );
    assert_eq!(port.identity_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        calls[0].request.clone().into_arguments()["generation"],
        json!(generation(2))
    );
}

#[tokio::test]
async fn batch_bindings_use_canonical_data_while_public_results_are_compact() {
    let evidence = source_ref_with_line(generation(2));
    let Value::Object(arguments) = json!({
        "references": [{
            "source_ref": {
                "$from": "find",
                "source": "source_ref",
                "index": 0
            }
        }]
    }) else {
        panic!("fixture arguments are objects");
    };
    let port = Arc::new(FakePort::with_responses([
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            100,
            json!({
                "matches": [{
                    "symbol_id": symbol(3),
                    "signature": "fn profile_target()",
                    "why": [
                        "identifier_match",
                        "lexical_match",
                        "docs_match"
                    ],
                    "source_ref": evidence
                }]
            }),
        )),
        Ok(response(
            generation(2),
            100,
            json!({"chunks": [], "elisions": [], "stale_references": [], "total_source_bytes": 0}),
        )),
    ]));

    let output = BatchService
        .execute(
            Arc::clone(&port),
            input(
                vec![
                    operation("find", BatchTool::CodeLocate, Map::new(), None, None),
                    operation(
                        "read",
                        BatchTool::SourceRead,
                        arguments,
                        Some(vec!["find"]),
                        None,
                    ),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("canonical binding and compact publication both succeed");

    let public_match = &output.data.operation_results[0]
        .data
        .as_ref()
        .expect("public child data")["matches"][0];
    assert_eq!(public_match["signature"], Value::Null);
    assert_eq!(public_match["why"], json!(["identifier_match"]));
    assert_eq!(public_match["source_ref"]["line_hint"], Value::Null);

    let calls = port.calls.lock().expect("call lock is available");
    let bound_reference = &calls[1].request.clone().into_arguments()["references"][0]["source_ref"];
    assert_eq!(
        bound_reference["line_hint"],
        json!({"start_line": 1, "end_line": 2})
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
    let port = Arc::new(FakePort::with_responses([Ok(typed_response(
        BatchTool::CodeLocate,
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

#[test]
fn binding_objects_with_extra_keys_fail_at_the_contract_boundary() {
    let mut arguments = Map::new();
    arguments.insert(
        "query".to_owned(),
        json!({
            "$from": "find",
            "source": "symbol_id",
            "index": 0,
            "fallback": "publish"
        }),
    );
    assert!(BatchArguments::try_from(arguments).is_err());
}

#[test]
fn malformed_binding_fields_fail_at_the_contract_boundary() {
    for binding in [
        json!({"$from": "x".repeat(33), "source": "symbol_id", "index": 0}),
        json!({"$from": "find!", "source": "symbol_id", "index": 0}),
        json!({"$from": "find", "source": "symbol_id", "index": 500}),
        json!({"$from": "find", "source": "warnings", "index": 0}),
    ] {
        let mut arguments = Map::new();
        arguments.insert("query".to_owned(), binding);
        assert!(BatchArguments::try_from(arguments).is_err());
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
    let port = Arc::new(FakePort::with_responses([Ok(typed_response(
        BatchTool::CodeLocate,
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
    let port = Arc::new(FakePort::with_responses([Ok(typed_response(
        BatchTool::CodeLocate,
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
        budget(900),
    );
    request.budget.as_mut().expect("test budget").max_results = Some(5);
    let port = Arc::new(FakePort::with_responses([
        Ok(typed_response(
            BatchTool::SymbolRelationships,
            generation(2),
            100,
            json!({"groups": [{}], "totals": {"returned_edges": 4}}),
        )),
        Ok(typed_response(
            BatchTool::SymbolRelationships,
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
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            100,
            json!({"matches": [{"symbol_id": symbol(3)}]}),
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
async fn unregistered_source_selections_fail_before_identity_resolution() {
    let mut metadata_arguments = Map::new();
    metadata_arguments.insert(
        "query".to_owned(),
        json!({"$from": "find", "source": "test_id", "index": 0}),
    );
    let mut nested_arguments = Map::new();
    nested_arguments.insert(
        "query".to_owned(),
        json!({"$from": "find", "source": "pack_id"}),
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
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            100,
            json!({"matches": []}),
        )),
        Ok(response(
            generation(2),
            200,
            json!({
                "chunks": [],
                "elisions": [],
                "stale_references": [],
                "total_source_bytes": 0
            }),
        )),
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
            operation("second", BatchTool::CodeLocate, Map::new(), None, None),
        ],
        budget(650),
    );
    let port = Arc::new(FakePort::with_responses([
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            100,
            json!({}),
        )),
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            200,
            json!({}),
        )),
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
    let first_tokens = calls[0]
        .budget
        .max_tokens
        .expect("publication reservation leaves child capacity");
    assert_eq!(calls[1].budget.max_tokens, Some(first_tokens - 100));
}

#[tokio::test]
async fn exhausted_parent_capacity_prevents_later_child_dispatch() {
    let input = input(
        vec![
            operation("later", BatchTool::CodeLocate, Map::new(), None, None),
            operation("overrun", BatchTool::SourceRead, Map::new(), None, None),
        ],
        budget(500),
    );
    let port = Arc::new(FakePort::with_responses([Ok(response(
        generation(2),
        200,
        json!({
            "chunks": [],
            "elisions": [],
            "stale_references": [],
            "total_source_bytes": 0
        }),
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
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .filter(|result| result.status == BatchOperationStatus::Error)
            .count(),
        1
    );
    assert_eq!(
        output
            .data
            .operation_results
            .iter()
            .filter(|result| result.status == BatchOperationStatus::NotRunBudget)
            .count(),
        1
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
async fn ordered_terminal_outcomes_match_the_versioned_golden() {
    let successful = || {
        Ok(typed_response(
            BatchTool::CodeLocate,
            generation(2),
            10,
            json!({"matches": []}),
        ))
    };
    let mut observed = Map::new();

    let all_success = BatchService
        .execute(
            Arc::new(FakePort::with_responses([successful(), successful()])),
            input(
                vec![
                    operation("first", BatchTool::CodeLocate, Map::new(), None, None),
                    operation("second", BatchTool::CodeLocate, Map::new(), None, None),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("the all-success case produces a complete envelope");
    observed.insert(
        "all_success".to_owned(),
        ordered_outcome_snapshot(&all_success),
    );

    let mixed = BatchService
        .execute(
            Arc::new(FakePort::with_responses([
                successful(),
                Err(unavailable_error()),
                successful(),
            ])),
            input(
                vec![
                    operation("success", BatchTool::CodeLocate, Map::new(), None, None),
                    operation(
                        "failure",
                        BatchTool::SymbolRelationships,
                        Map::new(),
                        None,
                        None,
                    ),
                    operation(
                        "later_success",
                        BatchTool::CodeLocate,
                        Map::new(),
                        None,
                        None,
                    ),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("continue-independent preserves mixed outcomes");
    observed.insert("mixed".to_owned(), ordered_outcome_snapshot(&mixed));

    let all_error = BatchService
        .execute(
            Arc::new(FakePort::with_responses([
                Err(unavailable_error()),
                Err(unavailable_error()),
            ])),
            input(
                vec![
                    operation(
                        "first_failure",
                        BatchTool::SymbolRelationships,
                        Map::new(),
                        None,
                        None,
                    ),
                    operation(
                        "second_failure",
                        BatchTool::SymbolRelationships,
                        Map::new(),
                        None,
                        None,
                    ),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("all child failures remain in the batch envelope");
    observed.insert("all_error".to_owned(), ordered_outcome_snapshot(&all_error));

    let mut fail_fast_request = input(
        vec![
            operation("not_started", BatchTool::CodeLocate, Map::new(), None, None),
            operation(
                "failure",
                BatchTool::SymbolRelationships,
                Map::new(),
                None,
                None,
            ),
        ],
        budget(1_000),
    );
    fail_fast_request.failure_policy = Some(FailurePolicy::FailFast);
    let fail_fast = BatchService
        .execute(
            Arc::new(FakePort::with_responses([Err(unavailable_error())])),
            fail_fast_request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("fail-fast fills every request-order result slot");
    observed.insert("fail_fast".to_owned(), ordered_outcome_snapshot(&fail_fast));

    let dependency_skip = BatchService
        .execute(
            Arc::new(FakePort::with_responses([Err(unavailable_error())])),
            input(
                vec![
                    operation(
                        "dependent",
                        BatchTool::CodeLocate,
                        Map::new(),
                        Some(vec!["failure"]),
                        None,
                    ),
                    operation(
                        "failure",
                        BatchTool::SymbolRelationships,
                        Map::new(),
                        None,
                        None,
                    ),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("a dependency failure remains distinct from fail-fast");
    observed.insert(
        "dependency_skip".to_owned(),
        ordered_outcome_snapshot(&dependency_skip),
    );

    let budget_exhaustion = BatchService
        .execute(
            Arc::new(FakePort::with_responses([Ok(response(
                generation(2),
                200,
                json!({
                    "chunks": [],
                    "elisions": [],
                    "stale_references": [],
                    "total_source_bytes": 0
                }),
            ))])),
            input(
                vec![
                    operation("later", BatchTool::CodeLocate, Map::new(), None, None),
                    operation("overrun", BatchTool::SourceRead, Map::new(), None, None),
                ],
                budget(500),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("budget exhaustion preserves completed and unstarted slots");
    observed.insert(
        "budget_exhaustion".to_owned(),
        ordered_outcome_snapshot(&budget_exhaustion),
    );

    let cancellation = BatchService
        .execute(
            Arc::new(FakePort::with_responses([Err(AgentPortError::Cancelled)])),
            input(
                vec![
                    operation("first", BatchTool::CodeLocate, Map::new(), None, None),
                    operation("second", BatchTool::CodeLocate, Map::new(), None, None),
                ],
                budget(1_000),
            ),
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("runtime cancellation preserves every request-order result slot");
    observed.insert(
        "cancellation".to_owned(),
        ordered_outcome_snapshot(&cancellation),
    );

    let golden: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp/batch-operation-status-goldens-v1.json"
    ))
    .expect("batch operation status golden is valid JSON");
    assert_eq!(
        golden["$schema"],
        "rootlight.query-batch-operation-status-goldens/1"
    );
    assert_eq!(Value::Object(observed), golden["cases"]);
}

#[tokio::test]
async fn selectable_profile_reaches_every_child_call() {
    let mut request = input(
        vec![operation(
            "find",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(1_000),
    );
    request.response_profile = Some(ResponseProfile::Standard);
    let port = Arc::new(FakePort::with_responses([Ok(typed_response(
        BatchTool::CodeLocate,
        generation(2),
        10,
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
        .expect("selectable profile is admitted");

    assert_eq!(
        port.calls.lock().expect("call lock")[0].response_profile,
        ResponseProfile::Standard
    );
}

#[tokio::test]
async fn measured_child_failure_reconciles_operation_and_aggregate_usage() {
    let measured = UsageSummary {
        rows: 7,
        edges: 3,
        source_bytes: 11,
        json_bytes: 0,
        estimated_tokens: 0,
        wall_time_ms: 13,
        cache_status: CacheStatus::Miss,
        trace_id: "measured-failure".to_owned(),
    };
    let request = input(
        vec![operation(
            "find",
            BatchTool::CodeLocate,
            Map::new(),
            None,
            None,
        )],
        budget(1_000),
    );
    let port = Arc::new(FakePort::with_responses([Err(
        AgentPortError::Unavailable.with_usage(measured.clone())
    )]));

    let output = BatchService
        .execute(
            port,
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("measured runtime failure remains in the batch envelope");

    assert_eq!(output.data.operation_results[0].usage, Some(measured));
    assert_eq!(output.usage.rows, 7);
    assert_eq!(output.usage.edges, 3);
    assert_eq!(output.usage.source_bytes, 11);
    assert_eq!(output.usage.wall_time_ms, 13);
}

#[tokio::test]
async fn cancellation_after_dispatch_preserves_all_request_order_slots() {
    let request = input(
        vec![
            operation("first", BatchTool::CodeLocate, Map::new(), None, None),
            operation("second", BatchTool::CodeLocate, Map::new(), None, None),
        ],
        budget(1_000),
    );
    let port = Arc::new(FakePort::with_responses([Err(AgentPortError::Cancelled
        .with_usage(UsageSummary {
            rows: 0,
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 1,
            cache_status: CacheStatus::Miss,
            trace_id: "cancelled".to_owned(),
        }))]));

    let output = BatchService
        .execute(
            port,
            request,
            repository(),
            TestCancellation(false),
            errors(),
        )
        .await
        .expect("accepted-plan cancellation remains a structured envelope");

    assert_eq!(output.data.operation_results.len(), 2);
    assert!(
        output
            .data
            .operation_results
            .iter()
            .all(|result| result.status == BatchOperationStatus::Cancelled)
    );
    assert_eq!(
        output.completeness.state,
        rootlight_mcp_contract::completeness::CompletenessState::Indeterminate
    );
}

#[tokio::test]
async fn pending_first_child_prevents_a_second_reservation_or_dispatch() {
    let request = input(
        vec![
            operation("first", BatchTool::CodeLocate, Map::new(), None, None),
            operation("second", BatchTool::CodeLocate, Map::new(), None, None),
        ],
        budget(1_000),
    );
    let port = Arc::new(PendingFirstPort::default());
    let task = tokio::spawn(BatchService.execute(
        Arc::clone(&port),
        request,
        repository(),
        TestCancellation(false),
        errors(),
    ));

    port.first_started.notified().await;
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    port.release_first.notify_one();
    task.await
        .expect("batch task does not panic")
        .expect("serialized batch completes");
    assert_eq!(port.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn incompatible_binding_types_fail_before_identity_resolution() {
    let mut arguments = Map::new();
    arguments.insert(
        "search_modes".to_owned(),
        json!({"$from": "find", "source": "symbol_id", "index": 0}),
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
