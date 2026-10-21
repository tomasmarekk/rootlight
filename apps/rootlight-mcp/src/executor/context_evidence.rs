//! Production adapters for typed context-pack evidence providers.

use super::*;
use rootlight_agent::context_evidence::{
    ContextSourceMaterial, ContextSourceMetadataFixedUpper, ContextSourceMetadataQuoteEntry,
    ContextSourceMetadataQuoteOutput, ContextSourceMetadataQuoteRequest, ContextSourceOutput,
    ContextSourceProjection, ContextSourceRequest, ContextSourceSnippet,
};

#[path = "source_metadata_quote.rs"]
mod source_metadata_quote;

const CONTEXT_SIGNATURE_BYTES: u32 = 1_024;
const CONTEXT_DEFINITION_CANDIDATE_TOKENS: usize = 384;
const FOCUSED_SOURCE_LINES_BEFORE: u8 = 1;
const FOCUSED_SOURCE_LINES_AFTER: u8 = 1;

/// Received child receipts outlive a dropped provider future. Pending work has
/// no receipt and is deliberately not inferred from its requested reservation.
#[derive(Default)]
pub(super) struct ContextWorkProgress {
    usage: std::sync::Mutex<BudgetCharge>,
}

impl ContextWorkProgress {
    fn record(&self, delta: BudgetCharge) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *usage = add_context_evidence_usage(*usage, delta);
    }

    fn snapshot(&self) -> BudgetCharge {
        *self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) async fn await_context_operation<T, F, D>(
    operation: F,
    mut cancellation: RequestCancellation,
    deadline: D,
    progress: &ContextWorkProgress,
) -> Result<T, ContextEvidencePortError>
where
    F: Future<Output = Result<T, ContextEvidencePortError>>,
    D: Future<Output = ()>,
{
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            progress.snapshot(),
        )),
        _ = deadline => Err(context_evidence_error(
            ContextEvidencePortErrorKind::DeadlineExceeded,
            progress.snapshot(),
        )),
        response = operation => response.map_err(|error| context_evidence_error(
            error.kind,
            join_context_evidence_usage(error.usage, progress.snapshot()),
        )),
    }
}

impl<P> ContextEvidencePort<RequestCancellation> for McpAgentToolPort<P>
where
    P: FirstSliceClientPort,
{
    fn retrieve(
        &self,
        invocation: EvidenceProviderInvocation,
        context: ContextEvidenceCallContext<RequestCancellation>,
    ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
        let port = Arc::clone(&self.port);
        let deadline = context.deadline();
        let reservation = context.reservation();
        let cancellation = context.cancellation().clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(context_evidence_error(
                    ContextEvidencePortErrorKind::Cancelled,
                    BudgetCharge::default(),
                ));
            }
            let progress = ContextWorkProgress::default();
            let operation = retrieve_context_evidence(
                port,
                invocation,
                reservation,
                cancellation.clone(),
                &progress,
            );
            await_context_operation(
                operation,
                cancellation,
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)),
                &progress,
            )
            .await
            .map_err(project_context_evidence_error)
        })
    }

    fn quote_source_metadata(
        &self,
        request: ContextSourceMetadataQuoteRequest,
        context: ContextEvidenceCallContext<RequestCancellation>,
    ) -> AgentPortFuture<Result<ContextSourceMetadataQuoteOutput, ContextEvidencePortError>> {
        let port = Arc::clone(&self.port);
        let deadline = context.deadline();
        let reservation = context.reservation();
        let cancellation = context.cancellation().clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(context_evidence_error(
                    ContextEvidencePortErrorKind::Cancelled,
                    BudgetCharge::default(),
                ));
            }
            let progress = ContextWorkProgress::default();
            let operation = source_metadata_quote::quote_source_metadata_with_progress(
                port,
                request,
                reservation,
                cancellation.clone(),
                &progress,
            );
            await_context_operation(
                operation,
                cancellation,
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)),
                &progress,
            )
            .await
        })
    }

    fn materialize_source(
        &self,
        request: ContextSourceRequest,
        context: ContextEvidenceCallContext<RequestCancellation>,
    ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
        let port = Arc::clone(&self.port);
        let deadline = context.deadline();
        let reservation = context.reservation();
        let cancellation = context.cancellation().clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(context_evidence_error(
                    ContextEvidencePortErrorKind::Cancelled,
                    BudgetCharge::default(),
                ));
            }
            let progress = ContextWorkProgress::default();
            let operation = materialize_context_source_with_progress(
                port,
                request,
                reservation,
                cancellation.clone(),
                &progress,
            );
            await_context_operation(
                operation,
                cancellation,
                tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)),
                &progress,
            )
            .await
        })
    }
}

#[cfg(test)]
pub(super) async fn materialize_context_source<P>(
    port: Arc<P>,
    request: ContextSourceRequest,
    reservation: BudgetCharge,
    cancellation: RequestCancellation,
) -> Result<ContextSourceOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    materialize_context_source_with_progress(
        port,
        request,
        reservation,
        cancellation,
        &ContextWorkProgress::default(),
    )
    .await
}

pub(super) async fn materialize_context_source_with_progress<P>(
    port: Arc<P>,
    request: ContextSourceRequest,
    reservation: BudgetCharge,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ContextSourceOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    if request.targets.is_empty() || request.max_bytes_per_snippet == 0 {
        return Err(invalid_context_evidence_response());
    }
    let context_lines_before = if request.include_snippets {
        FOCUSED_SOURCE_LINES_BEFORE
    } else {
        0
    };
    let context_lines_after = if request.include_snippets {
        FOCUSED_SOURCE_LINES_AFTER
    } else {
        0
    };
    let mut references = Vec::with_capacity(request.targets.len());
    let mut reference_sources = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        let source = &target.source_ref;
        if source.repository() != request.repository || source.generation() != request.generation {
            return Err(invalid_context_evidence_response());
        }
        if reference_sources.contains(source) {
            continue;
        }
        let span = source.span();
        let lines = source
            .line_hint()
            .map(|lines| lines.start_line()..=lines.end_line());
        let reference = client::SourceReference::new(
            source.repository(),
            source.generation(),
            span.file(),
            span.start_byte()..span.end_byte(),
            source.content_hash(),
            lines,
        )
        .map_err(|_| invalid_context_evidence_response())?;
        reference_sources.push(source.clone());
        references.push(reference);
    }
    let mut usage = BudgetCharge::default();
    let mut completeness = ResultCompleteness::complete();
    let mut read_materials = Vec::with_capacity(reference_sources.len());
    // Large packs use sequential reads within the existing selector ceiling.
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
                completeness = merge_context_evidence_completeness(
                    completeness,
                    context_evidence_budget_truncation(resource)
                        .map_err(|error| context_evidence_error(error.kind, usage))?,
                )
                .map_err(|error| context_evidence_error(error.kind, usage))?;
                break;
            }
        };
        let options = context_evidence_options(share)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        let prefix = port
            .source_read_lossless_text_prefix(
                SourceReadPortRequest {
                    repository: request.repository,
                    generation: client::GenerationSelector::Generation(request.generation),
                    selector_count: batch.len(),
                    reference_selector_indexes: Vec::new(),
                    symbol_selectors: Vec::new(),
                    references: batch.to_vec(),
                    context_lines_before,
                    context_lines_after,
                    merge_overlaps: false,
                    include_line_numbers: false,
                    encoding: SourceEncodingRequest::Utf8LosslessWhenValid,
                },
                request.max_bytes_per_snippet,
                options,
                cancellation.clone(),
            )
            .await
            .map_err(|error| map_context_source_client_error(error, usage))?;
        progress.record(context_evidence_usage(&prefix.result.context));
        let prefix_truncated = prefix.prefix_truncated;
        let response = prefix.result;
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.context));
        let invalid =
            || context_evidence_error(ContextEvidencePortErrorKind::InvalidResponse, usage);
        if response.context.repository != request.repository
            || response.context.generation != request.generation
            || response.context.parent_generation == Some(request.generation)
            || !context_source_cardinality_matches(
                batch.len(),
                response.chunks.len(),
                response.truncated,
                response.execution_completeness.state,
            )
            || u64::try_from(response.chunks.len()).ok() != Some(response.context.usage.results)
        {
            return Err(invalid());
        }
        if let Some(recovery) = &response.source_recovery {
            super::source_recovery::validate_analysis(
                Some(recovery),
                &response.context,
                response.chunks.iter().map(|chunk| chunk.tier),
            )
            .map_err(|_| invalid())?;
            let returned: Vec<_> = response
                .chunks
                .iter()
                .map(|chunk| chunk.source.file())
                .collect();
            super::source_recovery::validate_membership(recovery, &returned)
                .map_err(|_| invalid())?;
        }
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.execution_completeness.clone())
                .map_err(|error| context_evidence_error(error.kind, usage))?,
        )
        .map_err(|error| context_evidence_error(error.kind, usage))?;
        if prefix_truncated && !request.include_snippets {
            // Signature-only material has no per-snippet truncation marker.
            // Do not present a clipped declaration as complete source evidence.
            completeness = merge_context_evidence_completeness(
                completeness,
                context_evidence_budget_truncation(ContractLimitingResourceKind::SourceBytes)
                    .map_err(|error| context_evidence_error(error.kind, usage))?,
            )
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        }
        let mut physical_bytes = 0_u64;
        let mut text_bytes = 0_u64;
        let mut approved_range_clipped = false;
        for (index, chunk) in response.chunks.into_iter().enumerate() {
            let requested_source = &reference_sources[batch_index * 32 + index];
            let returned_source = client_source_ref(&chunk.source).map_err(|_| invalid())?;
            let content_bytes = u64::try_from(chunk.content.len()).map_err(|_| invalid())?;
            if &returned_source != requested_source
                || chunk.start_byte > requested_source.span().start_byte()
                || chunk.content_hash != requested_source.content_hash()
                || chunk.start_line.is_some()
                || chunk.end_line.is_some()
                || content_bytes > u64::from(request.max_bytes_per_snippet)
                || (chunk.end_byte < requested_source.span().end_byte() && !prefix_truncated)
                || (context_lines_before == 0
                    && chunk.start_byte != requested_source.span().start_byte())
                || (context_lines_after == 0 && chunk.end_byte > requested_source.span().end_byte())
            {
                return Err(invalid());
            }
            approved_range_clipped |= chunk.end_byte < requested_source.span().end_byte();
            let physical = context_text_physical_bytes(
                &chunk.content,
                chunk.physical_encoding,
                chunk.start_byte,
                chunk.end_byte,
                &cancellation,
            )
            .map_err(|error| context_evidence_error(error.kind, usage))?;
            physical_bytes = physical_bytes.checked_add(physical).ok_or_else(invalid)?;
            text_bytes = text_bytes.checked_add(content_bytes).ok_or_else(invalid)?;
            let projection = ContextSourceProjection {
                start_byte: chunk.start_byte,
                end_byte: chunk.end_byte,
                encoding: match chunk.physical_encoding {
                    client::SourceTextEncoding::Utf8 => None,
                    client::SourceTextEncoding::Utf16Le => {
                        Some(rootlight_ir::SourceTextEncoding::Utf16Le)
                    }
                    client::SourceTextEncoding::Utf16Be => {
                        Some(rootlight_ir::SourceTextEncoding::Utf16Be)
                    }
                },
                text_bytes: content_bytes,
            };
            let selected = context_selected_prefix(&chunk, requested_source, &cancellation)
                .map_err(|error| context_evidence_error(error.kind, usage))?;
            let Some(selected) = selected.filter(|text| !text.trim().is_empty()) else {
                completeness = merge_context_evidence_completeness(
                    completeness,
                    context_evidence_budget_truncation(ContractLimitingResourceKind::SourceBytes)
                        .map_err(|error| context_evidence_error(error.kind, usage))?,
                )
                .map_err(|error| context_evidence_error(error.kind, usage))?;
                continue;
            };
            let signature = bounded_context_signature(
                selected,
                usize::try_from(request.max_bytes_per_snippet.min(CONTEXT_SIGNATURE_BYTES))
                    .unwrap_or(usize::MAX),
            );
            let snippet = request.include_snippets.then_some(ContextSourceSnippet {
                content: chunk.content,
                language: chunk.language,
                truncated: prefix_truncated || response.truncated,
            });
            // Editing authority is the complete selection, not its preview.
            read_materials.push((requested_source.clone(), projection, signature, snippet));
        }
        if physical_bytes != response.total_source_bytes
            || text_bytes != response.total_text_bytes
            || physical_bytes != response.context.usage.source_bytes
            || (context_lines_after == 0 && prefix_truncated != approved_range_clipped)
        {
            return Err(invalid());
        }
        if response.source_recovery.is_some() {
            // This context vocabulary cannot retain the recovery witness.
            // Validate the bounded read first and conserve received work;
            // exact bytes remain available through standalone source.read 1.4.
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Unsupported,
                usage,
            ));
        }
    }
    let mut materials = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        let Some((source_ref, projection, signature, snippet)) = read_materials
            .iter()
            .find(|(source_ref, _, _, _)| source_ref == &target.source_ref)
        else {
            continue;
        };
        materials.push(ContextSourceMaterial {
            candidate_id: target.candidate_id.clone(),
            source_ref: source_ref.clone(),
            projection: projection.clone(),
            signature: signature.clone(),
            snippet: snippet.clone(),
        });
    }
    if cancellation.is_cancelled() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            usage,
        ));
    }
    Ok(ContextSourceOutput {
        repository: request.repository,
        generation: request.generation,
        materials,
        completeness,
        usage,
    })
}

