//! Related-symbol context must materialize a definition rather than an edge token.

use super::*;

fn relationship_invocation() -> EvidenceProviderInvocation {
    let input: ContextPackInput = serde_json::from_value(json!({
        "repository": {"repository_id": repository()},
        "task": "explain related_caller for the selected symbol",
        "seeds": {"symbols": [symbol()]},
        "sections": ["callers"],
        "source_policy": "references_only",
        "token_budget": 8000,
    }))
    .unwrap();
    let request = CanonicalContextPackRequest::new(&input, repository(), generation()).unwrap();
    rootlight_agent::context_evidence::ContextEvidenceProviderRegistry
        .plan(&request)
        .unwrap()
        .invocations()
        .iter()
        .find(|invocation| invocation.provider() == EvidenceProvider::Relationships)
        .expect("caller context schedules relationship evidence")
        .clone()
}

async fn retrieve(harness: &Harness) -> EvidenceProviderOutput {
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::clone(&harness.executor.batch_validator),
        unsupported: harness.executor.unsupported.clone(),
        invalid_arguments: harness.executor.invalid_arguments.clone(),
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
    };
    let invocation = relationship_invocation();
    let context = ContextEvidenceCallContext::new(
        cancellation(),
        Instant::now() + Duration::from_secs(10),
        invocation.reservation(),
    );
    adapter.retrieve(invocation, context).await.unwrap()
}

fn relationship_fixture(
    definition: client::SourceReference,
) -> (SymbolRelationshipsPortResponse, SymbolExplainPortResponse) {
    let mut relationships = relationships_page(missing_symbol(), None);
    relationships.result.groups[0].direction = "inbound".to_owned();
    relationships.result.groups[0].total_count = 1;
    relationships.result.total_edges = 1;
    let mut explanation = explain_response(definition);
    explanation.result.symbols[0].symbol = missing_symbol();
    explanation.result.symbols[0].display_name = "related_caller".to_owned();
    explanation.result.unresolved_symbols.clear();
    explanation.result.context.usage.results = 1;
    (relationships, explanation)
}

#[tokio::test]
async fn related_context_uses_the_explained_symbol_definition_across_source_orderings() {
    // Earlier edge positions and unrelated file IDs must not become a symbol's
    // primary source when the agent canonicalizes candidate reference ordering.
    for (definition_file, start, end) in [
        (file(), 40, 80),
        (FileId::from_bytes([0; 20]), 0, 40),
        (FileId::from_bytes([255; 20]), 0, 40),
    ] {
        let definition = client::SourceReference::new(
            repository(),
            generation(),
            definition_file,
            start..end,
            content_hash(),
            None,
        )
        .unwrap();
        let expected = client_source_ref(&definition).unwrap();
        let (relationships, explanation) = relationship_fixture(definition);
        let usage =
            first_stage_known_work(&[&relationships.result.context, &explanation.result.context]);
        let harness = Harness::new(FakeOutcome::ContextRelationshipEvidence {
            relationships,
            explanation,
        });
        let output = retrieve(&harness).await;
        let [observation] = output.observations.as_slice() else {
            panic!("one related symbol produces one context candidate");
        };
        assert_eq!(observation.symbol_id, Some(missing_symbol()));
        assert_eq!(observation.source_refs, [expected]);
        assert_eq!(observation.observed_score, Some(900));
        assert_eq!(observation.observed_relevance, Some(1000));
        assert_eq!(output.completeness.state, CompletenessState::Complete);
        assert_eq!(output.usage.rows, usage.rows);
        assert_eq!(output.usage.traversal_facts, usage.traversal_facts);
        let calls = harness.calls.lock().unwrap();
        let [
            ObservedCall::SymbolRelationships(graph),
            ObservedCall::SymbolExplain(explain),
        ] = calls.as_slice()
        else {
            panic!("relationship retrieval reuses its one bounded explanation");
        };
        assert_eq!(graph.seeds(), [symbol()]);
        assert_eq!(explain.symbols(), [missing_symbol()]);
    }
}

#[tokio::test]
async fn bounded_missing_explanation_keeps_edge_evidence_and_truncation() {
    let (relationships, mut explanation) = relationship_fixture(source_reference(40, 80, 4, 6));
    let expected =
        client_source_ref(&relationships.result.groups[0].items[0].source_refs[0]).unwrap();
    explanation.result.symbols.clear();
    explanation.result.context.usage.results = 0;
    explanation.result.truncated = true;
    explanation.result.execution_completeness = truncated_execution(
        client::LimitingResourceKind::Results,
        client::ContinuationGuidance::SplitRequest,
    );
    let harness = Harness::new(FakeOutcome::ContextRelationshipEvidence {
        relationships,
        explanation,
    });
    let output = retrieve(&harness).await;
    let [observation] = output.observations.as_slice() else {
        panic!("bounded explanation omission preserves observed relationship evidence");
    };
    assert_eq!(observation.symbol_id, Some(missing_symbol()));
    assert_eq!(observation.source_refs, [expected]);
    assert_eq!(observation.observed_relevance, Some(900));
    assert_eq!(output.completeness.state, CompletenessState::Truncated);
    assert_eq!(harness.call_count.load(Ordering::Relaxed), 2);
}
