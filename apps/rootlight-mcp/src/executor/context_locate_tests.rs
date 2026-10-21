//! File-only context evidence retains its exact source without inventing symbols.

use super::*;

fn invocation() -> EvidenceProviderInvocation {
    let input: ContextPackInput = serde_json::from_value(json!({
        "repository": {"repository_id": repository()},
        "task": "inspect the selected source file",
        "seeds": {"paths": ["src/lib.rs"]},
        "sections": ["definitions"],
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
        .find(|invocation| invocation.provider() == EvidenceProvider::Locate)
        .expect("a path definition seed uses located evidence")
        .clone()
}

async fn retrieve(harness: &Harness) -> Result<EvidenceProviderOutput, ContextEvidencePortError> {
    let adapter = McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::clone(&harness.executor.batch_validator),
        unsupported: harness.executor.unsupported.clone(),
        invalid_arguments: harness.executor.invalid_arguments.clone(),
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
    };
    let invocation = invocation();
    let context = ContextEvidenceCallContext::new(
        cancellation(),
        Instant::now() + Duration::from_secs(10),
        invocation.reservation(),
    );
    adapter.retrieve(invocation, context).await
}

fn file_response() -> CodeLocatePortResponse {
    let mut response = locate_response();
    let hit = &mut response.result.hits[0];
    hit.symbol = None;
    hit.kind = "file".to_owned();
    hit.identifier = "lib.rs".to_owned();
    hit.qualified_name = "src/lib.rs".to_owned();
    hit.source = Some(source_reference_without_lines(0, 48));
    response
}

#[tokio::test]
async fn file_path_context_retains_exact_source_without_fabricating_a_symbol() {
    let response = file_response();
    let expected = client_source_ref(response.result.hits[0].source.as_ref().unwrap()).unwrap();
    let expected_usage = first_stage_known_work(&[&response.result.context]);
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
    let output = retrieve(&harness).await.unwrap();
    let [observation] = output.observations.as_slice() else {
        panic!("one exact file hit produces one source-owned context observation");
    };
    assert_eq!(observation.symbol_id, None);
    assert_eq!(observation.identity, file().to_string());
    assert_eq!(observation.source_refs, [expected]);
    assert_eq!(observation.observed_score, Some(990));
    assert_eq!(output.completeness.state, CompletenessState::Complete);
    assert_eq!(output.usage.rows, expected_usage.rows);
    assert_eq!(output.usage.source_bytes, expected_usage.source_bytes);
    let calls = harness.calls.lock().unwrap();
    let [ObservedCall::CodeLocate(call)] = calls.as_slice() else {
        panic!("a file hit requires only the existing bounded locate request");
    };
    assert_eq!(call.query(), "src/lib.rs");
    assert_eq!(call.mode(), LocateMode::Prefix);
    assert_eq!(call.path_prefixes(), ["src/lib.rs"]);
    assert_eq!(call.kinds(), Some(["file".to_owned()].as_slice()));
    assert_eq!(
        call.maximum_results(),
        u32::from(invocation().max_candidates())
    );
}

#[tokio::test]
async fn file_path_context_does_not_promote_missing_source_or_nonfile_hits() {
    for missing_source in [false, true] {
        let mut response = file_response();
        if missing_source {
            response.result.hits[0].source = None;
        } else {
            response.result.hits[0].kind = "function".to_owned();
        }
        let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
        let output = retrieve(&harness).await.unwrap();
        assert!(output.observations.is_empty());
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn file_path_context_rejects_foreign_or_mismatched_source_identity() {
    for (repo, generation_id, file_id) in [
        (RepositoryId::from_bytes([91; 16]), generation(), file()),
        (repository(), GenerationId::from_bytes([92; 20]), file()),
        (repository(), generation(), FileId::from_bytes([93; 20])),
    ] {
        let mut response = file_response();
        response.result.hits[0].source = Some(
            client::SourceReference::new(repo, generation_id, file_id, 0..48, content_hash(), None)
                .unwrap(),
        );
        let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
        let error = retrieve(&harness).await.unwrap_err();
        assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
        assert_eq!(harness.call_count.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn located_symbol_context_keeps_its_existing_symbol_and_source() {
    let response = locate_response();
    let expected = client_source_ref(response.result.hits[0].source.as_ref().unwrap()).unwrap();
    let harness = Harness::new(FakeOutcome::CodeLocate(Ok(response)));
    let output = retrieve(&harness).await.unwrap();
    let [observation] = output.observations.as_slice() else {
        panic!("an existing located symbol still produces one observation");
    };
    assert_eq!(observation.symbol_id, Some(symbol()));
    assert_eq!(observation.identity, symbol().to_string());
    assert_eq!(observation.source_refs, [expected]);
}