fn context_selected_prefix<'a>(
    chunk: &'a client::SourceTextChunk,
    source: &SourceRef,
    cancellation: &RequestCancellation,
) -> Result<Option<&'a str>, ContextEvidencePortError> {
    if chunk.end_byte <= source.span().start_byte() {
        return Ok(None);
    }
    let selected_end = chunk.end_byte.min(source.span().end_byte());
    let mut physical = chunk.start_byte;
    let mut start = None;
    let mut end = None;
    for (index, (text_offset, character)) in chunk.content.char_indices().enumerate() {
        if index.is_multiple_of(1_024) && cancellation.is_cancelled() {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Cancelled,
                BudgetCharge::default(),
            ));
        }
        if physical == source.span().start_byte() {
            start = Some(text_offset);
        }
        if physical == selected_end {
            end = Some(text_offset);
            break;
        }
        let width = match chunk.physical_encoding {
            client::SourceTextEncoding::Utf8 => character.len_utf8(),
            client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => {
                character.len_utf16() * 2
            }
        };
        physical = physical
            .checked_add(u64::try_from(width).map_err(|_| invalid_context_evidence_response())?)
            .ok_or_else(invalid_context_evidence_response)?;
    }
    if end.is_none() && physical == selected_end {
        end = Some(chunk.content.len());
    }
    match (start, end) {
        (Some(start), Some(end)) if start <= end => Ok(Some(&chunk.content[start..end])),
        _ => Err(invalid_context_evidence_response()),
    }
}

fn bounded_context_signature(content: &str, maximum_bytes: usize) -> Option<String> {
    let declaration = content.lines().find(|line| !line.trim().is_empty())?.trim();
    let boundary = declaration
        .find(['{', ';'])
        .map_or(declaration.len(), |index| index.saturating_add(1));
    let (signature, _) =
        truncate_context_source(declaration[..boundary].to_owned(), maximum_bytes.min(4_096));
    (!signature.is_empty()).then_some(signature)
}

fn truncate_context_source(mut content: String, maximum_bytes: usize) -> (String, bool) {
    if content.len() <= maximum_bytes {
        return (content, false);
    }
    let mut boundary = maximum_bytes;
    while boundary > 0 && !content.is_char_boundary(boundary) {
        boundary -= 1;
    }
    content.truncate(boundary);
    (content, true)
}

