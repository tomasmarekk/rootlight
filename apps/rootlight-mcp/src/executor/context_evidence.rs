//! Production adapters for typed context-pack evidence providers.

use super::*;
use rootlight_agent::context_evidence::{
    ContextSourceMaterial, ContextSourceOutput, ContextSourceRequest, ContextSourceSnippet,
};

const CONTEXT_SIGNATURE_BYTES: u32 = 1_024;
const CONTEXT_DEFINITION_CANDIDATE_TOKENS: usize = 384;
const FOCUSED_SOURCE_LINES_BEFORE: u8 = 1;
const FOCUSED_SOURCE_LINES_AFTER: u8 = 1;

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
            let operation =
                retrieve_context_evidence(port, invocation, reservation, cancellation.clone());
            let mut cancellation_wait = cancellation.clone();
            tokio::select! {
                biased;
                _ = cancellation_wait.cancelled() => Err(context_evidence_error(
                    ContextEvidencePortErrorKind::Cancelled,
                    BudgetCharge::default(),
                )),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    Err(context_evidence_error(
                        ContextEvidencePortErrorKind::DeadlineExceeded,
                        BudgetCharge::default(),
                    ))
                }
                response = operation => response,
            }
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
            let operation =
                materialize_context_source(port, request, reservation, cancellation.clone());
            let mut cancellation_wait = cancellation.clone();
            tokio::select! {
                biased;
                _ = cancellation_wait.cancelled() => Err(context_evidence_error(
                    ContextEvidencePortErrorKind::Cancelled,
                    BudgetCharge::default(),
                )),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    Err(context_evidence_error(
                        ContextEvidencePortErrorKind::DeadlineExceeded,
                        BudgetCharge::default(),
                    ))
                }
                response = operation => response,
            }
        })
    }
}

async fn materialize_context_source<P>(
    port: Arc<P>,
    request: ContextSourceRequest,
    reservation: BudgetCharge,
    cancellation: RequestCancellation,
) -> Result<ContextSourceOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    if request.targets.is_empty() || request.max_bytes_per_snippet == 0 {
        return Err(invalid_context_evidence_response());
    }
    let options = context_evidence_options(reservation)?;
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
    let response = port
        .source_read(
            SourceReadPortRequest {
                repository: request.repository,
                generation: client::GenerationSelector::Generation(request.generation),
                selector_count: references.len(),
                reference_selector_indexes: Vec::new(),
                symbol_selectors: Vec::new(),
                references,
                context_lines_before,
                context_lines_after,
                merge_overlaps: false,
                include_line_numbers: false,
                encoding: SourceEncodingRequest::Utf8LosslessWhenValid,
            },
            options,
            cancellation,
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    if response.result.context.repository != request.repository
        || response.result.context.generation != request.generation
        || response.result.context.parent_generation == Some(request.generation)
    {
        return Err(invalid_context_evidence_response());
    }
    let usage = context_evidence_usage(&response.result.context);
    let completeness =
        context_evidence_completeness(response.result.execution_completeness.clone())?;
    if response.result.chunks.len() > reference_sources.len() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::InvalidResponse,
            usage,
        ));
    }
    let response_truncated = response.result.truncated;
    let mut read_materials = Vec::with_capacity(response.result.chunks.len());
    for (requested_source, chunk) in reference_sources.iter().zip(response.result.chunks) {
        let returned_source =
            client_source_ref(&chunk.source).map_err(|_| invalid_context_evidence_response())?;
        let returned_bytes = chunk
            .end_byte
            .checked_sub(chunk.start_byte)
            .ok_or_else(invalid_context_evidence_response)?;
        let content_bytes =
            u64::try_from(chunk.content.len()).map_err(|_| invalid_context_evidence_response())?;
        if &returned_source != requested_source
            || chunk.start_byte > requested_source.span().start_byte()
            || chunk.end_byte < requested_source.span().end_byte()
            || chunk.content_hash != requested_source.content_hash()
            || content_bytes != returned_bytes
        {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::InvalidResponse,
                usage,
            ));
        }
        let content = exact_context_utf8(chunk.content, chunk.encoding, usage)?;
        let signature = bounded_context_signature(
            &content,
            usize::try_from(request.max_bytes_per_snippet.min(CONTEXT_SIGNATURE_BYTES))
                .unwrap_or(usize::MAX),
        );
        let snippet = request.include_snippets.then(|| {
            let (content, reduced) = truncate_context_source(
                content,
                usize::try_from(request.max_bytes_per_snippet).unwrap_or(usize::MAX),
            );
            ContextSourceSnippet {
                content,
                language: chunk.language,
                truncated: reduced || response_truncated,
            }
        });
        // The public item retains the exact evidence span even though its
        // untrusted preview contains bounded surrounding source context.
        read_materials.push((requested_source.clone(), signature, snippet));
    }
    let mut materials = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        let Some((source_ref, signature, snippet)) = read_materials
            .iter()
            .find(|(source_ref, _, _)| source_ref == &target.source_ref)
        else {
            continue;
        };
        materials.push(ContextSourceMaterial {
            candidate_id: target.candidate_id.clone(),
            source_ref: source_ref.clone(),
            signature: signature.clone(),
            snippet: snippet.clone(),
        });
    }
    Ok(ContextSourceOutput {
        repository: request.repository,
        generation: request.generation,
        materials,
        completeness,
        usage,
    })
}

