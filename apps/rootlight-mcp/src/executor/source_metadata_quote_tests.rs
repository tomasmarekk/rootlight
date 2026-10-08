//! Metadata transport checks full-reference order and checked cumulative bounds.
//! Fixture counts are protocol statements, not claims about real source hydration.

use super::*;
use rootlight_agent::context_evidence::{ContextSourceMetadataQuoteRequest, ContextSourceTarget};
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

fn refs(count: usize) -> Vec<client::SourceReference> {
    (0..count)
        .map(|index| {
            client::SourceReference::new(
                repository(),
                generation(),
                FileId::from_bytes([u8::try_from(index + 1).unwrap(); 20]),
                4..12,
                rootlight_ids::content_hash(b"neutral indexed source"),
                Some(2..=3),
            )
            .unwrap()
        })
        .collect()
}

async fn request(references: &[client::SourceReference]) -> ContextSourceMetadataQuoteRequest {
    let candidate_id = source_material_tests::candidate_id().await;
    ContextSourceMetadataQuoteRequest {
        repository: repository(),
        generation: generation(),
        source_policy: SourcePolicy::FocusedSnippets,
        include_snippets: true,
        targets: references
            .iter()
            .map(|reference| ContextSourceTarget {
                candidate_id: candidate_id.clone(),
                source_ref: client_source_ref(reference).unwrap(),
            })
            .collect(),
    }
}

fn response(references: &[client::SourceReference]) -> client::SourceTextPrefixMetadataQuoteResult {
    let mut context = context(u64::try_from(references.len()).unwrap(), 0);
    context.usage.rows = 37;
    context.usage.edges = 41;
    context.usage.json_bytes = 211;
    context.usage.estimated_tokens = 211;
    context.usage.memory_bytes = Some(317);
    context.usage.token_accounting = Some(client::TokenAccountingProfile::Utf8ByteUpperBoundV1);
    client::SourceTextPrefixMetadataQuoteResult {
        context,
        quote_policy_version: 1,
        entries: references
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let count = u64::try_from(index + 1).unwrap();
                client::SourceTextPrefixMetadataQuoteEntry {
                    source: source.clone(),
                    prefix_fixed_upper: client::SourceTextPrefixFixedUpper {
                        json_bytes: 1_000 + count * 71,
                        memory_bytes: 2_000 + count * 97,
                        rows: count * 3,
                        traversal_facts: count * 5,
                        paths: count,
                    },
                }
            })
            .collect(),
        execution_completeness: complete_execution(),
    }
}

fn harness(
    responses: Vec<client::SourceTextPrefixMetadataQuoteResult>,
    pending: Option<(Arc<Notify>, Arc<AtomicBool>)>,
) -> Harness {
    Harness::new(FakeOutcome::SourceMetadataQuotes {
        responses: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
        pending,
        cancel_on_last: None,
    })
}

async fn quote(
    harness: &Harness,
    request: ContextSourceMetadataQuoteRequest,
) -> Result<
    rootlight_agent::context_evidence::ContextSourceMetadataQuoteOutput,
    ContextEvidencePortError,
> {
    adapter(harness)
        .quote_source_metadata(
            request,
            ContextEvidenceCallContext::new(
                cancellation(),
                Instant::now() + Duration::from_secs(30),
                BudgetLimits::server_ceiling().maximums(),
            ),
        )
        .await
}

#[tokio::test]
async fn metadata_quote_deduplicates_full_refs_and_accumulates_batches() {
    let references = refs(33);
    let mut selected = references.clone();
    selected.insert(1, references[0].clone());
    let first = response(&references[..32]);
    let second = response(&references[32..]);
    let expected_usage = first_stage_known_work(&[&first.context, &second.context]);
    let first_bound = first.entries.last().unwrap().prefix_fixed_upper;
    let second_bound = second.entries[0].prefix_fixed_upper;
    let harness = harness(vec![first, second], None);
    let output = quote(&harness, request(&selected).await).await.unwrap();
    assert_eq!(output.entries.len(), 33);
    assert_eq!(output.usage, expected_usage);
    assert_eq!(output.usage.source_bytes, 0);
    assert_eq!(output.completeness.state, CompletenessState::Complete);
    for (entry, reference) in output.entries.iter().zip(&references) {
        assert_eq!(entry.source_ref, client_source_ref(reference).unwrap());
    }
    let last = output.entries.last().unwrap().prefix_fixed_upper;
    assert_eq!(
        last.json_bytes,
        first_bound.json_bytes + second_bound.json_bytes
    );
    assert_eq!(
        last.memory_bytes,
        first_bound.memory_bytes + second_bound.memory_bytes
    );
    assert_eq!(last.rows, first_bound.rows + second_bound.rows);
    assert_eq!(
        last.traversal_facts,
        first_bound.traversal_facts + second_bound.traversal_facts
    );
    assert_eq!(last.paths, 33);
    let calls = harness.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for (index, call) in calls.iter().enumerate() {
        let ObservedCall::SourceMetadataQuote(call) = call else {
            panic!("metadata-only port");
        };
        assert_eq!(
            call.references(),
            &references[index * 32..(index * 32 + 32).min(33)]
        );
        assert_eq!(
            (call.context_lines_before(), call.context_lines_after()),
            (1, 1)
        );
        assert!(!call.include_line_numbers());
        assert!(!call.merge_overlaps());
    }
}