async fn retrieve_context_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    reservation: BudgetCharge,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let anchor_lookups = context_evidence_anchor_lookups(&invocation);
    let options = match invocation.provider() {
        EvidenceProvider::Relationships => {
            relationship_evidence_options(reservation, anchor_lookups)?
        }
        EvidenceProvider::Implementation | EvidenceProvider::Source => {
            source_evidence_options(reservation, anchor_lookups)?
        }
        EvidenceProvider::Tests | EvidenceProvider::ChangeImpact | EvidenceProvider::Planning => {
            composite_context_evidence_options(reservation, anchor_lookups)?
        }
        _ => context_evidence_options(reservation)?,
    };
    match invocation.provider() {
        EvidenceProvider::Locate => {
            retrieve_located_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::Definition => {
            retrieve_definition_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::Implementation | EvidenceProvider::Source => {
            retrieve_source_evidence_with_progress(
                port,
                invocation,
                reservation,
                options,
                cancellation,
                progress,
            )
            .await
        }
        EvidenceProvider::Relationships => {
            retrieve_relationship_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::Tests => {
            retrieve_test_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::Architecture => {
            retrieve_architecture_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::ChangeImpact => {
            retrieve_change_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::History => {
            retrieve_history_evidence(port, invocation, options, cancellation, progress).await
        }
        EvidenceProvider::Planning => {
            retrieve_planning_evidence(port, invocation, options, cancellation, progress).await
        }
    }
}

pub(super) fn context_evidence_options(
    reservation: BudgetCharge,
) -> Result<client::RequestOptions, ContextEvidencePortError> {
    // The daemon measures its rich internal response with a UTF-8 byte upper
    // bound, while the agent reservation estimates compact pack candidates.
    // The existing JSON envelope bounds that transport-only dimension.
    let transport_reservation = BudgetCharge {
        tokens: reservation.tokens.max(reservation.json_bytes),
        ..reservation
    };
    AnalyticalBudget::from_limits(BudgetLimits::from_maximums(transport_reservation))
        .map(|value| value.options)
        .map_err(|_| invalid_context_evidence_response())
}

fn context_evidence_anchor_lookups(invocation: &EvidenceProviderInvocation) -> usize {
    invocation
        .anchors()
        .iter()
        .filter(|anchor| match anchor {
            EvidenceAnchor::Path(_) | EvidenceAnchor::Route(_) => true,
            EvidenceAnchor::Change(value) | EvidenceAnchor::Plan(value) => {
                value.parse::<SymbolId>().is_err() && value.parse::<GenerationId>().is_err()
            }
            EvidenceAnchor::Located(value) => value.parse::<SymbolId>().is_err(),
            EvidenceAnchor::Symbol(_) | EvidenceAnchor::Test(_) => false,
        })
        .count()
}

fn composite_context_evidence_options(
    reservation: BudgetCharge,
    anchor_lookups: usize,
) -> Result<client::RequestOptions, ContextEvidencePortError> {
    let maximum_calls = u64::try_from(anchor_lookups)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let share = context_evidence_budget_share(reservation, BudgetCharge::default(), maximum_calls)
        .map_err(|_| invalid_context_evidence_response())?;
    context_evidence_options(share)
}

fn remaining_context_evidence_options(
    invocation: &EvidenceProviderInvocation,
    prior_usage: BudgetCharge,
) -> Result<client::RequestOptions, ContextEvidencePortError> {
    let share = context_evidence_budget_share(invocation.reservation(), prior_usage, 1)
        .map_err(|_| invalid_context_evidence_response())?;
    context_evidence_options(share)
}

pub(super) fn relationship_evidence_options(
    reservation: BudgetCharge,
    anchor_lookups: usize,
) -> Result<client::RequestOptions, ContextEvidencePortError> {
    // Anchor resolution and relationship discovery must leave one share for
    // dynamically partitioned explanations of the discovered symbols.
    let maximum_calls = u64::try_from(anchor_lookups)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    let share = context_evidence_budget_share(reservation, BudgetCharge::default(), maximum_calls)
        .map_err(|_| invalid_context_evidence_response())?;
    context_evidence_options(share)
}

pub(super) fn source_evidence_options(
    reservation: BudgetCharge,
    anchor_lookups: usize,
) -> Result<client::RequestOptions, ContextEvidencePortError> {
    // Fixed lookup shares leave work for definition explanations and the exact
    // read. Those later stages redistribute only the measured unused remainder.
    let maximum_calls = u64::try_from(anchor_lookups)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    let share = source_evidence_budget_share(reservation, BudgetCharge::default(), maximum_calls)
        .map_err(|_| invalid_context_evidence_response())?;
    context_evidence_options(share)
}

fn source_evidence_budget_share(
    reservation: BudgetCharge,
    usage: BudgetCharge,
    remaining_calls: u64,
) -> Result<BudgetCharge, ContractLimitingResourceKind> {
    // Internal explanation records and transport tokens are projected into
    // candidates at the provider boundary. Paths/depth are per-call ceilings,
    // not additive source work; dividing a single permitted path would reject
    // every otherwise valid one-anchor explanation followed by an exact read.
    let measured_work = BudgetCharge {
        results: 0,
        tokens: 0,
        actual_tokens: 0,
        depth: 0,
        paths: 0,
        ..usage
    };
    let mut share = context_evidence_budget_share(reservation, measured_work, 1)?;
    for (value, resource) in [
        (&mut share.rows, ContractLimitingResourceKind::Rows),
        (
            &mut share.source_bytes,
            ContractLimitingResourceKind::SourceBytes,
        ),
        (
            &mut share.traversal_facts,
            ContractLimitingResourceKind::Edges,
        ),
        (
            &mut share.json_bytes,
            ContractLimitingResourceKind::ResponseBytes,
        ),
        (
            &mut share.memory_bytes,
            ContractLimitingResourceKind::MemoryBytes,
        ),
    ] {
        *value = value.checked_div(remaining_calls).unwrap_or(0);
        if *value == 0 {
            return Err(resource);
        }
    }
    Ok(share)
}

async fn explain_source_context_symbols<P>(
    port: Arc<P>,
    invocation: &EvidenceProviderInvocation,
    symbols: Vec<SymbolId>,
    reservation: BudgetCharge,
    prior_usage: BudgetCharge,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ContextSymbolExplanations, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let mut explanations = Vec::with_capacity(symbols.len());
    let mut completeness = ResultCompleteness::complete();
    let mut usage = BudgetCharge::default();
    for (index, symbol) in symbols.iter().copied().enumerate() {
        if cancellation.is_cancelled() {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Cancelled,
                usage,
            ));
        }
        let remaining_explanations =
            u64::try_from(symbols.len().saturating_sub(index)).unwrap_or(u64::MAX);
        let consumed = add_context_evidence_usage(prior_usage, usage);
        let share = source_evidence_budget_share(reservation, consumed, remaining_explanations)
            .and_then(|mut share| {
                // The exact read still needs to inspect rows even when the
                // final explanation legitimately exhausts another resource.
                share.rows = reservation
                    .rows
                    .saturating_sub(consumed.rows)
                    .checked_div(remaining_explanations.saturating_add(1))
                    .unwrap_or(0);
                if share.rows == 0 {
                    Err(ContractLimitingResourceKind::Rows)
                } else {
                    Ok(share)
                }
            });
        let share = match share {
            Ok(share) => share,
            Err(resource) => {
                completeness = merge_context_evidence_completeness(
                    completeness,
                    context_evidence_budget_truncation(resource)
                        .map_err(|error| context_evidence_error(error.kind, usage))?,
                )
                .map_err(|error| context_evidence_error(error.kind, usage))?;
                break;
            }
        };
        let options = context_evidence_options(share)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        let response = port
            .symbol_explain(
                symbol_explain_request(invocation, vec![symbol]),
                options,
                cancellation.clone(),
            )
            .await
            .map_err(|error| map_context_source_client_error(error, usage))?;
        progress.record(context_evidence_usage(&response.result.context));
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.result.context));
        validate_context_evidence_identity(invocation, &response.result.context)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.result.execution_completeness.clone())
                .map_err(|error| context_evidence_error(error.kind, usage))?,
        )
        .map_err(|error| context_evidence_error(error.kind, usage))?;
        explanations.extend(response.result.symbols);
    }
    Ok(ContextSymbolExplanations {
        symbols: explanations,
        completeness,
        usage,
    })
}

const fn context_evidence_error(
    kind: ContextEvidencePortErrorKind,
    usage: BudgetCharge,
) -> ContextEvidencePortError {
    ContextEvidencePortError { kind, usage }
}

fn project_context_evidence_error(mut error: ContextEvidencePortError) -> ContextEvidencePortError {
    // A failed provider emits no pack candidates. Keep its measured work, but
    // do not charge the internal transport's results and tokens as candidates.
    error.usage.results = 0;
    error.usage.tokens = 0;
    error
}

const fn invalid_context_evidence_response() -> ContextEvidencePortError {
    context_evidence_error(
        ContextEvidencePortErrorKind::InvalidResponse,
        BudgetCharge {
            rows: 0,
            results: 0,
            tokens: 0,
            actual_tokens: 0,
            source_bytes: 0,
            traversal_facts: 0,
            depth: 0,
            paths: 0,
            json_bytes: 0,
            memory_bytes: 0,
            time_ms: 0,
        },
    )
}

const fn unsupported_context_evidence() -> ContextEvidencePortError {
    context_evidence_error(
        ContextEvidencePortErrorKind::Unsupported,
        BudgetCharge {
            rows: 0,
            results: 0,
            tokens: 0,
            actual_tokens: 0,
            source_bytes: 0,
            traversal_facts: 0,
            depth: 0,
            paths: 0,
            json_bytes: 0,
            memory_bytes: 0,
            time_ms: 0,
        },
    )
}

fn map_context_evidence_client_error(error: ClientPortError) -> ContextEvidencePortError {
    let kind = match error {
        ClientPortError::Public(error) if error.code() == ErrorCode::UnsupportedCapability => {
            ContextEvidencePortErrorKind::Unsupported
        }
        ClientPortError::InvalidResponse => ContextEvidencePortErrorKind::InvalidResponse,
        ClientPortError::Public(_) | ClientPortError::Transport | ClientPortError::Executor => {
            ContextEvidencePortErrorKind::Unavailable
        }
    };
    context_evidence_error(kind, BudgetCharge::default())
}

fn context_evidence_completeness(
    completeness: client::ResultCompleteness,
) -> Result<ResultCompleteness, ContextEvidencePortError> {
    contract_completeness(completeness).map_err(|_| invalid_context_evidence_response())
}

fn context_evidence_usage(context: &client::QueryContext) -> BudgetCharge {
    BudgetCharge {
        rows: context.usage.rows,
        results: context.usage.results,
        tokens: context.usage.estimated_tokens,
        actual_tokens: 0,
        source_bytes: context.usage.source_bytes,
        traversal_facts: context.usage.edges,
        depth: 0,
        paths: 0,
        json_bytes: context.usage.json_bytes,
        memory_bytes: context.usage.memory_bytes.unwrap_or(0),
        time_ms: context.usage.elapsed_micros.div_ceil(1_000),
    }
}

fn add_context_evidence_usage(left: BudgetCharge, right: BudgetCharge) -> BudgetCharge {
    BudgetCharge {
        rows: left.rows.saturating_add(right.rows),
        results: left.results.saturating_add(right.results),
        tokens: left.tokens.saturating_add(right.tokens),
        actual_tokens: left.actual_tokens.saturating_add(right.actual_tokens),
        source_bytes: left.source_bytes.saturating_add(right.source_bytes),
        traversal_facts: left.traversal_facts.saturating_add(right.traversal_facts),
        depth: left.depth.max(right.depth),
        paths: left.paths.saturating_add(right.paths),
        json_bytes: left.json_bytes.saturating_add(right.json_bytes),
        memory_bytes: left.memory_bytes.saturating_add(right.memory_bytes),
        time_ms: left.time_ms.max(right.time_ms),
    }
}

fn join_context_evidence_usage(left: BudgetCharge, right: BudgetCharge) -> BudgetCharge {
    // Both are cumulative, overlapping receipts, not independent work deltas.
    BudgetCharge {
        rows: left.rows.max(right.rows),
        results: left.results.max(right.results),
        tokens: left.tokens.max(right.tokens),
        actual_tokens: left.actual_tokens.max(right.actual_tokens),
        source_bytes: left.source_bytes.max(right.source_bytes),
        traversal_facts: left.traversal_facts.max(right.traversal_facts),
        depth: left.depth.max(right.depth),
        paths: left.paths.max(right.paths),
        json_bytes: left.json_bytes.max(right.json_bytes),
        memory_bytes: left.memory_bytes.max(right.memory_bytes),
        time_ms: left.time_ms.max(right.time_ms),
    }
}

