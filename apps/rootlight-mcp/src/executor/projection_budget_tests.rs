//! Standalone Explain keeps internal representation separate from public limits.

use super::*;

fn rich_explanation() -> (SymbolExplainPortResponse, Value) {
    let mut response = explain_response(source_reference_without_lines(0, 12));
    let template = response.result.symbols[0].clone();
    response.result.symbols = (1..=12)
        .map(|byte| {
            let mut value = template.clone();
            value.symbol = SymbolId::from_bytes([byte; 20]);
            value.provider = "p".repeat(128);
            value.evidence = "e".repeat(128);
            value.provenance_frontend_version = Some("v".repeat(256));
            value.provenance_rule = Some("r".repeat(256));
            value
        })
        .collect();
    response.result.symbols.sort_by_key(|value| value.symbol);
    response.result.unresolved_symbols.clear();
    response.result.context = context(24, 0);
    let arguments = json!({
        "repository": {"repository_id": repository()},
        "generation": generation(),
        "symbol_ids": response.result.symbols.iter().map(|value| value.symbol).collect::<Vec<_>>(),
        "include_provenance": "none",
        "response_profile": "evidence",
        "budget": {"max_tokens": 16000}
    });
    (response, arguments)
}

#[tokio::test]
async fn standalone_explain_admits_richer_internal_dto_when_public_output_fits() {
    let (response, arguments) = rich_explanation();
    let internal_bytes = serde_json::to_vec(&response.result).unwrap().len();
    assert!(
        internal_bytes > 16_000,
        "fixture exercises the internal envelope"
    );
    let harness = Harness::new(FakeOutcome::MeasuredSymbolExplain(response));
    let output = execute(&harness.executor, VerticalTool::SymbolExplain, arguments)
        .await
        .expect("public projection fits despite richer bounded internal DTO");
    let public_bytes = output["usage"]["json_bytes"].as_u64().unwrap();
    assert!(public_bytes < 16_000);
    assert_eq!(output["data"]["symbols"].as_array().unwrap().len(), 12);
    assert!(
        output["data"]["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value["provenance"].as_array().is_none_or(Vec::is_empty))
    );
    let ObservedCall::SymbolExplain(call) = harness.only_call() else {
        panic!("only Explain was dispatched");
    };
    let limits = call.options.effective_budget().unwrap().limits();
    assert!(limits.estimated_tokens >= u64::try_from(internal_bytes).unwrap());
    assert!(limits.estimated_tokens <= limits.json_bytes.max(16_000));
}

#[tokio::test]
async fn standalone_explain_still_rejects_public_output_over_caller_limit() {
    let (response, mut arguments) = rich_explanation();
    arguments["include_provenance"] = json!("full");
    let mut unbounded_arguments = arguments.clone();
    unbounded_arguments
        .as_object_mut()
        .unwrap()
        .remove("budget");
    let reference = Harness::new(FakeOutcome::SymbolExplain(Ok(response.clone())));
    let measured = execute(
        &reference.executor,
        VerticalTool::SymbolExplain,
        unbounded_arguments,
    )
    .await
    .expect("server-bounded reference projection is valid");
    assert!(measured["usage"]["json_bytes"].as_u64().unwrap() > 16_000);
    let harness = Harness::new(FakeOutcome::MeasuredSymbolExplain(response));
    let error = execute(&harness.executor, VerticalTool::SymbolExplain, arguments)
        .await
        .expect_err("the mapped public envelope remains bounded");
    assert_measured_budget_error(error.public_error().unwrap(), "tokens", 16_000);
    let ObservedCall::SymbolExplain(call) = harness.only_call() else {
        panic!("only Explain was dispatched");
    };
    assert!(
        call.options
            .effective_budget()
            .unwrap()
            .limits()
            .estimated_tokens
            > 16_000,
        "the public guard, rather than the old internal coupling, rejected output"
    );
}

#[test]
fn standalone_explain_projection_preserves_every_other_budget_dimension() {
    let requested = ResponseBudget {
        max_results: Some(30),
        max_tokens: Some(100),
        max_source_bytes: Some(256),
        max_traversal_facts: Some(40),
        max_depth: Some(2),
        max_paths: Some(5),
        timeout_ms: Some(10),
        evidence_level: None,
    };
    let budget = AnalyticalBudget::new(Some(&requested)).unwrap();
    let original = budget.options.effective_budget().unwrap().limits();
    let options = budget.projected_response_options().unwrap();
    let transported = options.effective_budget().unwrap().limits();
    assert_eq!(budget.limits.maximums().tokens, 100);
    assert_eq!(transported.rows, original.rows);
    assert_eq!(transported.edges, original.edges);
    assert_eq!(transported.results, original.results);
    assert_eq!(transported.source_bytes, original.source_bytes);
    assert_eq!(transported.json_bytes, original.json_bytes);
    assert_eq!(transported.memory_bytes, original.memory_bytes);
    assert_eq!(transported.duration, original.duration);
    assert_eq!(transported.depth, original.depth);
    assert_eq!(transported.paths, original.paths);
    assert_eq!(options.timeout(), budget.options.timeout());
    assert_eq!(
        transported.estimated_tokens,
        BudgetLimits::server_ceiling().maximums().tokens
    );
    let omitted = AnalyticalBudget::new(None).unwrap();
    assert_eq!(
        omitted.projected_response_options().unwrap(),
        omitted.options
    );
    let mut bounded = budget.limits.maximums();
    bounded.json_bytes = 4_096;
    let internal = AnalyticalBudget::from_limits(BudgetLimits::from_maximums(bounded)).unwrap();
    let transported = internal
        .projected_response_options()
        .unwrap()
        .effective_budget()
        .unwrap()
        .limits();
    assert_eq!(transported.estimated_tokens, 4_096);
    assert_eq!(transported.json_bytes, 4_096);
    assert_eq!(internal.limits.maximums().tokens, 100);
}

#[tokio::test]
async fn standalone_explain_projection_keeps_inflight_cancellation() {
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(FakeOutcome::ContextPendingExplanation {
        locate: locate_response(),
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
    });
    let router = ToolRouter::new(harness.executor, ExposureProfile::Developer).unwrap();
    let (sender, receiver) = watch::channel(false);
    let task = tokio::spawn(async move {
        router
            .handle(
                operating_request(json!({
                    "name": "symbol.explain", "arguments": {
                        "repository": {"repository_id": repository()},
                        "symbol_ids": [symbol()], "budget": {"max_tokens": 16000}
                    }
                })),
                RequestCancellation { receiver },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    sender.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, HandlerResponse::Cancelled));
    assert!(dropped.load(Ordering::SeqCst));
}
