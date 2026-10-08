//! Selected-source metadata quotes conserve received work across sequential batches.
//! Cumulative bounds retain first-seen full references and never imply body freshness.

use super::*;

pub(super) async fn quote_source_metadata_with_progress<P>(
    port: Arc<P>,
    request: ContextSourceMetadataQuoteRequest,
    reservation: BudgetCharge,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ContextSourceMetadataQuoteOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    if request.targets.is_empty()
        || request.targets.len() > rootlight_agent::context_pack::MAX_PACK_ITEMS
    {
        return Err(invalid_context_evidence_response());
    }
    let mut sources = Vec::with_capacity(request.targets.len());
    let mut references = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        if cancellation.is_cancelled() {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Cancelled,
                BudgetCharge::default(),
            ));
        }
        let source = &target.source_ref;
        if source.repository() != request.repository || source.generation() != request.generation {
            return Err(invalid_context_evidence_response());
        }
        if sources.contains(source) {
            continue;
        }
        let span = source.span();
        references.push(
            client::SourceReference::new(
                source.repository(),
                source.generation(),
                span.file(),
                span.start_byte()..span.end_byte(),
                source.content_hash(),
                source
                    .line_hint()
                    .map(|hint| hint.start_line()..=hint.end_line()),
            )
            .map_err(|_| invalid_context_evidence_response())?,
        );
        sources.push(source.clone());
    }
    let mut usage = BudgetCharge::default();
    let mut completeness = ResultCompleteness::complete();
    let mut entries = Vec::with_capacity(sources.len());
    let mut previous_batches = ContextSourceMetadataFixedUpper::default();
    for (batch_index, batch) in references.chunks(32).enumerate() {
        if cancellation.is_cancelled() {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Cancelled,
                usage,
            ));
        }
        let share = match context_evidence_budget_share(reservation, usage, 1) {
            Ok(share) => share,
            Err(resource) => {
                completeness = context_evidence_budget_truncation(resource)
                    .map_err(|error| context_evidence_error(error.kind, usage))?;
                break;
            }
        };
        let options = context_evidence_options(share)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        let response = port
            .source_text_prefix_metadata_quote(
                SourceReadPortRequest {
                    repository: request.repository,
                    generation: client::GenerationSelector::Generation(request.generation),
                    selector_count: batch.len(),
                    reference_selector_indexes: Vec::new(),
                    symbol_selectors: Vec::new(),
                    references: batch.to_vec(),
                    context_lines_before: u8::from(request.include_snippets),
                    context_lines_after: u8::from(request.include_snippets),
                    merge_overlaps: false,
                    include_line_numbers: false,
                    encoding: SourceEncodingRequest::Utf8LosslessWhenValid,
                },
                options,
                cancellation.clone(),
            )
            .await
            .map_err(|error| map_context_source_client_error(error, usage))?;
        let delta = context_evidence_usage(&response.context);
        progress.record(delta);
        usage = add_context_evidence_usage(usage, delta);
        let invalid =
            || context_evidence_error(ContextEvidencePortErrorKind::InvalidResponse, usage);
        if response.context.repository != request.repository
            || response.context.generation != request.generation
            || response.context.parent_generation == Some(request.generation)
            || response.quote_policy_version != 1
            || response.context.usage.source_bytes != 0
            || response.context.usage.results
                != u64::try_from(response.entries.len()).unwrap_or(u64::MAX)
            || response.entries.len() > batch.len()
            || (response.execution_completeness.state == client::ResultCompletenessState::Complete
                && response.entries.len() != batch.len())
        {
            return Err(invalid());
        }
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.execution_completeness.clone())
                .map_err(|error| context_evidence_error(error.kind, usage))?,
        )
        .map_err(|error| context_evidence_error(error.kind, usage))?;
        let mut previous = ContextSourceMetadataFixedUpper::default();
        for (index, entry) in response.entries.into_iter().enumerate() {
            let expected = sources.get(batch_index * 32 + index).ok_or_else(invalid)?;
            if client_source_ref(&entry.source).map_err(|_| invalid())? != *expected {
                return Err(invalid());
            }
            let current = ContextSourceMetadataFixedUpper {
                json_bytes: entry.prefix_fixed_upper.json_bytes,
                memory_bytes: entry.prefix_fixed_upper.memory_bytes,
                rows: entry.prefix_fixed_upper.rows,
                traversal_facts: entry.prefix_fixed_upper.traversal_facts,
                paths: entry.prefix_fixed_upper.paths,
            };
            let count = u64::try_from(index + 1).map_err(|_| invalid())?;
            if !monotone(previous, current)
                || current.json_bytes == 0
                || current.memory_bytes == 0
                || current.rows < count
                || current.paths < count
                || usize::try_from(current.json_bytes).is_err()
                || usize::try_from(current.memory_bytes).is_err()
            {
                return Err(invalid());
            }
            let cumulative = checked_add(previous_batches, current).ok_or_else(invalid)?;
            entries.push(ContextSourceMetadataQuoteEntry {
                source_ref: expected.clone(),
                prefix_fixed_upper: cumulative,
            });
            previous = current;
        }
        previous_batches = checked_add(previous_batches, previous).ok_or_else(invalid)?;
        if completeness.state != CompletenessState::Complete {
            break;
        }
    }
    if cancellation.is_cancelled() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            usage,
        ));
    }
    Ok(ContextSourceMetadataQuoteOutput {
        repository: request.repository,
        generation: request.generation,
        quote_policy_version: 1,
        entries,
        completeness,
        usage,
    })
}

fn monotone(
    previous: ContextSourceMetadataFixedUpper,
    next: ContextSourceMetadataFixedUpper,
) -> bool {
    next.json_bytes >= previous.json_bytes
        && next.memory_bytes >= previous.memory_bytes
        && next.rows >= previous.rows
        && next.traversal_facts >= previous.traversal_facts
        && next.paths >= previous.paths
}

fn checked_add(
    left: ContextSourceMetadataFixedUpper,
    right: ContextSourceMetadataFixedUpper,
) -> Option<ContextSourceMetadataFixedUpper> {
    Some(ContextSourceMetadataFixedUpper {
        json_bytes: left.json_bytes.checked_add(right.json_bytes)?,
        memory_bytes: left.memory_bytes.checked_add(right.memory_bytes)?,
        rows: left.rows.checked_add(right.rows)?,
        traversal_facts: left.traversal_facts.checked_add(right.traversal_facts)?,
        paths: left.paths.checked_add(right.paths)?,
    })
}