pub(super) fn context_evidence_budget_share(
    reservation: BudgetCharge,
    usage: BudgetCharge,
    remaining_calls: u64,
) -> Result<BudgetCharge, ContractLimitingResourceKind> {
    if remaining_calls == 0 {
        return Err(ContractLimitingResourceKind::Results);
    }
    let share = BudgetCharge {
        rows: reservation
            .rows
            .saturating_sub(usage.rows)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        results: reservation
            .results
            .saturating_sub(usage.results)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        tokens: reservation
            .tokens
            .saturating_sub(usage.tokens)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        actual_tokens: reservation
            .actual_tokens
            .saturating_sub(usage.actual_tokens)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        source_bytes: reservation
            .source_bytes
            .saturating_sub(usage.source_bytes)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        traversal_facts: reservation
            .traversal_facts
            .saturating_sub(usage.traversal_facts)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        depth: reservation.depth,
        paths: reservation
            .paths
            .saturating_sub(usage.paths)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        json_bytes: reservation
            .json_bytes
            .saturating_sub(usage.json_bytes)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        memory_bytes: reservation
            .memory_bytes
            .saturating_sub(usage.memory_bytes)
            .checked_div(remaining_calls)
            .unwrap_or(0),
        time_ms: reservation.time_ms,
    };
    for (value, resource) in [
        (share.rows, ContractLimitingResourceKind::Rows),
        (share.results, ContractLimitingResourceKind::Results),
        (
            share.source_bytes,
            ContractLimitingResourceKind::SourceBytes,
        ),
        (share.traversal_facts, ContractLimitingResourceKind::Edges),
        (share.paths, ContractLimitingResourceKind::Paths),
        (
            share.json_bytes,
            ContractLimitingResourceKind::ResponseBytes,
        ),
        (
            share.memory_bytes,
            ContractLimitingResourceKind::MemoryBytes,
        ),
        (share.depth, ContractLimitingResourceKind::Depth),
        (share.time_ms, ContractLimitingResourceKind::Deadline),
    ] {
        if value == 0 {
            return Err(resource);
        }
    }
    Ok(share)
}

fn context_evidence_budget_truncation(
    resource: ContractLimitingResourceKind,
) -> Result<ResultCompleteness, ContextEvidencePortError> {
    ResultCompleteness::new(
        CompletenessState::Truncated,
        vec![ContractLimitingResource::kind(resource)],
        ContinuationAvailability::Unavailable,
        vec![ContinuationGuidance::SplitRequest],
    )
    .map_err(|_| invalid_context_evidence_response())
}

fn merge_context_evidence_completeness(
    left: ResultCompleteness,
    right: ResultCompleteness,
) -> Result<ResultCompleteness, ContextEvidencePortError> {
    left.merge(&right)
        .map_err(|_| invalid_context_evidence_response())
}

fn validate_context_evidence_identity(
    invocation: &EvidenceProviderInvocation,
    context: &client::QueryContext,
) -> Result<(), ContextEvidencePortError> {
    if context.repository != invocation.repository()
        || context.generation != invocation.generation()
        || context.parent_generation == Some(context.generation)
    {
        return Err(invalid_context_evidence_response());
    }
    Ok(())
}

fn make_context_evidence_output(
    invocation: &EvidenceProviderInvocation,
    observations: Vec<EvidenceProviderObservation>,
    completeness: ResultCompleteness,
    usage: BudgetCharge,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError> {
    let transport_ceiling = usize::from(invocation.max_candidates()).saturating_add(usize::from(
        invocation.provider() == EvidenceProvider::ChangeImpact,
    ));
    if observations.len() > transport_ceiling {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::InvalidResponse,
            usage,
        ));
    }
    // Transport usage describes the daemon's rich internal envelope. The
    // parent ledger governs projected pack candidates, so only results and
    // tokens are replaced while measured work dimensions remain intact.
    let usage = BudgetCharge {
        results: u64::try_from(observations.len()).unwrap_or(u64::MAX),
        tokens: observations.iter().fold(0_u64, |total, value| {
            total.saturating_add(value.estimated_tokens)
        }),
        ..usage
    };
    Ok(EvidenceProviderOutput {
        repository: invocation.repository(),
        generation: invocation.generation(),
        invocation: invocation.id().clone(),
        observations,
        completeness,
        usage,
    })
}

fn context_source_refs(
    references: &[client::SourceReference],
) -> Result<Vec<SourceRef>, ContextEvidencePortError> {
    if references.len() > rootlight_agent::context_evidence::MAX_CANDIDATE_LINKS {
        return Err(invalid_context_evidence_response());
    }
    references
        .iter()
        .map(|reference| {
            client_source_ref(reference).map_err(|_| invalid_context_evidence_response())
        })
        .collect()
}

#[derive(Debug)]
struct ResolvedEvidenceAnchors {
    symbols: Vec<SymbolId>,
    hits: Vec<client::LocateHit>,
    completeness: ResultCompleteness,
    usage: BudgetCharge,
}

async fn resolve_context_evidence_anchors<P>(
    port: Arc<P>,
    invocation: &EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ResolvedEvidenceAnchors, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let mut symbols = Vec::new();
    let mut queries = Vec::new();
    for anchor in invocation.anchors() {
        match anchor {
            EvidenceAnchor::Symbol(symbol) | EvidenceAnchor::Test(symbol) => {
                symbols.push(*symbol);
            }
            EvidenceAnchor::Path(path) => queries.push((path.clone(), LocateMode::Prefix)),
            EvidenceAnchor::Route(route) => queries.push((route.clone(), LocateMode::Text)),
            EvidenceAnchor::Change(value) | EvidenceAnchor::Plan(value) => {
                if let Ok(symbol) = value.parse() {
                    symbols.push(symbol);
                } else if value.parse::<GenerationId>().is_err() {
                    queries.push((value.clone(), LocateMode::Text));
                }
            }
            EvidenceAnchor::Located(value) => {
                if let Ok(symbol) = value.parse() {
                    symbols.push(symbol);
                } else {
                    queries.push((value.clone(), LocateMode::Text));
                }
            }
        }
    }
    symbols.sort_unstable();
    symbols.dedup();

    let mut hits = Vec::new();
    let mut completeness = ResultCompleteness::complete();
    let mut usage = BudgetCharge::default();
    let per_query = usize::from(invocation.max_candidates())
        .div_ceil(queries.len().max(1))
        .max(1);
    let maximum_results = u32::try_from(per_query).unwrap_or(u32::MAX);
    for (query, mode) in queries {
        // A path seed selects its physical file. The scoped source route avoids
        // expanding every qualified symbol in that file under a locate budget.
        let path_prefixes = if mode == LocateMode::Prefix {
            vec![query.clone()]
        } else {
            Vec::new()
        };
        let request = CodeLocatePortRequest {
            kinds: (mode == LocateMode::Prefix).then(|| vec!["file".to_owned()]),
            repository: invocation.repository(),
            generation: client::GenerationSelector::Generation(invocation.generation()),
            query,
            mode,
            languages: Vec::new(),
            path_prefixes,
            maximum_results,
            page_offset: 0,
        };
        let response = port
            .code_locate(request, options, cancellation.clone())
            .await
            .map_err(map_context_evidence_client_error)?;
        progress.record(context_evidence_usage(&response.result.context));
        validate_context_evidence_identity(invocation, &response.result.context)?;
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.result.context));
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.result.execution_completeness.clone())?,
        )?;
        for hit in response.result.hits {
            if let Some(symbol) = hit.symbol {
                symbols.push(symbol);
            }
            hits.push(hit);
        }
    }
    symbols.sort_unstable();
    symbols.dedup();
    if symbols.len() > usize::from(invocation.max_candidates()) {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::InvalidResponse,
            usage,
        ));
    }
    Ok(ResolvedEvidenceAnchors {
        symbols,
        hits,
        completeness,
        usage,
    })
}

fn symbol_explain_request(
    invocation: &EvidenceProviderInvocation,
    symbols: Vec<SymbolId>,
) -> SymbolExplainPortRequest {
    SymbolExplainPortRequest {
        repository: invocation.repository(),
        generation: client::GenerationSelector::Generation(invocation.generation()),
        symbols,
        sections: Vec::new(),
        relation_sample_limit: Some(0),
        source_preview_lines: Some(0),
        include_provenance: ProvenanceLevel::Compact,
    }
}

#[derive(Debug)]
struct ContextSymbolExplanations {
    symbols: Vec<client::SymbolExplanation>,
    completeness: ResultCompleteness,
    usage: BudgetCharge,
}

async fn explain_context_symbols<P>(
    port: Arc<P>,
    invocation: &EvidenceProviderInvocation,
    symbols: Vec<SymbolId>,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ContextSymbolExplanations, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let mut explanations = Vec::with_capacity(symbols.len());
    let mut completeness = ResultCompleteness::complete();
    let mut usage = BudgetCharge::default();
    // A batched symbol.explain shares one results ceiling across every symbol.
    // Isolating anchors prevents a relation-rich first symbol from starving the
    // remaining explicit seeds while retaining the same bounded child budget.
    for symbol in symbols {
        let response = port
            .symbol_explain(
                symbol_explain_request(invocation, vec![symbol]),
                options,
                cancellation.clone(),
            )
            .await
            .map_err(|error| map_context_source_client_error(error, usage))?;
        progress.record(context_evidence_usage(&response.result.context));
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.result.context));
        validate_context_evidence_identity(invocation, &response.result.context)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.result.execution_completeness.clone())
                .map_err(|error| context_evidence_error(error.kind, usage))?,
        )
        .map_err(|error| context_evidence_error(error.kind, usage))?;
        explanations.extend(response.result.symbols);
    }
    Ok(ContextSymbolExplanations {
        symbols: explanations,
        completeness,
        usage,
    })
}