#[tokio::test]
async fn metadata_quote_rejects_tampering_after_charging_received_work() {
    let references = refs(2);
    for case in 0..9 {
        let mut receipt = response(&references);
        match case {
            0 => receipt.entries.swap(0, 1),
            1 => receipt.quote_policy_version = 2,
            2 => receipt.entries[1].prefix_fixed_upper.json_bytes = 1,
            3 => receipt.context.usage.source_bytes = 13,
            4 => {
                receipt.entries.pop();
            }
            5 => receipt.entries[0].prefix_fixed_upper.json_bytes = 0,
            6 => receipt.entries[0].prefix_fixed_upper.memory_bytes = 0,
            7 => receipt.entries[0].prefix_fixed_upper.rows = 0,
            8 => receipt.entries[0].prefix_fixed_upper.paths = 0,
            _ => unreachable!(),
        }
        let expected = first_stage_known_work(&[&receipt.context]);
        let harness = harness(vec![receipt], None);
        let error = quote(&harness, request(&references).await)
            .await
            .unwrap_err();
        assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
        assert_eq!(error.usage, expected);
        assert_eq!(harness.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn metadata_quote_cancel_retains_first_batch_and_drops_pending_second() {
    let references = refs(33);
    let receipt = response(&references[..32]);
    let expected = first_stage_known_work(&[&receipt.context]);
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let harness = harness(
        vec![receipt],
        Some((Arc::clone(&started), Arc::clone(&dropped))),
    );
    let (sender, receiver) = watch::channel(false);
    let task = tokio::spawn(adapter(&harness).quote_source_metadata(
        request(&references).await,
        ContextEvidenceCallContext::new(
            RequestCancellation { receiver },
            Instant::now() + Duration::from_secs(30),
            BudgetLimits::server_ceiling().maximums(),
        ),
    ));
    tokio::time::timeout(Duration::from_secs(10), started.notified())
        .await
        .unwrap();
    sender.send(true).unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, expected);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(harness.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn metadata_quote_rejects_cross_batch_overflow_without_losing_work() {
    let references = refs(33);
    let mut first = response(&references[..32]);
    for entry in &mut first.entries {
        entry.prefix_fixed_upper.json_bytes = u64::MAX;
    }
    let second = response(&references[32..]);
    let expected = first_stage_known_work(&[&first.context, &second.context]);
    let harness = harness(vec![first, second], None);
    let error = quote(&harness, request(&references).await)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
    assert_eq!(error.usage, expected);
}

#[tokio::test]
async fn metadata_quote_same_poll_cancel_preserves_last_received_receipt() {
    let references = refs(1);
    let receipt = response(&references);
    let expected = first_stage_known_work(&[&receipt.context]);
    let (sender, receiver) = watch::channel(false);
    let harness = Harness::new(FakeOutcome::SourceMetadataQuotes {
        responses: Arc::new(Mutex::new(vec![Ok(receipt)].into())),
        pending: None,
        cancel_on_last: Some(sender),
    });
    let error = adapter(&harness)
        .quote_source_metadata(
            request(&references).await,
            ContextEvidenceCallContext::new(
                RequestCancellation { receiver },
                Instant::now() + Duration::from_secs(30),
                BudgetLimits::server_ceiling().maximums(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ContextEvidencePortErrorKind::Cancelled);
    assert_eq!(error.usage, expected);
    assert_eq!(harness.calls.lock().unwrap().len(), 1);
}
