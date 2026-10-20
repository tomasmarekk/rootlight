//! Interrupted requests retain received work, not unobserved pending work.

use super::*;
use rootlight_agent::context_evidence::{
    ContextEvidenceCollector, ContextEvidenceProviderRegistry, ContextSourceRequest,
    ContextSourceTarget,
};
use rootlight_mcp_contract::context::SourcePolicy;

fn adapter(harness: &Harness) -> McpAgentToolPort<FakePort> {
    McpAgentToolPort {
        port: Arc::clone(&harness.executor.port),
        validator: Arc::clone(&harness.executor.batch_validator),
        unsupported: harness.executor.unsupported.clone(),
        invalid_arguments: harness.executor.invalid_arguments.clone(),
        invalid_cursor: harness.executor.invalid_cursor.clone(),
        exposure_profile: ExposureProfile::Developer,
        cursor_key: harness.executor.cursor_key,
    }
}

async fn pending_started(started: &Notify) {
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .expect("the next child starts before the test interrupts it");
}

#[tokio::test]
async fn cancellation_retains_received_locate_work_and_drops_pending_explanation() {
    let mut locate = locate_response();
    locate.result.context.usage.source_bytes = 23;
    locate.result.context.usage.json_bytes = 71;
    locate.result.context.usage.memory_bytes = Some(97);
    let mut expected = first_stage_known_work(&[&locate.result.context]);
    expected.results = 0;
    expected.tokens = 0;
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let harness = Harness::new(FakeOutcome::ContextPendingExplanation {
        locate,
        started: Arc::clone(&started),
        dropped: Arc::clone(&dropped),
    });
    let (sender, receiver) = watch::channel(false);
    let invocation = first_stage_source_invocation();
    let future = adapter(&harness).retrieve(
        invocation.clone(),
        ContextEvidenceCallContext::new(
            RequestCancellation { receiver },
            Instant::now() + Duration::from_secs(30),
            invocation.reservation(),
        ),
    );
    let task = tokio::spawn(future);
    pending_started(&started).await;
    sender.send(true).unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, expected);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(harness.calls.lock().unwrap().len(), 2);
}