async fn explain_context_symbols_within_budget<P>(
    port: Arc<P>,
    invocation: &EvidenceProviderInvocation,
    symbols: Vec<SymbolId>,
    prior_usage: BudgetCharge,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<ContextSymbolExplanations, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let mut explanations = Vec::with_capacity(symbols.len());
    let mut completeness = ResultCompleteness::complete();
    let mut usage = BudgetCharge::default();
    for (index, symbol) in symbols.iter().copied().enumerate() {
        let calls_remaining =
            u64::try_from(symbols.len().saturating_sub(index)).unwrap_or(u64::MAX);
        let consumed = add_context_evidence_usage(prior_usage, usage);
        let share = match context_evidence_budget_share(
            invocation.reservation(),
            consumed,
            calls_remaining,
        ) {
            Ok(share) => share,
            Err(resource) => {
                completeness = merge_context_evidence_completeness(
                    completeness,
                    context_evidence_budget_truncation(resource)?,
                )?;
                break;
            }
        };
        let response = port
            .symbol_explain(
                symbol_explain_request(invocation, vec![symbol]),
                context_evidence_options(share)?,
                cancellation.clone(),
            )
            .await
            .map_err(map_context_evidence_client_error)?;
        progress.record(context_evidence_usage(&response.result.context));
        validate_context_evidence_identity(invocation, &response.result.context)?;
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.result.context));
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.result.execution_completeness.clone())?,
        )?;
        explanations.extend(response.result.symbols);
    }
    Ok(ContextSymbolExplanations {
        symbols: explanations,
        completeness,
        usage,
    })
}

async fn retrieve_located_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved =
        resolve_context_evidence_anchors(port, &invocation, options, cancellation, progress)
            .await?;
    let mut observations = Vec::new();
    for hit in resolved.hits {
        let (symbol_id, identity) = match hit.symbol {
            Some(symbol) => (Some(symbol), symbol.to_string()),
            None if hit.kind == "file" => {
                let Some(source) = hit.source.as_ref() else {
                    continue;
                };
                if source.file() != hit.file
                    || source.repository() != invocation.repository()
                    || source.generation() != invocation.generation()
                {
                    return Err(context_evidence_error(
                        ContextEvidencePortErrorKind::InvalidResponse,
                        resolved.usage,
                    ));
                }
                (None, hit.file.to_string())
            }
            None => continue,
        };
        let source_refs = match hit.source.as_ref() {
            Some(source) => context_source_refs(std::slice::from_ref(source))?,
            None => Vec::new(),
        };
        let tokens = hit
            .identifier
            .len()
            .saturating_add(hit.qualified_name.len())
            .saturating_add(hit.path.len());
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id,
            identity,
            observed_score: Some(u16::try_from(hit.score.min(1_000)).unwrap_or(1_000)),
            observed_relevance: None,
            estimated_tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs,
        });
    }
    make_context_evidence_output(
        &invocation,
        observations,
        resolved.completeness,
        resolved.usage,
    )
}

async fn retrieve_definition_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let explained = explain_context_symbols(
        port,
        &invocation,
        resolved.symbols,
        options,
        cancellation,
        progress,
    )
    .await?;
    let usage = add_context_evidence_usage(resolved.usage, explained.usage);
    let completeness =
        merge_context_evidence_completeness(resolved.completeness, explained.completeness)?;
    let mut observations = Vec::new();
    for explanation in explained.symbols {
        let source_refs = context_source_refs(std::slice::from_ref(&explanation.definition))?;
        let tokens = explanation
            .display_name
            .len()
            .saturating_add(explanation.signature.as_ref().map_or(0, String::len));
        let confidence = u16::try_from(explanation.confidence.min(1_000)).unwrap_or(1_000);
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: Some(explanation.symbol),
            identity: explanation.symbol.to_string(),
            observed_score: Some(confidence),
            observed_relevance: None,
            estimated_tokens: u64::try_from(tokens.min(CONTEXT_DEFINITION_CANDIDATE_TOKENS))
                .unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs,
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

#[cfg(test)]
pub(super) async fn retrieve_source_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    reservation: BudgetCharge,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    retrieve_source_evidence_with_progress(
        port,
        invocation,
        reservation,
        options,
        cancellation,
        &ContextWorkProgress::default(),
    )
    .await
}

async fn retrieve_source_evidence_with_progress<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    reservation: BudgetCharge,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let explained = explain_source_context_symbols(
        Arc::clone(&port),
        &invocation,
        resolved.symbols,
        reservation,
        resolved.usage,
        cancellation.clone(),
        progress,
    )
    .await
    .map_err(|error| {
        context_evidence_error(
            error.kind,
            add_context_evidence_usage(resolved.usage, error.usage),
        )
    })?;
    let mut usage = add_context_evidence_usage(resolved.usage, explained.usage);
    let mut completeness =
        merge_context_evidence_completeness(resolved.completeness, explained.completeness)
            .map_err(|error| context_evidence_error(error.kind, usage))?;
    let references = explained
        .symbols
        .iter()
        .map(|value| value.definition.clone())
        .collect::<Vec<_>>();
    if references.is_empty() {
        if cancellation.is_cancelled() {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::Cancelled,
                usage,
            ));
        }
        return make_context_evidence_output(&invocation, Vec::new(), completeness, usage);
    }
    if cancellation.is_cancelled() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            usage,
        ));
    }
    let source_share = match source_evidence_budget_share(reservation, usage, 1) {
        Ok(share) => share,
        Err(resource) => {
            completeness = merge_context_evidence_completeness(
                completeness,
                context_evidence_budget_truncation(resource)
                    .map_err(|error| context_evidence_error(error.kind, usage))?,
            )
            .map_err(|error| context_evidence_error(error.kind, usage))?;
            return make_context_evidence_output(&invocation, Vec::new(), completeness, usage);
        }
    };
    let source_options = context_evidence_options(source_share)
        .map_err(|error| context_evidence_error(error.kind, usage))?;
    let source = port
        .source_read_lossless_text(
            SourceReadPortRequest {
                repository: invocation.repository(),
                generation: client::GenerationSelector::Generation(invocation.generation()),
                selector_count: references.len(),
                reference_selector_indexes: Vec::new(),
                symbol_selectors: Vec::new(),
                references,
                context_lines_before: 0,
                context_lines_after: 0,
                merge_overlaps: false,
                include_line_numbers: true,
                encoding: SourceEncodingRequest::Utf8LosslessWhenValid,
            },
            source_options,
            cancellation.clone(),
        )
        .await
        .map_err(|error| map_context_source_client_error(error, usage))?;
    progress.record(context_evidence_usage(&source.result.context));
    usage = add_context_evidence_usage(usage, context_evidence_usage(&source.result.context));
    validate_context_evidence_identity(&invocation, &source.result.context)
        .map_err(|error| context_evidence_error(error.kind, usage))?;
    completeness = merge_context_evidence_completeness(
        completeness,
        context_evidence_completeness(source.result.execution_completeness.clone())
            .map_err(|error| context_evidence_error(error.kind, usage))?,
    )
    .map_err(|error| context_evidence_error(error.kind, usage))?;
    let invalid_read =
        || context_evidence_error(ContextEvidencePortErrorKind::InvalidResponse, usage);
    if !context_source_cardinality_matches(
        explained.symbols.len(),
        source.result.chunks.len(),
        source.result.truncated,
        source.result.execution_completeness.state,
    ) || u64::try_from(source.result.chunks.len()).ok()
        != Some(source.result.context.usage.results)
    {
        return Err(invalid_read());
    }
    let mut observations = Vec::new();
    let mut physical_bytes = 0_u64;
    let mut text_bytes = 0_u64;
    for (index, chunk) in source.result.chunks.into_iter().enumerate() {
        let Some(explanation) = explained.symbols.get(index) else {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::InvalidResponse,
                usage,
            ));
        };
        let expected_source = &explanation.definition;
        if &chunk.source != expected_source
            || chunk.start_byte != expected_source.byte_range().start
            || chunk.end_byte != expected_source.byte_range().end
            || chunk.content_hash != expected_source.content_hash()
        {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::InvalidResponse,
                usage,
            ));
        }
        let chunk_source_bytes = context_text_physical_bytes(
            &chunk.content,
            chunk.physical_encoding,
            chunk.start_byte,
            chunk.end_byte,
            &cancellation,
        )
        .map_err(|error| context_evidence_error(error.kind, usage))?;
        physical_bytes = physical_bytes
            .checked_add(chunk_source_bytes)
            .ok_or_else(invalid_read)?;
        text_bytes = text_bytes
            .checked_add(u64::try_from(chunk.content.len()).map_err(|_| invalid_read())?)
            .ok_or_else(invalid_read)?;
        let source_refs = context_source_refs(std::slice::from_ref(&chunk.source))
            .map_err(|error| context_evidence_error(error.kind, usage))?;
        let symbol = explanation.symbol;
        let confidence = u16::try_from(explanation.confidence.min(1_000)).unwrap_or(1_000);
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: Some(symbol),
            identity: format!("{}:{}:{}", symbol, chunk.start_byte, chunk.end_byte),
            observed_score: Some(confidence),
            observed_relevance: None,
            // The pack reservation uses token estimates, while exact source
            // bytes remain a separately enforced resource in the parent ledger.
            estimated_tokens: rootlight_mcp_contract::accounting::estimate_tokens(
                chunk.content.len(),
            ),
            source_bytes: chunk_source_bytes,
            source_refs,
        });
    }
    if physical_bytes != source.result.total_source_bytes
        || text_bytes != source.result.total_text_bytes
        || source.result.context.usage.source_bytes != physical_bytes
    {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::InvalidResponse,
            usage,
        ));
    }
    if cancellation.is_cancelled() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            usage,
        ));
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

