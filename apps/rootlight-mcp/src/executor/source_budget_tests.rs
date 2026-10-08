//! First-stage source evidence with child responses that honor their own budgets.
//! Signature and exact-definition reads share physical work without replacing
//! full editing references with presentation prefixes.

use super::*;

#[derive(Clone)]
struct SourceFixture {
    symbol: SymbolId,
    path: String,
    signature: String,
    chunk: client::SourceTextChunk,
}

impl SourceFixture {
    fn new(marker: u8, signature_bytes: usize, definition_bytes: usize) -> Self {
        let suffix = ") -> usize";
        let mut signature = "fn assemble(".to_owned();
        for index in 0_usize.. {
            let parameter = format!("parameter_{index}: usize, ");
            if signature.len() + parameter.len() + suffix.len() > signature_bytes {
                break;
            }
            signature.push_str(&parameter);
        }
        signature.push_str(&" ".repeat(signature_bytes - signature.len() - suffix.len()));
        signature.push_str(suffix);
        let mut content = format!("{signature} {{\n    // ");
        let tail = "\n    0\n}";
        assert!(definition_bytes >= content.len() + tail.len());
        content.push_str(&"x".repeat(definition_bytes - content.len() - tail.len()));
        content.push_str(tail);
        let content_hash = rootlight_ids::content_hash(content.as_bytes());
        let source = client::SourceReference::new(
            repository(),
            generation(),
            FileId::from_bytes([marker; 20]),
            0..u64::try_from(content.len()).unwrap(),
            content_hash,
            None,
        )
        .unwrap();
        Self {
            symbol: SymbolId::from_bytes([marker; 20]),
            path: format!("src/module_{marker}.rs"),
            signature,
            chunk: client::SourceTextChunk {
                extracted_from: None,
                source,
                path: format!("src/module_{marker}.rs"),
                start_byte: 0,
                end_byte: u64::try_from(content.len()).unwrap(),
                start_line: Some(1),
                end_line: Some(4),
                content,
                physical_encoding: client::SourceTextEncoding::Utf8,
                content_hash,
                language: "rust".to_owned(),
                tier: ClientTier::TierB,
                generated: false,
            },
        }
    }
}

struct BudgetedSourcePort {
    fixtures: Vec<SourceFixture>,
    calls: Mutex<Vec<ObservedCall>>,
    received: Mutex<Vec<QueryContext>>,
}

impl BudgetedSourcePort {
    fn new(fixtures: Vec<SourceFixture>) -> Self {
        Self {
            fixtures,
            calls: Mutex::new(Vec::new()),
            received: Mutex::new(Vec::new()),
        }
    }

    fn record_context(
        &self,
        context: QueryContext,
        options: client::RequestOptions,
    ) -> QueryContext {
        let limits = options.effective_budget().unwrap().limits();
        let usage = &context.usage;
        assert!(usage.rows <= limits.rows);
        assert!(usage.results <= limits.results);
        assert!(usage.edges <= limits.edges);
        assert!(usage.source_bytes <= limits.source_bytes);
        assert!(usage.json_bytes <= limits.json_bytes);
        assert!(usage.memory_bytes.unwrap_or(0) <= limits.memory_bytes);
        assert!(usage.estimated_tokens <= limits.estimated_tokens);
        self.received.lock().unwrap().push(context.clone());
        context
    }

    fn calls(&self) -> Vec<ObservedCall> {
        self.calls.lock().unwrap().clone()
    }

    fn known_work(&self) -> BudgetCharge {
        let contexts = self.received.lock().unwrap();
        first_stage_known_work(&contexts.iter().collect::<Vec<_>>())
    }
}

fn bounded_context(results: u64, physical: u64, presentation: usize) -> QueryContext {
    let mut value = context(results, physical);
    value.usage.rows = 1;
    value.usage.edges = 0;
    value.usage.json_bytes = 128 + u64::try_from(presentation).unwrap();
    value.usage.memory_bytes = Some(value.usage.json_bytes);
    value.usage.estimated_tokens = value.usage.json_bytes;
    value.usage.token_accounting = Some(client::TokenAccountingProfile::Utf8ByteUpperBoundV1);
    value
}

