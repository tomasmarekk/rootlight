//! Focused tests for native MCP-to-daemon request and metadata mapping.
//!
//! A typed fake pins exact client calls while the real executor validates the
//! complete five-tool output contract and source-free failure classes.

use std::sync::{Arc, Mutex, OnceLock};

use rootlight_client::{
    AnalysisTier as ClientAnalysisTier, ArchitectureCycles, ClientError, CodeDead,
    CodeDeadEntryPointSummary, CodeLocate, CoverageStatus, CycleProjection, FlowTrace,
    FlowTraceFrontier, FlowTraceProjection, GenerationSelector, LocateMode, OperationKind,
    OperationStage, OperationState, QueryContext, QueryUsage, RecoveryClass,
    RepositoryCoverageEntry, RepositoryIndex, RepositoryList, RepositoryListEntry,
    RepositoryOperationAction, RepositoryOperationStatus, RepositoryStatus, RequestTimeout,
    SourceChunk, SourceRead, SourceReference, SymbolExplain, SymbolRelationships,
};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_mcp_contract::{
    ErrorCode, PublicError, ToolResponse, VerticalTool,
    vertical::{
        CodeLocateOutput, OperationStatusOutput, RepoIndexOutput, SourceReadOutput,
        SymbolExplainOutput,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use super::{
    AsyncClientFuture, AsyncFirstSliceClient, FIRST_SLICE_PROVIDER, NativeFirstSliceClientPort,
    UnavailableFirstSliceClientPort, map_client_error,
};
use crate::{FirstSliceToolExecutor, RequestCancellation, ToolExecutionFailure, ToolExecutor};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    RepositoryIndex {
        root: String,
        operation: OperationId,
        detached: bool,
        timeout: RequestTimeout,
    },
    OperationStatus {
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    },
    CodeLocate {
        repository: RepositoryId,
        generation: GenerationSelector,
        query: String,
        mode: LocateMode,
        maximum_results: u32,
        timeout: RequestTimeout,
    },
    SymbolExplain {
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        timeout: RequestTimeout,
    },
    SourceRead {
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        timeout: RequestTimeout,
    },
    RepositoryList {
        max_results: Option<u32>,
        query: Option<String>,
        timeout: RequestTimeout,
    },
    RepositoryStatus {
        repository: RepositoryId,
        generation: GenerationSelector,
        timeout: RequestTimeout,
    },
    SymbolRelationships {
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        min_confidence: Option<u16>,
        max_results: Option<u16>,
        timeout: RequestTimeout,
    },
    FlowTrace {
        repository: RepositoryId,
        generation: GenerationSelector,
        from: SymbolId,
        to: Option<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        max_depth: Option<u8>,
        max_paths: Option<u16>,
        min_confidence: Option<u16>,
        timeout: RequestTimeout,
    },
    ArchitectureCycles {
        repository: RepositoryId,
        generation: GenerationSelector,
        relations: Vec<String>,
        min_size: Option<u8>,
        max_cycles: Option<u16>,
        include_self_cycles: Option<bool>,
        timeout: RequestTimeout,
    },
    CodeDead {
        repository: RepositoryId,
        generation: GenerationSelector,
        entry_point_policy: Option<String>,
        include_exported: Option<bool>,
        include_tests: Option<bool>,
        min_confidence: Option<u16>,
        max_candidates: Option<u16>,
        timeout: RequestTimeout,
    },
}

#[derive(Clone, Default)]
struct FakeAsyncClient {
    calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeAsyncClient {
    fn record(&self, call: Call) {
        self.calls
            .lock()
            .expect("fake call recorder is not poisoned")
            .push(call);
    }
}

impl AsyncFirstSliceClient for FakeAsyncClient {
    fn repository_index(
        &self,
        root: String,
        operation: OperationId,
        detached: bool,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryIndex> {
        self.record(Call::RepositoryIndex {
            root,
            operation,
            detached,
            timeout,
        });
        Box::pin(async move {
            Ok(RepositoryIndex {
                repository: repository(),
                operation,
                state: OperationState::Succeeded,
                revision: 2,
                parent_generation: Some(parent_generation()),
                published_generation: Some(generation()),
                discovered_inputs: 1,
                indexed_files: 1,
                entities: 1,
                elapsed_micros: 10,
            })
        })
    }

    fn operation_status(
        &self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryOperationStatus> {
        self.record(Call::OperationStatus {
            operation,
            action,
            wait_ms,
            after_revision,
            timeout,
        });
        Box::pin(async move {
            Ok(RepositoryOperationStatus {
                operation: rootlight_client::OperationStatus {
                    operation,
                    state: OperationState::Running,
                    revision: 4,
                    completed_units: 1,
                    total_units: 2,
                    error: None,
                    kind: OperationKind::RepositoryIndex,
                    stage: OperationStage::Executing,
                    plan_hash: [4; 32],
                    detached: true,
                    cancellation_requested: false,
                    deadline_unix_ms: None,
                    lease_expires_unix_ms: None,
                    recovery_class: RecoveryClass::NotApplicable,
                },
                published_generation: None,
                started_unix_ms: 1_700_000_000_000,
                peak_rss_bytes: 0,
                written_bytes: 0,
                files_examined: 1,
                retry_after_ms: Some(10),
            })
        })
    }

    fn code_locate(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: String,
        mode: LocateMode,
        maximum_results: u32,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<CodeLocate> {
        self.record(Call::CodeLocate {
            repository,
            generation,
            query,
            mode,
            maximum_results,
            timeout,
        });
        Box::pin(async move {
            Ok(CodeLocate {
                context: query_context(repository, generation, true),
                hits: Vec::new(),
                matched_candidates: 0,
                truncated: false,
            })
        })
    }

    fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SymbolExplain> {
        self.record(Call::SymbolExplain {
            repository,
            generation,
            symbols: symbols.clone(),
            timeout,
        });
        Box::pin(async move {
            Ok(SymbolExplain {
                context: query_context(repository, generation, false),
                symbols: Vec::new(),
                unresolved_symbols: symbols,
                truncated: false,
            })
        })
    }

    fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SourceRead> {
        self.record(Call::SourceRead {
            repository,
            generation,
            references: references.clone(),
            timeout,
        });
        Box::pin(async move {
            let chunks = references
                .into_iter()
                .map(|source| SourceChunk {
                    source,
                    path: "src/lib.rs".to_owned(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 1,
                    end_line: 1,
                    content: String::new(),
                    content_hash: content_hash(),
                    language: "rust".to_owned(),
                    generated: false,
                })
                .collect();
            Ok(SourceRead {
                context: query_context(repository, generation, true),
                chunks,
                total_source_bytes: 0,
                truncated: false,
            })
        })
    }

    fn repository_list(
        &self,
        max_results: Option<u32>,
        query: Option<String>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryList> {
        self.record(Call::RepositoryList {
            max_results,
            query,
            timeout,
        });
        Box::pin(async move {
            Ok(RepositoryList {
                repositories: vec![RepositoryListEntry {
                    repository_id: repository(),
                    active_generation: generation(),
                    languages: vec!["rust".to_owned()],
                    structural_freshness: "current".to_owned(),
                    semantic_freshness: "current".to_owned(),
                    state: "ready".to_owned(),
                }],
            })
        })
    }

    fn repository_status(
        &self,
        repository: RepositoryId,
        generation_selector: GenerationSelector,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryStatus> {
        self.record(Call::RepositoryStatus {
            repository,
            generation: generation_selector,
            timeout,
        });
        Box::pin(async move {
            Ok(RepositoryStatus {
                repository_id: repository,
                active_generation: generation(),
                parent_generation: Some(parent_generation()),
                structural_freshness: "current".to_owned(),
                semantic_freshness: "current".to_owned(),
                state: "ready".to_owned(),
                coverage: vec![RepositoryCoverageEntry {
                    language: "rust".to_owned(),
                    tier: "tier_a".to_owned(),
                    status: "complete".to_owned(),
                    discovered_files: 1,
                    indexed_files: 1,
                }],
            })
        })
    }

    fn symbol_relationships(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        min_confidence: Option<u16>,
        max_results: Option<u16>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SymbolRelationships> {
        self.record(Call::SymbolRelationships {
            repository,
            generation,
            seeds,
            relations,
            direction,
            min_confidence,
            max_results,
            timeout,
        });
        Box::pin(async move {
            Ok(SymbolRelationships {
                context: query_context(repository, generation, true),
                groups: Vec::new(),
                returned_edges: 0,
                total_edges: 0,
                exact: true,
                truncated: false,
            })
        })
    }

    fn flow_trace(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        from: SymbolId,
        to: Option<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        max_depth: Option<u8>,
        max_paths: Option<u16>,
        min_confidence: Option<u16>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<FlowTrace> {
        self.record(Call::FlowTrace {
            repository,
            generation,
            from,
            to,
            relations: relations.clone(),
            direction,
            max_depth,
            max_paths,
            min_confidence,
            timeout,
        });
        Box::pin(async move {
            Ok(FlowTrace {
                context: query_context(repository, generation, true),
                paths: Vec::new(),
                frontier: FlowTraceFrontier {
                    reached_nodes: 1,
                    examined_edges: 0,
                    truncated: false,
                    unresolved_boundaries: 0,
                },
                projection: FlowTraceProjection {
                    relations,
                    min_confidence: min_confidence.unwrap_or(0),
                },
            })
        })
    }

    fn architecture_cycles(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        relations: Vec<String>,
        min_size: Option<u8>,
        max_cycles: Option<u16>,
        include_self_cycles: Option<bool>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<ArchitectureCycles> {
        self.record(Call::ArchitectureCycles {
            repository,
            generation,
            relations: relations.clone(),
            min_size,
            max_cycles,
            include_self_cycles,
            timeout,
        });
        Box::pin(async move {
            Ok(ArchitectureCycles {
                context: query_context(repository, generation, true),
                components: Vec::new(),
                cycles: Vec::new(),
                break_candidates: Vec::new(),
                projection: CycleProjection {
                    relations,
                    min_confidence: 0,
                },
            })
        })
    }

    fn code_dead(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        entry_point_policy: Option<String>,
        include_exported: Option<bool>,
        include_tests: Option<bool>,
        min_confidence: Option<u16>,
        max_candidates: Option<u16>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<CodeDead> {
        self.record(Call::CodeDead {
            repository,
            generation,
            entry_point_policy,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            timeout,
        });
        Box::pin(async move {
            Ok(CodeDead {
                context: query_context(repository, generation, true),
                candidates: Vec::new(),
                entry_points: CodeDeadEntryPointSummary {
                    policy: "standard".to_owned(),
                    entry_point_count: 0,
                    complete: false,
                },
                blind_spots: Vec::new(),
                false_positive_controls: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn native_port_maps_all_five_calls_without_blocking_adapters() {
    let fake = FakeAsyncClient::default();
    let calls = Arc::clone(&fake.calls);
    let executor = FirstSliceToolExecutor::new(NativeFirstSliceClientPort::with_client(fake))
        .expect("executor initializes");

    let index: RepoIndexOutput = execute(
        &executor,
        VerticalTool::RepoIndex,
        json!({"root": "C:/fixture", "mode": "auto", "detached": true}),
    )
    .await;
    let ToolResponse::Success(index) = index else {
        panic!("index succeeds");
    };
    assert_eq!(index.data.accepted_plan.providers, [FIRST_SLICE_PROVIDER]);
    assert_eq!(index.data.accepted_plan.estimated_disk_bytes, 0);
    assert_eq!(
        index.data.accepted_plan.parent_generation.0,
        Some(parent_generation())
    );
    let indexed_operation = index.data.operation_id;

    let status: OperationStatusOutput = execute(
        &executor,
        VerticalTool::OperationStatus,
        json!({
            "operation_id": operation(),
            "action": "cancel",
            "wait_ms": 20,
            "after_revision": 3
        }),
    )
    .await;
    assert!(matches!(status, ToolResponse::Success(_)));

    let locate: CodeLocateOutput = execute(
        &executor,
        VerticalTool::CodeLocate,
        json!({
            "repository": {"repository_id": repository()},
            "generation": "active",
            "query": "answer",
            "search_modes": ["exact"],
            "max_results": 7
        }),
    )
    .await;
    let ToolResponse::Success(locate) = locate else {
        panic!("locate succeeds");
    };
    assert!(
        locate.generation.structural_freshness
            == rootlight_mcp_contract::vertical::Freshness::Current
    );
    assert!(locate.data.query_interpretation.tokens.is_empty());
    assert_eq!(locate.coverage.languages.len(), 1);
    assert_eq!(locate.coverage.languages[0].language, "rust");
    assert!(locate.usage.trace_id.starts_with("bridge-"));

    let explain: SymbolExplainOutput = execute(
        &executor,
        VerticalTool::SymbolExplain,
        json!({
            "repository": {"repository_id": repository()},
            "generation": parent_generation(),
            "symbol_ids": [symbol()],
            "include_provenance": "none"
        }),
    )
    .await;
    let ToolResponse::Success(explain) = explain else {
        panic!("explain succeeds");
    };
    assert_eq!(
        explain.generation.structural_freshness,
        rootlight_mcp_contract::vertical::Freshness::Superseded
    );
    assert_eq!(explain.coverage.languages.len(), 1);
    assert_eq!(explain.coverage.languages[0].language, "rust");

    let source: SourceReadOutput = execute(
        &executor,
        VerticalTool::SourceRead,
        json!({
            "repository": {"repository_id": repository()},
            "generation": generation(),
            "references": [{
                "source_ref": {
                    "repository": repository(),
                    "generation": generation(),
                    "span": {
                        "file": file(),
                        "start_byte": 0,
                        "end_byte": 0
                    },
                    "content_hash": content_hash(),
                    "line_hint": {
                        "start_line": 1,
                        "end_line": 1
                    }
                }
            }]
        }),
    )
    .await;
    let ToolResponse::Success(source) = source else {
        panic!("source read succeeds");
    };
    assert_eq!(source.coverage.languages.len(), 1);
    assert_eq!(source.coverage.languages[0].language, "rust");

    let calls = calls.lock().expect("fake call recorder is not poisoned");
    assert_eq!(calls.len(), 5);
    let Call::RepositoryIndex {
        root,
        operation: first_operation,
        detached,
        ..
    } = &calls[0]
    else {
        panic!("first call is repository index");
    };
    assert_eq!(root, "C:/fixture");
    assert!(*detached);
    assert_eq!(*first_operation, indexed_operation);
    assert!(matches!(
        &calls[1],
        Call::OperationStatus {
            operation: observed,
            action: RepositoryOperationAction::Cancel,
            wait_ms: Some(20),
            after_revision: Some(3),
            ..
        } if *observed == operation()
    ));
    assert!(matches!(
        &calls[2],
        Call::CodeLocate {
            repository: observed,
            generation: GenerationSelector::Active,
            query,
            mode: LocateMode::Exact,
            maximum_results: 7,
            ..
        } if *observed == repository() && query == "answer"
    ));
    assert!(matches!(
        &calls[3],
        Call::SymbolExplain {
            generation: GenerationSelector::Generation(observed),
            symbols,
            ..
        } if *observed == parent_generation() && symbols == &[symbol()]
    ));
    assert!(matches!(
        &calls[4],
        Call::SourceRead {
            generation: GenerationSelector::Generation(observed),
            references,
            ..
        } if *observed == generation() && references.len() == 1
    ));
}

#[tokio::test]
async fn unavailable_port_returns_transport_for_every_tool() {
    let executor =
        FirstSliceToolExecutor::new(UnavailableFirstSliceClientPort).expect("executor initializes");
    for (tool, arguments) in valid_inputs() {
        let error = executor
            .execute(tool, object(arguments), cancellation())
            .await
            .expect_err("unavailable port rejects every call");
        assert_eq!(error.failure(), Some(ToolExecutionFailure::Transport));
    }
}

#[test]
fn client_errors_map_to_source_free_port_classes() {
    let public = PublicError::builder(ErrorCode::NotFound, "requested entity was not found")
        .build()
        .expect("public fixture validates");
    assert_eq!(
        map_client_error(ClientError::Public(Box::new(public.clone()))),
        crate::ClientPortError::Public(Box::new(public))
    );
    assert_eq!(
        map_client_error(ClientError::UnexpectedResponse),
        crate::ClientPortError::InvalidResponse
    );
    assert_eq!(
        map_client_error(ClientError::InvalidResponseCorrelation),
        crate::ClientPortError::InvalidResponse
    );
    assert_eq!(
        map_client_error(ClientError::ResponseAllocationFailed),
        crate::ClientPortError::Executor
    );
    assert_eq!(
        map_client_error(ClientError::RequestTimedOut),
        crate::ClientPortError::Transport
    );
    assert_eq!(
        map_client_error(ClientError::DaemonLaunchCleanupTimedOut),
        crate::ClientPortError::Transport
    );
    assert_eq!(
        map_client_error(ClientError::InvalidFirstSliceRequest),
        crate::ClientPortError::Executor
    );
}

async fn execute<T: DeserializeOwned>(
    executor: &FirstSliceToolExecutor<NativeFirstSliceClientPort>,
    tool: VerticalTool,
    arguments: Value,
) -> T {
    let output = executor
        .execute(tool, object(arguments), cancellation())
        .await
        .expect("native adapter maps response");
    serde_json::from_value(Value::Object(output)).expect("mapped output decodes")
}

fn object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("test tool input is an object")
}

fn cancellation() -> RequestCancellation {
    static SENDER: OnceLock<watch::Sender<bool>> = OnceLock::new();
    let sender = SENDER.get_or_init(|| watch::channel(false).0);
    RequestCancellation {
        receiver: sender.subscribe(),
    }
}

fn valid_inputs() -> [(VerticalTool, Value); 5] {
    [
        (VerticalTool::RepoIndex, json!({"root": "C:/fixture"})),
        (
            VerticalTool::OperationStatus,
            json!({"operation_id": operation()}),
        ),
        (
            VerticalTool::CodeLocate,
            json!({
                "repository": {"repository_id": repository()},
                "query": "answer"
            }),
        ),
        (
            VerticalTool::SymbolExplain,
            json!({
                "repository": {"repository_id": repository()},
                "symbol_ids": [symbol()]
            }),
        ),
        (
            VerticalTool::SourceRead,
            json!({
                "repository": {"repository_id": repository()},
                "generation": generation(),
                "references": [{
                    "source_ref": {
                        "repository": repository(),
                        "generation": generation(),
                        "span": {
                            "file": file(),
                            "start_byte": 0,
                            "end_byte": 0
                        },
                        "content_hash": content_hash(),
                        "line_hint": {
                            "start_line": 1,
                            "end_line": 1
                        }
                    }
                }]
            }),
        ),
    ]
}

fn query_context(
    repository: RepositoryId,
    selector: GenerationSelector,
    active_generation: bool,
) -> QueryContext {
    let resolved_generation = match selector {
        GenerationSelector::Active => generation(),
        GenerationSelector::Generation(generation) => generation,
    };
    QueryContext {
        repository,
        generation: resolved_generation,
        parent_generation: (resolved_generation != parent_generation())
            .then_some(parent_generation()),
        active_generation,
        tier: ClientAnalysisTier::TierC,
        coverage_status: CoverageStatus::Complete,
        skipped_inputs: 0,
        usage: QueryUsage {
            rows: 1,
            edges: 0,
            results: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            elapsed_micros: 1,
        },
    }
}

const fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

const fn operation() -> OperationId {
    OperationId::from_bytes([2; 16])
}

const fn parent_generation() -> GenerationId {
    GenerationId::from_bytes([3; 20])
}

const fn generation() -> GenerationId {
    GenerationId::from_bytes([4; 20])
}

const fn symbol() -> SymbolId {
    SymbolId::from_bytes([5; 20])
}

const fn file() -> FileId {
    FileId::from_bytes([6; 20])
}

const fn content_hash() -> ContentHash {
    ContentHash::from_bytes([7; 32])
}