fn map_context_source_client_error(
    error: ClientPortError,
    prior_usage: BudgetCharge,
) -> ContextEvidencePortError {
    let mut error = map_context_evidence_client_error(error);
    error.usage = add_context_evidence_usage(prior_usage, error.usage);
    error
}

fn context_source_cardinality_matches(
    expected: usize,
    actual: usize,
    truncated: bool,
    state: client::ResultCompletenessState,
) -> bool {
    actual <= expected
        && (truncated || actual == expected)
        && truncated == (state == client::ResultCompletenessState::Truncated)
}

fn context_text_physical_bytes(
    content: &str,
    encoding: client::SourceTextEncoding,
    start_byte: u64,
    end_byte: u64,
    cancellation: &RequestCancellation,
) -> Result<u64, ContextEvidencePortError> {
    let cancelled = || {
        context_evidence_error(
            ContextEvidencePortErrorKind::Cancelled,
            BudgetCharge::default(),
        )
    };
    if cancellation.is_cancelled() {
        return Err(cancelled());
    }
    let physical_bytes = match encoding {
        client::SourceTextEncoding::Utf8 => u64::try_from(content.len()).ok(),
        client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => {
            if !start_byte.is_multiple_of(2)
                || !end_byte.is_multiple_of(2)
                || (start_byte == 0 && !content.is_empty() && !content.starts_with('\u{feff}'))
            {
                return Err(invalid_context_evidence_response());
            }
            let mut bytes = 0_u64;
            for (index, character) in content.chars().enumerate() {
                if index.is_multiple_of(1_024) && cancellation.is_cancelled() {
                    return Err(cancelled());
                }
                if character == '\0' {
                    return Err(invalid_context_evidence_response());
                }
                let width = u64::try_from(character.len_utf16())
                    .ok()
                    .and_then(|width| width.checked_mul(2))
                    .ok_or_else(invalid_context_evidence_response)?;
                bytes = bytes
                    .checked_add(width)
                    .ok_or_else(invalid_context_evidence_response)?;
            }
            Some(bytes)
        }
    }
    .ok_or_else(invalid_context_evidence_response)?;
    if cancellation.is_cancelled() {
        return Err(cancelled());
    }
    if end_byte.checked_sub(start_byte) != Some(physical_bytes) {
        return Err(invalid_context_evidence_response());
    }
    Ok(physical_bytes)
}

