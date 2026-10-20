//! Exact source-read accounting for a distinct source-only generation capability.
//! Graph and lexical services are never constructed; VBA activity remains on
//! the full guarded route until its separate source metadata is available.

use super::*;

/// Reads exact bytes through existing query/source budget and publication accounting.
///
/// # Errors
/// Preserves identity, source, cancellation and budget failures; VBA selectors
/// require the ordinary graph-backed route with its mandatory activity sidecar.
pub fn read_source_authority(
    authority: &rootlight_source::VerifiedSourceAuthority,
    source: &SourceService<'_>,
    references: Vec<SourceRef>,
    options: SourceReadOptions,
    source_budget: SourceBudget,
    budget: QueryBudget,
    cancellation: &Cancellation,
) -> Result<QueryResponse<SourceReadQueryResult>, QueryError> {
    if references.iter().any(|reference| {
        authority
            .find_file(reference.span().file())
            .is_some_and(|file| conditional_activity::applicable(&file.language))
    }) {
        return Err(QueryError::IndexDrift);
    }
    let metadata = authority.metadata();
    let plan = plan(
        metadata,
        references,
        options,
        source_budget,
        budget,
        mem::size_of::<SourceChunkResult>(),
    )?;
    execute(metadata, &plan, source, cancellation)
}

fn plan(
    metadata: rootlight_storage::GenerationMetadata,
    references: Vec<SourceRef>,
    options: SourceReadOptions,
    mut source_budget: SourceBudget,
    budget: QueryBudget,
    chunk_memory_bytes: usize,
) -> Result<SourceReadPlan, QueryError> {
    budget.validate()?;
    intersect_source_duration(&mut source_budget, budget);
    source_budget.validate()?;
    if references.is_empty() || references.len() > source_budget.max_selectors {
        return Err(QueryError::Source(SourceError::SelectorLimit));
    }
    if options.context_lines_before > source_budget.max_context_lines
        || options.context_lines_after > source_budget.max_context_lines
    {
        return Err(QueryError::Source(SourceError::ContextLimit));
    }
    for reference in &references {
        if reference.generation() != metadata.generation()
            || reference.repository() != metadata.repository()
        {
            return Err(QueryError::GenerationMismatch);
        }
    }
    let chunk_memory = checked_usize_to_u64(
        references
            .len()
            .checked_mul(chunk_memory_bytes)
            .ok_or(QueryError::MemoryUnavailable)?,
    )?;
    let activity_memory = 0_u64;
    let reserved_memory = chunk_memory
        .checked_add(activity_memory)
        .ok_or(QueryError::MemoryUnavailable)?;
    let reference_rows = checked_usize_to_u64(references.len())?;
    if reference_rows > budget.max_rows {
        return Err(QueryError::PlanRejected {
            resource: QueryResource::Rows,
        });
    }
    // Chunk metadata and mandatory fail-closed sidecars remain outside the
    // source service's materialization allowance, even if its bytes fill it.
    let response_memory = budget
        .max_memory_bytes
        .checked_sub(reserved_memory)
        .filter(|remaining| *remaining > 0)
        .ok_or(QueryError::PlanRejected {
            resource: QueryResource::MemoryBytes,
        })?
        .min(checked_usize_to_u64(
            source_budget.max_response_memory_bytes,
        )?);
    source_budget.max_response_memory_bytes =
        usize::try_from(response_memory).map_err(|_| QueryError::MemoryUnavailable)?;
    let memory_bytes = checked_add(
        response_memory,
        reserved_memory,
        QueryResource::MemoryBytes,
        u64::MAX,
    )?;
    let estimate = PlanEstimate {
        rows: if activity_memory == 0 {
            reference_rows
        } else {
            budget.max_rows
        },
        edges: 0,
        results: checked_usize_to_u64(references.len())?,
        source_bytes: checked_usize_to_u64(source_budget.max_source_bytes)?,
        memory_bytes: if activity_memory == 0 {
            memory_bytes
        } else {
            budget.max_memory_bytes
        },
        json_bytes: budget.max_json_bytes,
        estimated_tokens: budget.max_tokens,
        duration_micros: duration_micros(budget.max_duration),
    };
    ensure_estimate(estimate, budget)?;
    let mut operators = vec![
        QueryOperator::GenerationPin,
        QueryOperator::SourceResolve,
        QueryOperator::VfsSnapshotRead,
        QueryOperator::ContentHashVerify,
    ];
    if activity_memory != 0 {
        operators.push(QueryOperator::ConditionalActivityProjection);
    }
    operators.push(QueryOperator::OutputBudget);
    let explanation = PlanExplanation {
        generation: metadata.generation(),
        kind: PlanKind::SourceRead,
        operators,
        estimate,
    };
    Ok(SourceReadPlan {
        references,
        options,
        source_budget,
        budget,
        explanation,
    })
}

fn execute(
    metadata: rootlight_storage::GenerationMetadata,
    plan: &SourceReadPlan,
    source: &SourceService<'_>,
    cancellation: &Cancellation,
) -> Result<QueryResponse<SourceReadQueryResult>, QueryError> {
    if metadata.generation() != plan.explanation.generation {
        return Err(QueryError::GenerationMismatch);
    }
    let started = Instant::now();
    let control = QueryControl::for_source(cancellation, source, plan.budget, plan.source_budget);
    control.check()?;
    let result = source.read(
        &plan.references,
        plan.options,
        plan.source_budget,
        cancellation,
    )?;
    control.check()?;
    if result.generation != metadata.generation() {
        return Err(QueryError::GenerationMismatch);
    }
    let mut tracker = UsageTracker::new(plan.budget);
    tracker.add_rows(checked_usize_to_u64(plan.references.len())?)?;
    tracker.add_source_bytes(checked_usize_to_u64(result.total_source_bytes)?)?;
    tracker.add_memory(checked_usize_to_u64(result.total_response_memory_bytes)?)?;
    tracker.add_memory(checked_usize_to_u64(
        result
            .chunks
            .len()
            .checked_mul(mem::size_of::<SourceChunkResult>())
            .ok_or(QueryError::MemoryUnavailable)?,
    )?)?;
    let mut chunks = Vec::new();
    try_reserve(&mut chunks, result.chunks.len())?;
    for chunk in result.chunks {
        control.check()?;
        tracker.add_results(1)?;
        let encoding = match chunk.encoding {
            ServiceSourceEncoding::Utf8 => SourceChunkEncoding::Utf8,
            ServiceSourceEncoding::Bytes => SourceChunkEncoding::Bytes,
            _ => return Err(QueryError::InvalidSourceEncoding),
        };
        chunks.push(SourceChunkResult {
            extracted_from: chunk.extracted_from,
            reference: chunk.reference,
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            bytes: chunk.bytes,
            encoding,
            content_hash: chunk.content_hash,
            language: chunk.language,
            generated: chunk.generated,
            trust: RepositoryDataTrust::UntrustedRepositoryData,
        });
    }
    let limiting_resources = Vec::new();
    let data = SourceReadQueryResult {
        generation: result.generation,
        chunks,
        conditional_activity: Vec::new(),
        execution: authoritative_execution(&limiting_resources),
    };
    finish_response(plan.explanation.clone(), data, tracker, started, &control)
}