macro_rules! unused_simple_port_methods {
    ($($name:ident($request:ty) -> $response:ty),* $(,)?) => {
        $(fn $name(
            &self,
            _request: $request,
            _cancellation: RequestCancellation,
        ) -> ClientPortFuture<$response> {
            Box::pin(async { Err(ClientPortError::Executor) })
        })*
    };
}

macro_rules! unused_analytical_port_methods {
    ($($name:ident($request:ty) -> $response:ty),* $(,)?) => {
        $(fn $name(
            &self,
            _request: $request,
            _options: client::RequestOptions,
            _cancellation: RequestCancellation,
        ) -> ClientPortFuture<$response> {
            Box::pin(async { Err(ClientPortError::Executor) })
        })*
    };
}

impl FirstSliceClientPort for BudgetedSourcePort {
    unused_simple_port_methods! {
        repository_index(RepositoryIndexPortRequest) -> RepositoryIndexPortResponse,
        operation_status(OperationStatusPortRequest) -> RepositoryOperationStatus,
        repository_catalog_page(RepositoryCatalogPagePortRequest) -> RepositoryCatalogPage,
        repository_status(RepositoryStatusPortRequest) -> RepositoryStatus,
    }

    unused_analytical_port_methods! {
        source_read(SourceReadPortRequest) -> SourceReadPortResponse,
        symbol_relationships(SymbolRelationshipsPortRequest) -> SymbolRelationshipsPortResponse,
        flow_trace(FlowTracePortRequest) -> FlowTracePortResponse,
        architecture_cycles(ArchitectureCyclesPortRequest) -> ArchitectureCyclesPortResponse,
        code_dead(CodeDeadPortRequest) -> CodeDeadPortResponse,
        architecture_overview(ArchitectureOverviewPortRequest) -> ArchitectureOverviewPortResponse,
        tests_select(TestsSelectPortRequest) -> TestsSelectPortResponse,
        change_impact(ChangeImpactPortRequest) -> ChangeImpactPortResponse,
        plan_change(PlanChangePortRequest) -> PlanChangePortResponse,
        history_compare(HistoryComparePortRequest) -> HistoryComparePortResponse,
        query_advanced(QueryAdvancedPortRequest) -> QueryAdvancedPortResponse,
    }

    fn code_locate(
        &self,
        request: CodeLocatePortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        let fixture = self
            .fixtures
            .iter()
            .find(|fixture| fixture.path == request.query())
            .unwrap();
        let mut response = locate_response();
        response.result.hits.truncate(1);
        response.result.hits[0].symbol = Some(fixture.symbol);
        response.result.hits[0].file = fixture.chunk.source.file();
        response.result.hits[0].identifier = "assemble".to_owned();
        response.result.hits[0].qualified_name = "crate::assemble".to_owned();
        response.result.hits[0].path = fixture.path.clone();
        response.result.hits[0].kind = "function".to_owned();
        response.result.hits[0].tier = ClientTier::TierB;
        response.result.hits[0].source = Some(fixture.chunk.source.clone());
        response.result.context = self.record_context(bounded_context(1, 0, 0), options);
        self.calls
            .lock()
            .unwrap()
            .push(ObservedCall::CodeLocate(ObservedAnalyticCall::new(
                request, options,
            )));
        Box::pin(async move { Ok(response) })
    }

    fn symbol_explain(
        &self,
        request: SymbolExplainPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        assert_eq!(request.symbols().len(), 1);
        let fixture = self
            .fixtures
            .iter()
            .find(|fixture| request.symbols() == [fixture.symbol])
            .unwrap();
        let signature_bytes = u64::try_from(fixture.signature.len()).unwrap();
        let fits = signature_bytes <= options.effective_budget().unwrap().limits().source_bytes;
        let mut response = explain_response(fixture.chunk.source.clone());
        response.result.unresolved_symbols.clear();
        response.result.symbols[0].symbol = fixture.symbol;
        response.result.symbols[0].display_name = "assemble".to_owned();
        response.result.symbols[0].qualified_name = "crate::assemble".to_owned();
        response.result.symbols[0].signature = fits.then(|| fixture.signature.clone());
        // A real compact explanation accounts entity, provenance and evidence
        // independently of the single candidate projected by the parent.
        response.result.context = self.record_context(
            bounded_context(
                4,
                if fits { signature_bytes } else { 0 },
                if fits { fixture.signature.len() } else { 0 },
            ),
            options,
        );
        if !fits {
            response.result.truncated = true;
            response.result.execution_completeness = truncated_execution(
                client::LimitingResourceKind::SourceBytes,
                client::ContinuationGuidance::SplitRequest,
            );
        }
        self.calls
            .lock()
            .unwrap()
            .push(ObservedCall::SymbolExplain(ObservedAnalyticCall::new(
                request, options,
            )));
        Box::pin(async move { Ok(response) })
    }