async fn retrieve_relationship_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let response = port
        .symbol_relationships(
            SymbolRelationshipsPortRequest {
                repository: invocation.repository(),
                generation: client::GenerationSelector::Generation(invocation.generation()),
                seeds: resolved.symbols,
                relations: vec!["calls".to_owned(), "references".to_owned()],
                direction: Some("both".to_owned()),
                min_confidence: None,
                max_results: Some(invocation.max_candidates()),
                page_offset: 0,
            },
            options,
            cancellation.clone(),
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    let mut usage = add_context_evidence_usage(
        resolved.usage,
        context_evidence_usage(&response.result.context),
    );
    let mut completeness = merge_context_evidence_completeness(
        resolved.completeness,
        context_evidence_completeness(response.result.execution_completeness.clone())?,
    )?;
    let mut related_symbols = response
        .result
        .groups
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.symbol))
        .collect::<Vec<_>>();
    related_symbols.sort_unstable();
    related_symbols.dedup();
    let mut task_relevance = std::collections::BTreeMap::new();
    let mut definitions = std::collections::BTreeMap::new();
    if !related_symbols.is_empty() {
        let explained = explain_context_symbols_within_budget(
            Arc::clone(&port),
            &invocation,
            related_symbols,
            usage,
            cancellation,
            progress,
        )
        .await?;
        usage = add_context_evidence_usage(usage, explained.usage);
        completeness = merge_context_evidence_completeness(completeness, explained.completeness)?;
        for explanation in explained.symbols {
            if task_mentions_identifier(invocation.task(), &explanation.display_name) {
                task_relevance.insert(explanation.symbol, 1_000);
            }
            definitions.insert(explanation.symbol, explanation.definition);
        }
    }
    let mut observations = Vec::new();
    for group in response.result.groups {
        for item in group.items {
            // The candidate represents the related symbol. Edge references may
            // cover only the callee's name, so prefer its explained definition.
            let source_refs = context_source_refs(
                definitions
                    .get(&item.symbol)
                    .map_or(item.source_refs.as_slice(), std::slice::from_ref),
            )?;
            let relevance = task_relevance
                .get(&item.symbol)
                .copied()
                .unwrap_or(item.confidence);
            observations.push(EvidenceProviderObservation {
                kind: EvidenceProviderObservationKind::Primary,
                symbol_id: Some(item.symbol),
                identity: item.symbol.to_string(),
                observed_score: Some(item.confidence),
                observed_relevance: Some(relevance),
                estimated_tokens: u64::try_from(
                    group.relation.len().saturating_add(group.direction.len()),
                )
                .unwrap_or(u64::MAX),
                source_bytes: 0,
                source_refs,
            });
        }
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

fn task_mentions_identifier(task: &str, identifier: &str) -> bool {
    let normalized_identifier = identifier
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    !normalized_identifier.is_empty()
        && task
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .any(|token| token == normalized_identifier)
}

fn test_evidence_confidence(score: u16, why: &[String]) -> Option<u16> {
    if why.iter().any(|reason| reason == "direct_test_edge") {
        return (700..=1_000)
            .contains(&score)
            .then(|| u16::try_from(u32::from(score - 700) * 1_000 / 300).unwrap_or(1_000));
    }
    if why.iter().any(|reason| reason == "transitive_dependency") {
        return (400..=600)
            .contains(&score)
            .then(|| u16::try_from(u32::from(score - 400) * 1_000 / 200).unwrap_or(1_000));
    }
    why.iter()
        .any(|reason| reason == "shared_file_with_seed")
        .then_some(1_000)
}

#[cfg(test)]
mod provider_score_tests {
    use super::{task_mentions_identifier, test_evidence_confidence};

    #[test]
    fn task_relevance_matches_whole_identifiers_only() {
        assert!(task_mentions_identifier(
            "fix budget_entry without breaking budget_helper",
            "budget_helper"
        ));
        assert!(!task_mentions_identifier(
            "fix budget_entry without breaking budget_helpers",
            "budget_helper"
        ));
        assert!(task_mentions_identifier(
            "fix budgetentry without breaking budgethelper",
            "BudgetHelper"
        ));
    }

    #[test]
    fn test_relevance_bands_recover_evidence_confidence() {
        assert_eq!(
            test_evidence_confidence(970, &["direct_test_edge".to_owned()]),
            Some(900)
        );
        assert_eq!(
            test_evidence_confidence(580, &["transitive_dependency".to_owned()]),
            Some(900)
        );
        assert_eq!(
            test_evidence_confidence(150, &["shared_file_with_seed".to_owned()]),
            Some(1_000)
        );
        assert_eq!(
            test_evidence_confidence(580, &["unknown_signal".to_owned()]),
            None
        );
    }
}

async fn retrieve_test_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let provider_options = remaining_context_evidence_options(&invocation, resolved.usage)?;
    let response = port
        .tests_select(
            TestsSelectPortRequest {
                repository: invocation.repository(),
                generation: client::GenerationSelector::Generation(invocation.generation()),
                seeds: resolved.symbols,
                seed_paths: Vec::new(),
                seed_build_targets: Vec::new(),
                frameworks: Vec::new(),
                max_total_ms: None,
                max_slow_tests: None,
                change_working_tree: None,
                change_revision_range: None,
                test_kinds: Vec::new(),
                max_tests: Some(invocation.max_candidates()),
                include_commands: Some(false),
            },
            provider_options,
            cancellation,
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    let usage = add_context_evidence_usage(
        resolved.usage,
        context_evidence_usage(&response.result.context),
    );
    let completeness = merge_context_evidence_completeness(
        resolved.completeness,
        context_evidence_completeness(response.result.execution_completeness.clone())?,
    )?;
    let mut observations = Vec::new();
    for test in response.result.tests {
        let confidence = test_evidence_confidence(test.score, &test.why)
            .ok_or_else(invalid_context_evidence_response)?;
        let tokens = test
            .test_id
            .len()
            .saturating_add(test.path.as_ref().map_or(0, String::len))
            .saturating_add(test.why.iter().map(String::len).sum::<usize>());
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: test.test_id.parse().ok(),
            identity: test.test_id,
            observed_score: Some(confidence),
            observed_relevance: Some(test.score),
            estimated_tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs: Vec::new(),
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

async fn retrieve_architecture_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let response = port
        .architecture_overview(
            ArchitectureOverviewPortRequest {
                repository: invocation.repository(),
                generation: client::GenerationSelector::Generation(invocation.generation()),
                views: vec!["hotspots".to_owned()],
                max_components: Some(invocation.max_candidates()),
                include_edges: Some(true),
                min_confidence: None,
                contract: client::ArchitectureOverviewOptions::default(),
            },
            options,
            cancellation,
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    let usage = context_evidence_usage(&response.result.context);
    let completeness =
        context_evidence_completeness(response.result.execution_completeness.clone())?;
    let mut observations = Vec::new();
    for component in response.result.components {
        let tokens = component
            .id
            .len()
            .saturating_add(component.kind.len())
            .saturating_add(component.name.len())
            .saturating_add(
                component
                    .responsibility_evidence
                    .iter()
                    .map(String::len)
                    .sum::<usize>(),
            );
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: None,
            identity: component.id,
            observed_score: Some(component.confidence),
            observed_relevance: None,
            estimated_tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs: Vec::new(),
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

fn change_impact_request(
    invocation: &EvidenceProviderInvocation,
    resolved_symbols: Vec<SymbolId>,
) -> Result<ChangeImpactPortRequest, ContextEvidencePortError> {
    let mut changed_paths = Vec::new();
    for anchor in invocation.anchors() {
        match anchor {
            EvidenceAnchor::Path(path) => changed_paths.push(path.clone()),
            EvidenceAnchor::Symbol(_)
            | EvidenceAnchor::Route(_)
            | EvidenceAnchor::Test(_)
            | EvidenceAnchor::Change(_)
            | EvidenceAnchor::Plan(_)
            | EvidenceAnchor::Located(_) => {}
        }
    }
    changed_paths.sort();
    changed_paths.dedup();
    if resolved_symbols.is_empty() && changed_paths.is_empty() {
        return Err(unsupported_context_evidence());
    }
    Ok(ChangeImpactPortRequest {
        repository: invocation.repository(),
        generation: client::GenerationSelector::Generation(invocation.generation()),
        changed_symbols: resolved_symbols,
        changed_paths,
        working_tree: None,
        revision_range: None,
        scope_paths: Vec::new(),
        scope_packages: Vec::new(),
        scope_services: Vec::new(),
        relation_policy: "standard".to_owned(),
        include_history: false,
        max_depth: Some(4),
        min_confidence: None,
        include_tests: Some(true),
        max_dependents: Some(invocation.max_candidates()),
    })
}

async fn retrieve_change_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    let request = change_impact_request(&invocation, resolved.symbols)?;
    let provider_options = remaining_context_evidence_options(&invocation, resolved.usage)?;
    let response = port
        .change_impact(request, provider_options, cancellation)
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    let usage = add_context_evidence_usage(
        resolved.usage,
        context_evidence_usage(&response.result.context),
    );
    let completeness = merge_context_evidence_completeness(
        resolved.completeness,
        context_evidence_completeness(response.result.execution_completeness.clone())?,
    )?;
    let mut observations = Vec::new();
    let observed_confidence = response
        .result
        .impacted
        .iter()
        .flat_map(|group| group.dependents.iter())
        .map(|entry| entry.confidence)
        .max();
    if observed_confidence.is_some() || !response.result.resolved_changes.is_empty() {
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::ChangeRiskSummary,
            symbol_id: response
                .result
                .impacted
                .iter()
                .flat_map(|group| group.dependents.iter())
                .next()
                .map(|entry| entry.symbol_id),
            identity: format!("risk:{}", invocation.id().as_str()),
            observed_score: observed_confidence,
            observed_relevance: None,
            estimated_tokens: u64::try_from(response.result.risk_summary.reasons.len().max(1) * 16)
                .unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs: Vec::new(),
        });
    }
    for group in response.result.impacted {
        for dependent in group.dependents {
            observations.push(EvidenceProviderObservation {
                kind: EvidenceProviderObservationKind::Primary,
                symbol_id: Some(dependent.symbol_id),
                identity: dependent.symbol_id.to_string(),
                observed_score: Some(dependent.confidence),
                observed_relevance: None,
                estimated_tokens: u64::try_from(
                    dependent.via.iter().map(String::len).sum::<usize>(),
                )
                .unwrap_or(u64::MAX),
                source_bytes: 0,
                source_refs: Vec::new(),
            });
        }
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

async fn retrieve_history_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let base = invocation.anchors().iter().find_map(|anchor| match anchor {
        EvidenceAnchor::Change(value)
        | EvidenceAnchor::Plan(value)
        | EvidenceAnchor::Located(value) => value.parse::<GenerationId>().ok(),
        EvidenceAnchor::Symbol(_)
        | EvidenceAnchor::Path(_)
        | EvidenceAnchor::Route(_)
        | EvidenceAnchor::Test(_) => None,
    });
    let Some(base) = base.filter(|value| *value != invocation.generation()) else {
        return Err(unsupported_context_evidence());
    };
    let response = port
        .history_compare(
            HistoryComparePortRequest {
                repository: invocation.repository(),
                base: client::HistoryRevisionSelector::Generation(base),
                head: client::HistoryRevisionSelector::Generation(invocation.generation()),
                scope: client::HistoryCompareScope::default(),
                change_kinds: Vec::new(),
                include_unchanged_context: false,
                max_results: Some(invocation.max_candidates()),
            },
            options,
            cancellation,
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    if response.result.matched_states.base_generation != base
        || response.result.matched_states.head_generation != invocation.generation()
    {
        return Err(invalid_context_evidence_response());
    }
    let usage = context_evidence_usage(&response.result.context);
    let completeness =
        context_evidence_completeness(response.result.execution_completeness.clone())?;
    let mut observations = Vec::new();
    for change in response.result.changes {
        let tokens = change.kind.len().saturating_add(change.entity_kind.len());
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: Some(change.symbol_id),
            identity: change.symbol_id.to_string(),
            observed_score: Some(change.significance),
            observed_relevance: None,
            estimated_tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs: Vec::new(),
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

fn plan_objective_for_context(
    objective: rootlight_agent::context_pack_request::ContextPackObjective,
) -> PlanObjective {
    match objective {
        rootlight_agent::context_pack_request::ContextPackObjective::BugFix => {
            PlanObjective::BugFix
        }
        rootlight_agent::context_pack_request::ContextPackObjective::Refactor => {
            PlanObjective::Refactor
        }
        rootlight_agent::context_pack_request::ContextPackObjective::Explanation => {
            PlanObjective::Explanation
        }
        rootlight_agent::context_pack_request::ContextPackObjective::Migration => {
            PlanObjective::Migration
        }
        rootlight_agent::context_pack_request::ContextPackObjective::Review => {
            PlanObjective::Review
        }
    }
}

async fn retrieve_planning_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
    progress: &ContextWorkProgress,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
        progress,
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let provider_options = remaining_context_evidence_options(&invocation, resolved.usage)?;
    let targets = resolved
        .symbols
        .into_iter()
        .map(|symbol_id| PlanTargetSelector::Symbol(PlanSymbolTarget { symbol_id }))
        .collect();
    let request = normalize_plan_change(PlanChangeInput {
        repository: RepositorySelector::ById(
            rootlight_mcp_contract::vertical::RepositoryIdSelector {
                repository_id: invocation.repository(),
            },
        ),
        generation: Some(GenerationSelector::Explicit(invocation.generation())),
        objective: plan_objective_for_context(invocation.objective()),
        objective_text: invocation.task().to_owned(),
        targets,
        constraints: None,
        change_context: None,
        max_steps: Some(u8::try_from(invocation.max_candidates().min(100)).unwrap_or(100)),
        budget: None,
        profile: Some(ResponseProfile::Compact),
        explain: Some(false),
    })
    .map_err(|_| unsupported_context_evidence())?;
    let response = port
        .plan_change(request, provider_options, cancellation)
        .await
        .map_err(map_context_evidence_client_error)?;
    progress.record(context_evidence_usage(&response.result.context));
    validate_context_evidence_identity(&invocation, &response.result.context)?;
    let usage = add_context_evidence_usage(
        resolved.usage,
        context_evidence_usage(&response.result.context),
    );
    let completeness = merge_context_evidence_completeness(
        resolved.completeness,
        context_evidence_completeness(response.result.execution_completeness.clone())?,
    )?;
    let mut observations = Vec::new();
    for step in response.result.plan {
        let symbol_id = step.targets.first().copied();
        let tokens = step
            .action
            .len()
            .saturating_add(step.risks.iter().map(String::len).sum::<usize>())
            .saturating_add(step.verification.as_ref().map_or(0, String::len));
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id,
            identity: format!("plan-step:{}:{}", step.step, invocation.id().as_str()),
            observed_score: None,
            observed_relevance: None,
            estimated_tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
            source_bytes: 0,
            source_refs: Vec::new(),
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

#[cfg(test)]
mod context_source_tests {
    use super::*;

    #[test]
    fn context_source_evidence_error_projects_only_candidate_dimensions() {
        use rootlight_agent::policy::{BudgetLedger, BudgetResource, ExecutionPolicyError};

        let measured = BudgetCharge {
            rows: 11,
            results: 4,
            tokens: 2_914,
            actual_tokens: 17,
            source_bytes: 23,
            traversal_facts: 3,
            depth: 2,
            paths: 1,
            json_bytes: 2_914,
            memory_bytes: 2_050,
            time_ms: 7,
        };
        let expected = BudgetCharge {
            results: 0,
            tokens: 0,
            ..measured
        };
        for kind in [
            ContextEvidencePortErrorKind::Unavailable,
            ContextEvidencePortErrorKind::Unsupported,
            ContextEvidencePortErrorKind::InvalidResponse,
            ContextEvidencePortErrorKind::Cancelled,
            ContextEvidencePortErrorKind::DeadlineExceeded,
        ] {
            let projected = project_context_evidence_error(context_evidence_error(kind, measured));
            assert_eq!(projected.kind, kind);
            assert_eq!(projected.usage, expected);
        }
        let mut ledger = BudgetLedger::from_limits(BudgetLimits::from_maximums(expected));
        ledger.charge(expected).expect("exact measured work fits");
        assert_eq!(ledger.consumed(), expected);
        for (resource, limits) in [
            (
                BudgetResource::Rows,
                BudgetCharge {
                    rows: 10,
                    ..expected
                },
            ),
            (
                BudgetResource::ActualTokens,
                BudgetCharge {
                    actual_tokens: 16,
                    ..expected
                },
            ),
            (
                BudgetResource::SourceBytes,
                BudgetCharge {
                    source_bytes: 22,
                    ..expected
                },
            ),
            (
                BudgetResource::TraversalFacts,
                BudgetCharge {
                    traversal_facts: 2,
                    ..expected
                },
            ),
            (
                BudgetResource::Depth,
                BudgetCharge {
                    depth: 1,
                    ..expected
                },
            ),
            (
                BudgetResource::Paths,
                BudgetCharge {
                    paths: 0,
                    ..expected
                },
            ),
            (
                BudgetResource::JsonBytes,
                BudgetCharge {
                    json_bytes: 2_913,
                    ..expected
                },
            ),
            (
                BudgetResource::MemoryBytes,
                BudgetCharge {
                    memory_bytes: 2_049,
                    ..expected
                },
            ),
            (
                BudgetResource::Time,
                BudgetCharge {
                    time_ms: 6,
                    ..expected
                },
            ),
        ] {
            let mut ledger = BudgetLedger::from_limits(BudgetLimits::from_maximums(limits));
            assert_eq!(
                ledger.charge(expected),
                Err(ExecutionPolicyError::BudgetExceeded { resource }),
                "candidate projection preserves the {resource:?} hard bound"
            );
            assert_eq!(ledger.consumed(), BudgetCharge::default());
        }
    }

    #[test]
    fn context_source_evidence_rejects_false_complete_subsets() {
        use client::ResultCompletenessState::{Complete, Truncated, UnsupportedPartial};

        assert!(context_source_cardinality_matches(2, 2, false, Complete));
        assert!(context_source_cardinality_matches(2, 1, true, Truncated));
        assert!(context_source_cardinality_matches(2, 0, true, Truncated));
        assert!(context_source_cardinality_matches(
            2,
            2,
            false,
            UnsupportedPartial
        ));
        for (expected, actual, truncated, state) in [
            (2, 0, false, Complete),
            (2, 1, false, Complete),
            (2, 3, true, Truncated),
            (2, 1, true, Complete),
            (2, 2, false, Truncated),
        ] {
            assert!(!context_source_cardinality_matches(
                expected, actual, truncated, state
            ));
        }
    }

    #[test]
    fn context_source_evidence_failure_retains_already_measured_work() {
        let prior = BudgetCharge {
            rows: 7,
            results: 2,
            source_bytes: 23,
            tokens: 37,
            json_bytes: 41,
            memory_bytes: 43,
            ..BudgetCharge::default()
        };
        let unsupported = PublicError::builder(
            ErrorCode::UnsupportedCapability,
            "source encoding is unsupported",
        )
        .build()
        .expect("static unsupported error satisfies the public contract");
        for (error, expected) in [
            (
                ClientPortError::InvalidResponse,
                ContextEvidencePortErrorKind::InvalidResponse,
            ),
            (
                ClientPortError::Public(Box::new(unsupported)),
                ContextEvidencePortErrorKind::Unsupported,
            ),
        ] {
            let mapped = map_context_source_client_error(error, prior);
            assert_eq!(mapped.kind, expected);
            assert_eq!(mapped.usage, prior);
        }
    }

    #[test]
    fn context_source_evidence_counts_physical_bytes_separately_from_text() {
        let (_sender, receiver) = tokio::sync::watch::channel(false);
        let cancellation = RequestCancellation { receiver };
        let content = "\u{feff}\u{2603}\u{1f642}\r\n";
        assert_eq!(content.len(), 12);
        for encoding in [
            client::SourceTextEncoding::Utf16Le,
            client::SourceTextEncoding::Utf16Be,
        ] {
            assert_eq!(
                context_text_physical_bytes(content, encoding, 0, 12, &cancellation)
                    .expect("BOM and scalar widths retain original coordinates"),
                12
            );
            assert_eq!(
                context_text_physical_bytes("\u{2603}", encoding, 2, 4, &cancellation)
                    .expect("BMP presentation expands without expanding physical source"),
                2
            );
            assert_eq!(
                context_text_physical_bytes("\u{1f642}", encoding, 4, 8, &cancellation)
                    .expect("non-BMP scalar consumes one surrogate pair"),
                4
            );
        }
        assert_eq!(
            context_text_physical_bytes(
                content,
                client::SourceTextEncoding::Utf8,
                0,
                12,
                &cancellation
            )
            .expect("UTF-8 offsets remain identical to text widths"),
            12
        );
        assert_eq!(
            context_text_physical_bytes(
                "\0",
                client::SourceTextEncoding::Utf8,
                7,
                8,
                &cancellation
            )
            .expect("the established UTF-8 NUL policy is retained"),
            1
        );
    }

    #[test]
    fn context_source_evidence_rejects_contradictory_physical_coordinates() {
        let (_sender, receiver) = tokio::sync::watch::channel(false);
        let cancellation = RequestCancellation { receiver };
        for encoding in [
            client::SourceTextEncoding::Utf16Le,
            client::SourceTextEncoding::Utf16Be,
        ] {
            for (content, start, end) in [
                ("\u{2603}", 2, 5),
                ("\u{2603}", 1, 3),
                ("\u{1f642}", 2, 4),
                ("x", 0, 2),
                ("\0", 2, 4),
                ("x", u64::MAX - 1, 0),
            ] {
                assert_eq!(
                    context_text_physical_bytes(content, encoding, start, end, &cancellation)
                        .expect_err("contradictory source widths fail closed")
                        .kind,
                    ContextEvidencePortErrorKind::InvalidResponse
                );
            }
        }
        assert!(
            context_text_physical_bytes(
                "\u{2603}",
                client::SourceTextEncoding::Utf8,
                2,
                5,
                &cancellation
            )
            .is_ok()
        );
        assert!(
            context_text_physical_bytes(
                "\u{2603}",
                client::SourceTextEncoding::Utf8,
                2,
                4,
                &cancellation
            )
            .is_err()
        );
    }

    #[test]
    fn context_source_evidence_stops_on_request_cancellation() {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let cancellation = RequestCancellation { receiver };
        sender
            .send(true)
            .expect("request owner signals cancellation");
        for encoding in [
            client::SourceTextEncoding::Utf8,
            client::SourceTextEncoding::Utf16Le,
            client::SourceTextEncoding::Utf16Be,
        ] {
            assert_eq!(
                context_text_physical_bytes("", encoding, 0, 0, &cancellation)
                    .expect_err("cancellation precedes admission even for an empty chunk")
                    .kind,
                ContextEvidencePortErrorKind::Cancelled
            );
        }
    }

    #[test]
    fn context_source_signature_uses_approved_physical_selection() {
        let text =
            "\u{feff}// preceding context\r\npub fn beacon() { \"\u{1f642}\" }\r\n// after\r\n";
        let declaration_start = text.find("pub fn").unwrap();
        let declaration_end = text.find("\r\n// after").unwrap();
        for encoding in [
            client::SourceTextEncoding::Utf8,
            client::SourceTextEncoding::Utf16Le,
            client::SourceTextEncoding::Utf16Be,
        ] {
            let width = |value: &str| {
                u64::try_from(match encoding {
                    client::SourceTextEncoding::Utf8 => value.len(),
                    client::SourceTextEncoding::Utf16Le | client::SourceTextEncoding::Utf16Be => {
                        value.encode_utf16().count() * 2
                    }
                })
                .unwrap()
            };
            let repository = RepositoryId::from_bytes([1; 16]);
            let generation = GenerationId::from_bytes([2; 20]);
            let file = rootlight_ids::FileId::from_bytes([3; 20]);
            let hash = rootlight_ids::ContentHash::from_bytes([4; 32]);
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(
                    file,
                    width(&text[..declaration_start]),
                    width(&text[..declaration_end]),
                )
                .unwrap(),
                hash,
                None,
            );
            let mut chunk = client::SourceTextChunk {
                extracted_from: None,
                source: client::SourceReference::new(
                    repository,
                    generation,
                    file,
                    source.span().start_byte()..source.span().end_byte(),
                    hash,
                    None,
                )
                .unwrap(),
                path: "sample.rs".to_owned(),
                start_byte: 0,
                end_byte: width(text),
                start_line: None,
                end_line: None,
                content: text.to_owned(),
                physical_encoding: encoding,
                content_hash: hash,
                language: "rust".to_owned(),
                tier: client::AnalysisTier::TierA,
                generated: false,
            };
            let (_sender, receiver) = tokio::sync::watch::channel(false);
            let cancellation = RequestCancellation { receiver };
            let selected = context_selected_prefix(&chunk, &source, &cancellation)
                .unwrap()
                .unwrap();
            assert_eq!(selected, &text[declaration_start..declaration_end]);
            assert_eq!(
                bounded_context_signature(selected, 1024).as_deref(),
                Some("pub fn beacon() {")
            );
            chunk.content.truncate(declaration_start);
            chunk.end_byte = source.span().start_byte();
            assert!(
                context_selected_prefix(&chunk, &source, &cancellation)
                    .unwrap()
                    .is_none()
            );
        }
    }
}