async fn candidate_id() -> rootlight_agent::context_evidence::EvidenceCandidateId {
    let input: ContextPackInput = serde_json::from_value(json!({
        "repository": {"repository_id": repository()}, "task": "inspect source",
        "seeds": {"symbols": [symbol()]}, "sections": ["definitions"],
        "source_policy": "references_only", "token_budget": 4000,
    }))
    .unwrap();
    let request = CanonicalContextPackRequest::new(&input, repository(), generation()).unwrap();
    let plan = ContextEvidenceProviderRegistry.plan(&request).unwrap();
    let mut response = explain_response(source_reference_without_lines(0, 8));
    response.result.unresolved_symbols.clear();
    response.result.context.usage.results = 1;
    let harness = Harness::new(FakeOutcome::SymbolExplain(Ok(response)));
    ContextEvidenceCollector
        .collect(
            &adapter(&harness),
            &request,
            &plan,
            cancellation(),
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .unwrap()
        .candidates[0]
        .id()
        .clone()
}

async fn prefix_batches(
    completed_batches: usize,
) -> (
    ContextSourceRequest,
    Vec<client::SourceTextPrefixReadResult>,
) {
    let candidate = candidate_id().await;
    let content = "fn item;";
    let hash = rootlight_ids::content_hash(content.as_bytes());
    let refs: Vec<_> = (0..completed_batches * 32 + 1)
        .map(|ordinal| {
            client::SourceReference::new(
                repository(),
                generation(),
                FileId::from_bytes([u8::try_from(ordinal + 1).unwrap(); 20]),
                0..u64::try_from(content.len()).unwrap(),
                hash,
                None,
            )
            .unwrap()
        })
        .collect();
    let request = ContextSourceRequest {
        repository: repository(),
        generation: generation(),
        source_policy: SourcePolicy::FocusedSnippets,
        include_snippets: true,
        max_bytes_per_snippet: 2048,
        targets: refs
            .iter()
            .map(|reference| ContextSourceTarget {
                // Repeated identity is a transport-batching fixture, not an agent admission proof.
                candidate_id: candidate.clone(),
                source_ref: client_source_ref(reference).unwrap(),
            })
            .collect(),
    };
    let responses = refs
        .chunks(32)
        .take(completed_batches)
        .map(|batch| {
            let bytes = u64::try_from(batch.len() * content.len()).unwrap();
            let mut query_context = context(u64::try_from(batch.len()).unwrap(), bytes);
            query_context.usage.json_bytes = u64::try_from(batch.len() * 256).unwrap();
            query_context.usage.memory_bytes = Some(u64::try_from(batch.len() * 128).unwrap());
            client::SourceTextPrefixReadResult {
                result: client::SourceTextReadResult {
                    source_recovery: None,
                    context: query_context,
                    chunks: batch
                        .iter()
                        .map(|reference| client::SourceTextChunk {
                            extracted_from: None,
                            source: reference.clone(),
                            path: "src/item.rs".into(),
                            start_byte: 0,
                            end_byte: u64::try_from(content.len()).unwrap(),
                            start_line: None,
                            end_line: None,
                            content: content.into(),
                            physical_encoding: client::SourceTextEncoding::Utf8,
                            content_hash: hash,
                            language: "rust".into(),
                            tier: ClientTier::TierB,
                            generated: false,
                        })
                        .collect(),
                    total_source_bytes: bytes,
                    total_text_bytes: bytes,
                    truncated: false,
                    execution_completeness: complete_execution(),
                    conditional_activity: Vec::new(),
                },
                prefix_truncated: false,
            }
        })
        .collect();
    (request, responses)
}

#[tokio::test]
async fn cancellation_retains_completed_prefix_batches_without_double_charging() {
    for completed_batches in [1, 2] {
        let (request, responses) = prefix_batches(completed_batches).await;
        let contexts: Vec<_> = responses
            .iter()
            .map(|response| &response.result.context)
            .collect();
        let expected = first_stage_known_work(&contexts);
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let harness = Harness::new(FakeOutcome::SourcePrefixThenPending {
            responses: Arc::new(Mutex::new(responses.into())),
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        });
        let (sender, receiver) = watch::channel(false);
        let future = adapter(&harness).materialize_source(
            request,
            ContextEvidenceCallContext::new(
                RequestCancellation { receiver },
                Instant::now() + Duration::from_secs(30),
                BudgetLimits::server_ceiling().maximums(),
            ),
        );
        let task = tokio::spawn(future);
        pending_started(&started).await;
        sender.send(true).unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
        assert_eq!(error.usage, expected);
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(harness.calls.lock().unwrap().len(), completed_batches + 1);
    }
}

#[tokio::test]
async fn signalled_deadline_retains_completed_prefix_receipts_and_drops_pending_batch() {
    for completed_batches in [1, 2] {
        let (request, responses) = prefix_batches(completed_batches).await;
        let contexts: Vec<_> = responses
            .iter()
            .map(|response| &response.result.context)
            .collect();
        let expected = first_stage_known_work(&contexts);
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let harness = Harness::new(FakeOutcome::SourcePrefixThenPending {
            responses: Arc::new(Mutex::new(responses.into())),
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        });
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let port = Arc::clone(&harness.executor.port);
        let task = tokio::spawn(async move {
            let progress = context_evidence::ContextWorkProgress::default();
            context_evidence::await_context_operation(
                context_evidence::materialize_context_source_with_progress(
                    port,
                    request,
                    BudgetLimits::server_ceiling().maximums(),
                    cancellation(),
                    &progress,
                ),
                cancellation(),
                async { receiver.await.expect("test deadline owner remains alive") },
                &progress,
            )
            .await
        });
        pending_started(&started).await;
        sender.send(()).unwrap();
        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.kind, ContextEvidencePortErrorKind::DeadlineExceeded);
        assert_eq!(error.usage, expected);
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(harness.calls.lock().unwrap().len(), completed_batches + 1);
    }
}

#[tokio::test]
async fn completed_invalid_response_keeps_overlapping_receipts_once() {
    let (request, mut responses) = prefix_batches(2).await;
    responses[1].result.context.repository = RepositoryId::from_bytes([83; 16]);
    let contexts: Vec<_> = responses
        .iter()
        .map(|response| &response.result.context)
        .collect();
    let expected = first_stage_known_work(&contexts);
    let harness = Harness::new(FakeOutcome::SourcePrefixThenPending {
        responses: Arc::new(Mutex::new(responses.into())),
        started: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let error = adapter(&harness)
        .materialize_source(
            request,
            ContextEvidenceCallContext::new(
                cancellation(),
                Instant::now() + Duration::from_secs(30),
                BudgetLimits::server_ceiling().maximums(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
    assert_eq!(error.usage, expected);
    assert_eq!(harness.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn cancelled_before_dispatch_has_no_received_work_or_child_calls() {
    let (request, responses) = prefix_batches(1).await;
    let harness = Harness::new(FakeOutcome::SourcePrefixThenPending {
        responses: Arc::new(Mutex::new(responses.into())),
        started: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
    });
    let (_sender, receiver) = watch::channel(true);
    let cancellation = RequestCancellation { receiver };
    let invocation = first_stage_source_invocation();
    let context = ContextEvidenceCallContext::new(
        cancellation.clone(),
        Instant::now() + Duration::from_secs(30),
        invocation.reservation(),
    );
    let error = adapter(&harness)
        .retrieve(invocation, context)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, BudgetCharge::default());
    let error = adapter(&harness)
        .materialize_source(
            request,
            ContextEvidenceCallContext::new(
                cancellation,
                Instant::now() + Duration::from_secs(30),
                BudgetLimits::server_ceiling().maximums(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, BudgetCharge::default());
    assert!(harness.calls.lock().unwrap().is_empty());
}