fn exact_context_utf8(
    content: Vec<u8>,
    encoding: client::SourceEncoding,
    usage: BudgetCharge,
) -> Result<String, ContextEvidencePortError> {
    if encoding != client::SourceEncoding::Utf8 {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::InvalidResponse,
            usage,
        ));
    }
    String::from_utf8(content)
        .map_err(|_| context_evidence_error(ContextEvidencePortErrorKind::InvalidResponse, usage))
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
            retrieve_located_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Definition => {
            retrieve_definition_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Implementation | EvidenceProvider::Source => {
            retrieve_source_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Relationships => {
            retrieve_relationship_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Tests => {
            retrieve_test_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Architecture => {
            retrieve_architecture_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::ChangeImpact => {
            retrieve_change_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::History => {
            retrieve_history_evidence(port, invocation, options, cancellation).await
        }
        EvidenceProvider::Planning => {
            retrieve_planning_evidence(port, invocation, options, cancellation).await
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
    // Definition resolution and source retrieval share the same request
    // options, so each call receives a non-overlapping share of scanned rows.
    let maximum_calls = u64::try_from(anchor_lookups)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    let rows = reservation
        .rows
        .checked_div(maximum_calls)
        .filter(|value| *value > 0)
        .ok_or_else(invalid_context_evidence_response)?;
    context_evidence_options(BudgetCharge {
        rows,
        ..reservation
    })
}

const fn context_evidence_error(
    kind: ContextEvidencePortErrorKind,
    usage: BudgetCharge,
) -> ContextEvidencePortError {
    ContextEvidencePortError { kind, usage }
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
        let request = CodeLocatePortRequest {
            repository: invocation.repository(),
            generation: client::GenerationSelector::Generation(invocation.generation()),
            query,
            mode,
            languages: Vec::new(),
            maximum_results,
            page_offset: 0,
        };
        let response = port
            .code_locate(request, options, cancellation.clone())
            .await
            .map_err(map_context_evidence_client_error)?;
        validate_context_evidence_identity(invocation, &response.result.context)?;
        usage = add_context_evidence_usage(usage, context_evidence_usage(&response.result.context));
        completeness = merge_context_evidence_completeness(
            completeness,
            context_evidence_completeness(response.result.execution_completeness.clone())?,
        )?;
        for hit in response.result.hits {
            symbols.push(hit.symbol);
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
            .map_err(map_context_evidence_client_error)?;
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

async fn explain_context_symbols_within_budget<P>(
    port: Arc<P>,
    invocation: &EvidenceProviderInvocation,
    symbols: Vec<SymbolId>,
    prior_usage: BudgetCharge,
    cancellation: RequestCancellation,
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
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved =
        resolve_context_evidence_anchors(port, &invocation, options, cancellation).await?;
    let mut observations = Vec::new();
    for hit in resolved.hits {
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
            symbol_id: Some(hit.symbol),
            identity: hit.symbol.to_string(),
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
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let explained =
        explain_context_symbols(port, &invocation, resolved.symbols, options, cancellation).await?;
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

async fn retrieve_source_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
    )
    .await?;
    if resolved.symbols.is_empty() {
        return Err(context_evidence_error(
            ContextEvidencePortErrorKind::Unsupported,
            resolved.usage,
        ));
    }
    let explained = explain_context_symbols(
        Arc::clone(&port),
        &invocation,
        resolved.symbols,
        options,
        cancellation.clone(),
    )
    .await?;
    let mut usage = add_context_evidence_usage(resolved.usage, explained.usage);
    let mut completeness =
        merge_context_evidence_completeness(resolved.completeness, explained.completeness)?;
    let symbols = explained
        .symbols
        .iter()
        .map(|value| (value.symbol, value.confidence))
        .collect::<Vec<_>>();
    let references = explained
        .symbols
        .iter()
        .map(|value| value.definition.clone())
        .collect::<Vec<_>>();
    if references.is_empty() {
        return make_context_evidence_output(&invocation, Vec::new(), completeness, usage);
    }
    let source = port
        .source_read(
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
            options,
            cancellation,
        )
        .await
        .map_err(map_context_evidence_client_error)?;
    validate_context_evidence_identity(&invocation, &source.result.context)?;
    usage = add_context_evidence_usage(usage, context_evidence_usage(&source.result.context));
    completeness = merge_context_evidence_completeness(
        completeness,
        context_evidence_completeness(source.result.execution_completeness.clone())?,
    )?;
    let mut observations = Vec::new();
    for (index, chunk) in source.result.chunks.into_iter().enumerate() {
        let Some((symbol, confidence)) = symbols.get(index).copied() else {
            return Err(context_evidence_error(
                ContextEvidencePortErrorKind::InvalidResponse,
                usage,
            ));
        };
        let source_refs = context_source_refs(std::slice::from_ref(&chunk.source))?;
        let confidence = u16::try_from(confidence.min(1_000)).unwrap_or(1_000);
        observations.push(EvidenceProviderObservation {
            kind: EvidenceProviderObservationKind::Primary,
            symbol_id: Some(symbol),
            identity: format!("{}:{}:{}", symbol, chunk.start_byte, chunk.end_byte),
            observed_score: Some(confidence),
            observed_relevance: None,
            estimated_tokens: u64::try_from(chunk.content.len()).unwrap_or(u64::MAX),
            source_bytes: u64::try_from(chunk.content.len()).unwrap_or(u64::MAX),
            source_refs,
        });
    }
    make_context_evidence_output(&invocation, observations, completeness, usage)
}

async fn retrieve_relationship_evidence<P>(
    port: Arc<P>,
    invocation: EvidenceProviderInvocation,
    options: client::RequestOptions,
    cancellation: RequestCancellation,
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
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
    if !related_symbols.is_empty() {
        let explained = explain_context_symbols_within_budget(
            Arc::clone(&port),
            &invocation,
            related_symbols,
            usage,
            cancellation,
        )
        .await?;
        usage = add_context_evidence_usage(usage, explained.usage);
        completeness = merge_context_evidence_completeness(completeness, explained.completeness)?;
        for explanation in explained.symbols {
            if task_mentions_identifier(invocation.task(), &explanation.display_name) {
                task_relevance.insert(explanation.symbol, 1_000);
            }
        }
    }
    let mut observations = Vec::new();
    for group in response.result.groups {
        for item in group.items {
            let source_refs = context_source_refs(&item.source_refs)?;
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
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
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
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
    )
    .await?;
    let request = change_impact_request(&invocation, resolved.symbols)?;
    let provider_options = remaining_context_evidence_options(&invocation, resolved.usage)?;
    let response = port
        .change_impact(request, provider_options, cancellation)
        .await
        .map_err(map_context_evidence_client_error)?;
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
) -> Result<EvidenceProviderOutput, ContextEvidencePortError>
where
    P: FirstSliceClientPort,
{
    let resolved = resolve_context_evidence_anchors(
        Arc::clone(&port),
        &invocation,
        options,
        cancellation.clone(),
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
    fn context_source_requires_exact_utf8_bytes() {
        assert_eq!(
            exact_context_utf8(
                b"fn parse() {}".to_vec(),
                client::SourceEncoding::Utf8,
                BudgetCharge::default(),
            )
            .expect("valid UTF-8 is retained"),
            "fn parse() {}"
        );
        for (bytes, encoding) in [
            (vec![0xff], client::SourceEncoding::Utf8),
            (b"fn parse() {}".to_vec(), client::SourceEncoding::Bytes),
        ] {
            let error = exact_context_utf8(bytes, encoding, BudgetCharge::default())
                .expect_err("non-UTF-8 representations fail closed");
            assert_eq!(error.kind, ContextEvidencePortErrorKind::InvalidResponse);
        }
    }
}