    fn source_read_lossless_text(
        &self,
        request: SourceReadPortRequest,
        options: client::RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadLosslessPortResponse> {
        let limit = options.effective_budget().unwrap().limits().source_bytes;
        let mut chunks = Vec::new();
        let mut physical = 0_u64;
        let mut presentation = 0_usize;
        for reference in request.references() {
            let fixture = self
                .fixtures
                .iter()
                .find(|fixture| &fixture.chunk.source == reference)
                .unwrap();
            let width = reference.byte_range().end - reference.byte_range().start;
            if physical + width > limit {
                break;
            }
            physical += width;
            presentation += fixture.chunk.content.len();
            chunks.push(fixture.chunk.clone());
        }
        let truncated = chunks.len() < request.references().len();
        let context = self.record_context(
            bounded_context(u64::try_from(chunks.len()).unwrap(), physical, presentation),
            options,
        );
        self.calls
            .lock()
            .unwrap()
            .push(ObservedCall::SourceReadLossless(ObservedAnalyticCall::new(
                request, options,
            )));
        let response = SourceReadLosslessPortResponse::new(
            client::SourceTextReadResult {
                context,
                chunks,
                total_source_bytes: physical,
                total_text_bytes: u64::try_from(presentation).unwrap(),
                truncated,
                execution_completeness: if truncated {
                    truncated_execution(
                        client::LimitingResourceKind::SourceBytes,
                        client::ContinuationGuidance::SplitRequest,
                    )
                } else {
                    complete_execution()
                },
                conditional_activity: Vec::new(),
            },
            metadata("trace-budgeted-exact-source"),
            Vec::new(),
            Vec::new(),
        );
        Box::pin(async move { Ok(response) })
    }
}

fn source_invocation(
    fixtures: &[SourceFixture],
    paths: bool,
    reverse: bool,
) -> EvidenceProviderInvocation {
    let mut symbols: Vec<_> = fixtures.iter().map(|fixture| fixture.symbol).collect();
    let mut path_values: Vec<_> = fixtures
        .iter()
        .map(|fixture| fixture.path.clone())
        .collect();
    if reverse {
        symbols.reverse();
        path_values.reverse();
    }
    let input: ContextPackInput = serde_json::from_value(json!({
        "repository": {"repository_id": repository()},
        "task": "inspect the selected declarations",
        "seeds": if paths { json!({"paths": path_values}) } else { json!({"symbols": symbols}) },
        "sections": ["source"], "source_policy": "references_only", "token_budget": 4000,
    }))
    .unwrap();
    let request = CanonicalContextPackRequest::new(&input, repository(), generation()).unwrap();
    rootlight_agent::context_evidence::ContextEvidenceProviderRegistry
        .plan(&request)
        .unwrap()
        .invocations()
        .iter()
        .find(|invocation| invocation.provider() == EvidenceProvider::Implementation)
        .unwrap()
        .clone()
}

async fn retrieve(
    port: Arc<BudgetedSourcePort>,
    invocation: EvidenceProviderInvocation,
) -> EvidenceProviderOutput {
    let reservation = invocation.reservation();
    retrieve_with_reservation(port, invocation, reservation).await
}

async fn retrieve_with_reservation(
    port: Arc<BudgetedSourcePort>,
    invocation: EvidenceProviderInvocation,
    reservation: BudgetCharge,
) -> EvidenceProviderOutput {
    let lookups = invocation
        .anchors()
        .iter()
        .filter(|anchor| matches!(anchor, EvidenceAnchor::Path(_)))
        .count();
    let options = context_evidence::source_evidence_options(reservation, lookups).unwrap();
    context_evidence::retrieve_source_evidence(
        port,
        invocation,
        reservation,
        options,
        cancellation(),
    )
    .await
    .unwrap()
}

fn assert_source_truncation(output: &EvidenceProviderOutput) {
    assert_eq!(output.completeness.state, CompletenessState::Truncated);
    assert!(
        output
            .completeness
            .limiting_resources
            .iter()
            .any(|resource| resource.kind == ContractLimitingResourceKind::SourceBytes)
    );
}

fn assert_known_work(output: &EvidenceProviderOutput, port: &BudgetedSourcePort) {
    let mut expected = port.known_work();
    expected.results = u64::try_from(output.observations.len()).unwrap();
    expected.tokens = output
        .observations
        .iter()
        .map(|value| value.estimated_tokens)
        .sum();
    assert_eq!(output.usage, expected);
}

#[tokio::test]
async fn first_stage_source_budget_bounds_individually_affordable_header_and_body() {
    let fixture = SourceFixture::new(20, 640, 1_800);
    let invocation = source_invocation(std::slice::from_ref(&fixture), false, false);
    assert_eq!(invocation.max_candidates(), 1);
    assert_eq!(invocation.reservation().source_bytes, 2_048);
    assert_eq!(invocation.reservation().paths, 1);
    assert_eq!(invocation.reservation().results, 4);
    let port = Arc::new(BudgetedSourcePort::new(vec![fixture.clone()]));
    let output = retrieve(Arc::clone(&port), invocation).await;
    assert_source_truncation(&output);
    assert!(output.observations.is_empty());
    assert_eq!(output.usage.source_bytes, 640);
    assert_known_work(&output, &port);
    let calls = port.calls();
    let [
        ObservedCall::SymbolExplain(_),
        ObservedCall::SourceReadLossless(read),
    ] = calls.as_slice()
    else {
        panic!("source evidence uses explanation followed by exact text read");
    };
    assert_eq!(read.references(), [fixture.chunk.source]);
    assert_eq!(
        read.options
            .effective_budget()
            .unwrap()
            .limits()
            .source_bytes,
        2_048 - 640
    );
    assert!(!read.merge_overlaps());
    assert_eq!(read.context_lines_before(), 0);
    assert_eq!(read.context_lines_after(), 0);
}

#[tokio::test]
async fn first_stage_source_budget_admits_exact_body_at_remaining_source_boundary() {
    for body_bytes in [1_408, 1_409] {
        let fixture = SourceFixture::new(20, 640, body_bytes);
        let full_reference = client_source_ref(&fixture.chunk.source).unwrap();
        let invocation = source_invocation(std::slice::from_ref(&fixture), false, false);
        let port = Arc::new(BudgetedSourcePort::new(vec![fixture]));
        let output = retrieve(Arc::clone(&port), invocation).await;
        if body_bytes == 1_408 {
            assert_eq!(output.completeness.state, CompletenessState::Complete);
            assert_eq!(output.usage.source_bytes, 2_048);
            assert_eq!(output.observations.len(), 1);
            assert_eq!(output.observations[0].source_refs, [full_reference]);
            assert_eq!(output.observations[0].source_bytes, 1_408);
        } else {
            assert_source_truncation(&output);
            assert!(output.observations.is_empty());
            assert_eq!(output.usage.source_bytes, 640);
        }
        assert_known_work(&output, &port);
    }
}

#[tokio::test]
async fn first_stage_source_budget_exhaustion_skips_read_and_preserves_explanation_work() {
    let fixture = SourceFixture::new(20, 2_048, 2_080);
    let invocation = source_invocation(std::slice::from_ref(&fixture), false, false);
    let port = Arc::new(BudgetedSourcePort::new(vec![fixture]));
    let output = retrieve(Arc::clone(&port), invocation).await;
    assert_source_truncation(&output);
    assert!(output.observations.is_empty());
    assert_eq!(output.usage.source_bytes, 2_048);
    assert!(matches!(
        port.calls().as_slice(),
        [ObservedCall::SymbolExplain(_)]
    ));
    assert_known_work(&output, &port);
}

#[tokio::test]
async fn first_stage_source_budget_multi_anchor_order_uses_only_actual_remainder() {
    let fixtures = vec![
        SourceFixture::new(20, 800, 1_400),
        SourceFixture::new(21, 900, 1_450),
    ];
    for reverse in [false, true] {
        let invocation = source_invocation(&fixtures, false, reverse);
        assert_eq!(invocation.reservation().source_bytes, 4_096);
        let port = Arc::new(BudgetedSourcePort::new(fixtures.clone()));
        let output = retrieve(Arc::clone(&port), invocation).await;
        assert_source_truncation(&output);
        assert_eq!(output.usage.source_bytes, 800 + 900 + 1_400);
        assert_eq!(output.observations.len(), 1);
        assert_eq!(output.observations[0].symbol_id, Some(fixtures[0].symbol));
        assert_eq!(
            output.observations[0].source_refs,
            [client_source_ref(&fixtures[0].chunk.source).unwrap()]
        );
        assert_known_work(&output, &port);
        let calls = port.calls();
        let [
            ObservedCall::SymbolExplain(first),
            ObservedCall::SymbolExplain(second),
            ObservedCall::SourceReadLossless(read),
        ] = calls.as_slice()
        else {
            panic!("explicit anchors are explained once then read in canonical order");
        };
        assert_eq!(first.symbols(), [fixtures[0].symbol]);
        assert_eq!(second.symbols(), [fixtures[1].symbol]);
        assert!(
            second
                .options
                .effective_budget()
                .unwrap()
                .limits()
                .source_bytes
                <= 4_096 - 800
        );
        assert_eq!(
            read.options
                .effective_budget()
                .unwrap()
                .limits()
                .source_bytes,
            4_096 - 800 - 900
        );
        assert_eq!(
            read.references(),
            [
                fixtures[0].chunk.source.clone(),
                fixtures[1].chunk.source.clone()
            ]
        );
    }
}

#[tokio::test]
async fn first_stage_source_budget_multiple_path_lookups_preserve_order_and_known_work() {
    let fixtures = vec![
        SourceFixture::new(20, 800, 1_400),
        SourceFixture::new(21, 900, 1_450),
    ];
    for reverse in [false, true] {
        let invocation = source_invocation(&fixtures, true, reverse);
        let source_limit = invocation.reservation().source_bytes;
        let port = Arc::new(BudgetedSourcePort::new(fixtures.clone()));
        let output = retrieve(Arc::clone(&port), invocation).await;
        assert_eq!(output.completeness.state, CompletenessState::Complete);
        assert_eq!(output.observations.len(), 2);
        assert!(output.usage.source_bytes <= source_limit);
        assert_known_work(&output, &port);
        let calls = port.calls();
        let [
            ObservedCall::CodeLocate(_),
            ObservedCall::CodeLocate(_),
            ObservedCall::SymbolExplain(_),
            ObservedCall::SymbolExplain(_),
            ObservedCall::SourceReadLossless(read),
        ] = calls.as_slice()
        else {
            panic!("path lookup work is followed by canonical explanations and one exact read");
        };
        assert_eq!(
            read.options
                .effective_budget()
                .unwrap()
                .limits()
                .source_bytes,
            source_limit - 800 - 900
        );
        assert_eq!(
            read.references(),
            [
                fixtures[0].chunk.source.clone(),
                fixtures[1].chunk.source.clone()
            ]
        );
    }
}

#[tokio::test]
async fn first_stage_source_budget_respects_reduced_call_reservation() {
    let fixture = SourceFixture::new(20, 640, 1_408);
    let invocation = source_invocation(std::slice::from_ref(&fixture), false, false);
    let mut reservation = invocation.reservation();
    reservation.source_bytes = 1_024;
    let port = Arc::new(BudgetedSourcePort::new(vec![fixture]));
    let output = retrieve_with_reservation(Arc::clone(&port), invocation, reservation).await;
    assert_source_truncation(&output);
    assert!(output.observations.is_empty());
    assert_eq!(output.usage.source_bytes, 640);
    assert_known_work(&output, &port);
    let calls = port.calls();
    let [
        ObservedCall::SymbolExplain(_),
        ObservedCall::SourceReadLossless(read),
    ] = calls.as_slice()
    else {
        panic!("reduced call budget still uses the exact read route");
    };
    assert_eq!(
        read.options
            .effective_budget()
            .unwrap()
            .limits()
            .source_bytes,
        384
    );
}
