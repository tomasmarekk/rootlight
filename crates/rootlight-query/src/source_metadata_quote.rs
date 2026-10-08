//! Resident indexed metadata quotes for future bounded lossless source prefixes.
//! Bounds include fixed response and conditional projection costs but exclude
//! source body/presentation buffers and do not attest current source validity.

use crate::service::{QueryControl, UsageTracker, finish_response, serialized_size};
use crate::{
    ExecutionCompleteness, PlanEstimate, PlanExplanation, PlanKind, QueryBudget, QueryError,
    QueryOperator, QueryResource, QueryResponse, QueryUsage, RepositoryDataTrust,
    TokenAccountingProfile,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::GenerationId;
use rootlight_ir::SourceRef;
use rootlight_source::{
    LosslessTextReadOptions, SourceBudget, SourceMetadata, source_metadata_memory_upper,
    validate_source_metadata,
};
use rootlight_storage::GenerationSnapshot;
use serde::Serialize;
use std::{mem, time::Instant};

/// Cumulative fixed future cost for the ordered reference prefix.
///
/// Source bytes and encoded content are additional: physical at most `2*U*M`,
/// decoded at most `U*M`, and escaped JSON at most `6*U*M`. Allocator capacities
/// and actual read budgets are still checked by the later read operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourceTextPrefixFixedUpper {
    /// Fixed JSON bytes, including whole response wrappers and escaped metadata.
    pub json_bytes: u64,
    /// Fixed metadata, DTO, path reconstruction and conditional scratch memory.
    pub memory_bytes: u64,
    /// Maximum logical rows inspected by the future query projection.
    pub rows: u64,
    /// Maximum metadata/conditional traversal work outside source decoding.
    pub traversal_facts: u64,
    /// Number of ordered source targets, including distinct spans in one file.
    pub paths: u64,
}

/// One approved full reference and its cumulative future fixed upper bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceTextPrefixMetadataQuoteEntry {
    /// Full editing authority, including the original optional line hint.
    pub reference: SourceRef,
    /// Bound for all entries from zero through this entry, not a per-item delta.
    pub prefix_fixed_upper: SourceTextPrefixFixedUpper,
}

/// Body-free result bound to one already resident immutable generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceTextPrefixMetadataQuoteQueryResult {
    /// Resident generation whose indexed metadata was inspected.
    pub generation: GenerationId,
    /// Fixed upper policy revision; revision one uses cumulative bounds.
    pub quote_policy_version: u32,
    /// Ordered unique full selections; no source or path text is returned.
    pub entries: Vec<SourceTextPrefixMetadataQuoteEntry>,
    /// Completeness of metadata quoting, not source validity or freshness.
    pub execution: ExecutionCompleteness,
}

pub(super) fn execute(
    generation: &GenerationSnapshot,
    references: &[SourceRef],
    options: LosslessTextReadOptions,
    mut source_budget: SourceBudget,
    budget: QueryBudget,
    cancellation: &Cancellation,
) -> Result<QueryResponse<SourceTextPrefixMetadataQuoteQueryResult>, QueryError> {
    budget.validate()?;
    source_budget.validate()?;
    if !matches!(
        (options.context_lines_before, options.context_lines_after),
        (0, 0) | (1, 1)
    ) {
        return Err(QueryError::Source(
            rootlight_source::SourceError::ContextLimit,
        ));
    }
    let started = Instant::now();
    let control = QueryControl::new(cancellation, budget.max_duration());
    control.check()?;
    if references.is_empty() || references.len() > rootlight_source::HARD_MAX_SOURCE_SELECTORS {
        return Err(QueryError::Source(
            rootlight_source::SourceError::SelectorLimit,
        ));
    }
    let count = u64::try_from(references.len()).map_err(|_| QueryError::MemoryUnavailable)?;
    let mut tracker = UsageTracker::new(budget);
    let entry_memory = references
        .len()
        .checked_mul(
            mem::size_of::<SourceMetadata<'_>>()
                + mem::size_of::<SourceTextPrefixMetadataQuoteEntry>()
                + mem::size_of::<crate::conditional_activity::PrefixActivityUpper>(),
        )
        .ok_or(QueryError::MemoryUnavailable)?;
    tracker.add_memory(u64::try_from(entry_memory).map_err(|_| QueryError::MemoryUnavailable)?)?;
    let mut scratch = 0usize;
    for reference in references {
        control.check()?;
        tracker.add_rows(1)?;
        let file = generation
            .find_file(reference.span().file())
            .ok_or(QueryError::IndexDrift)?;
        scratch = scratch.max(source_metadata_memory_upper(file)?);
        let locator_bytes = if let Some(locator) = &file.path_locator {
            locator.components().iter().try_fold(0usize, |sum, part| {
                sum.checked_add(part.len())
                    .ok_or(QueryError::MemoryUnavailable)
            })?
        } else {
            0
        };
        let path_work = file
            .path
            .len()
            .checked_add(locator_bytes)
            .ok_or(QueryError::MemoryUnavailable)?;
        tracker.add_rows(u64::try_from(path_work).map_err(|_| QueryError::MemoryUnavailable)?)?;
    }
    tracker.add_memory(u64::try_from(scratch).map_err(|_| QueryError::MemoryUnavailable)?)?;
    let mapping_work = u64::try_from(generation.document().source_mappings.len())
        .map_err(|_| QueryError::MemoryUnavailable)?
        .checked_mul(count)
        .ok_or(QueryError::MemoryUnavailable)?;
    tracker.add_rows(mapping_work)?;
    source_budget.max_duration = source_budget.max_duration.min(budget.max_duration());
    let metadata =
        validate_source_metadata(generation, references, options, source_budget, cancellation)?;
    control.check()?;
    let conditional = crate::conditional_activity::prefix_metadata_upper(
        generation,
        references,
        budget,
        &mut tracker,
        &control,
    )?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(references.len())
        .map_err(|_| QueryError::MemoryUnavailable)?;
    let mut fixed_memory = 0u64;
    let mut traversal = 0u64;
    let mut fixed_json = fixed_response_json_upper(generation, &[], &control)?;
    for (index, item) in metadata.iter().enumerate() {
        control.check()?;
        tracker.add_rows(1)?;
        let slots = mem::size_of::<rootlight_source::SourceTextChunk>()
            + mem::size_of::<crate::QuerySourceTextChunk>()
            + mem::size_of::<SourceRef>();
        // Retain room for metadata copies and record slots across source,
        // query, wire and client projections; serialized ownership and body
        // buffers are charged separately by the consumer. Actual capacities
        // still fail closed under the later operation's admitted budget.
        let metadata_memory = item
            .path_memory_upper()
            .checked_add(
                item.file()
                    .path
                    .len()
                    .checked_add(item.file().language.len())
                    .and_then(|bytes| bytes.checked_mul(16))
                    .ok_or(QueryError::MemoryUnavailable)?,
            )
            .and_then(|bytes| bytes.checked_add(slots.checked_mul(16)?))
            .ok_or(QueryError::MemoryUnavailable)?;
        fixed_memory = fixed_memory
            .checked_add(u64::try_from(metadata_memory).map_err(|_| QueryError::MemoryUnavailable)?)
            .ok_or(QueryError::MemoryUnavailable)?;
        let target_traversal = u64::try_from(item.file().path.len())
            .map_err(|_| QueryError::MemoryUnavailable)?
            .checked_add(mapping_work / count)
            .and_then(|work| work.checked_add(1))
            .ok_or(QueryError::MemoryUnavailable)?;
        traversal = traversal
            .checked_add(target_traversal)
            .ok_or(QueryError::MemoryUnavailable)?;
        fixed_json = fixed_json
            .checked_add(serialized_size(
                &chunk_fixed_upper(item),
                u64::MAX,
                &control,
            )?)
            .and_then(|bytes| bytes.checked_add(u64::from(index != 0)))
            .ok_or(QueryError::MemoryUnavailable)?;
        let sidecar = conditional[index];
        entries.push(SourceTextPrefixMetadataQuoteEntry {
            reference: item.reference().clone(),
            prefix_fixed_upper: SourceTextPrefixFixedUpper {
                json_bytes: fixed_json
                    .checked_add(sidecar.json)
                    .ok_or(QueryError::MemoryUnavailable)?,
                memory_bytes: fixed_memory
                    .checked_add(sidecar.memory)
                    .ok_or(QueryError::MemoryUnavailable)?,
                rows: u64::try_from(index + 1)
                    .map_err(|_| QueryError::MemoryUnavailable)?
                    .checked_add(sidecar.rows)
                    .ok_or(QueryError::MemoryUnavailable)?,
                traversal_facts: traversal
                    .checked_add(sidecar.rows)
                    .ok_or(QueryError::MemoryUnavailable)?,
                paths: u64::try_from(index + 1).map_err(|_| QueryError::MemoryUnavailable)?,
            },
        });
        tracker.add_results(1)?;
    }
    let plan = quote_plan(generation, budget);
    finish_response(
        plan,
        SourceTextPrefixMetadataQuoteQueryResult {
            generation: generation.metadata().generation(),
            quote_policy_version: 1,
            entries,
            execution: ExecutionCompleteness::complete(),
        },
        tracker,
        started,
        &control,
    )
}

fn quote_plan(generation: &GenerationSnapshot, budget: QueryBudget) -> PlanExplanation {
    PlanExplanation {
        generation: generation.metadata().generation(),
        kind: PlanKind::SourceRead,
        operators: vec![
            QueryOperator::GenerationPin,
            QueryOperator::SourceResolve,
            QueryOperator::ConditionalActivityProjection,
            QueryOperator::OutputBudget,
        ],
        estimate: PlanEstimate {
            rows: budget.max_rows(),
            edges: 0,
            results: budget.max_results(),
            source_bytes: 0,
            memory_bytes: budget.max_memory_bytes(),
            json_bytes: budget.max_json_bytes(),
            estimated_tokens: budget.max_tokens(),
            duration_micros: u64::try_from(budget.max_duration().as_micros()).unwrap_or(u64::MAX),
        },
    }
}

#[derive(Serialize)]
struct ChunkFixedUpper<'a> {
    extracted_from: Option<&'a SourceRef>,
    reference: &'a SourceRef,
    path: &'a str,
    start_byte: u64,
    end_byte: u64,
    start_line: Option<u64>,
    end_line: Option<u64>,
    content: &'static str,
    physical_encoding: &'static str,
    content_hash: rootlight_ids::ContentHash,
    language: &'a str,
    generated: bool,
    trust: RepositoryDataTrust,
}

struct ChunkFixedUppers<'a>(&'a [SourceMetadata<'a>]);

fn chunk_fixed_upper<'a>(metadata: &SourceMetadata<'a>) -> ChunkFixedUpper<'a> {
    ChunkFixedUpper {
        extracted_from: metadata.extracted_from(),
        reference: metadata.reference(),
        path: &metadata.file().path,
        start_byte: u64::MAX,
        end_byte: u64::MAX,
        start_line: Some(u64::MAX),
        end_line: Some(u64::MAX),
        content: "",
        physical_encoding: "utf-16le",
        content_hash: metadata.file().content_hash,
        language: &metadata.file().language,
        generated: false,
        trust: RepositoryDataTrust::UntrustedRepositoryData,
    }
}

impl Serialize for ChunkFixedUppers<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for metadata in self.0 {
            sequence.serialize_element(&chunk_fixed_upper(metadata))?;
        }
        sequence.end()
    }
}

fn fixed_response_json_upper(
    generation: &GenerationSnapshot,
    metadata: &[SourceMetadata<'_>],
    control: &QueryControl<'_>,
) -> Result<u64, QueryError> {
    #[derive(Serialize)]
    struct ResultUpper<'a> {
        generation: GenerationId,
        chunks: ChunkFixedUppers<'a>,
        total_source_bytes: u64,
        total_text_bytes: u64,
        conditional_activity: [u8; 0],
        execution: ExecutionCompleteness,
    }
    #[derive(Serialize)]
    struct PrefixUpper<'a> {
        result: ResultUpper<'a>,
        prefix_truncated: bool,
    }
    // The complete query wrapper bounds plan, usage and completeness fields;
    // body strings are empty because their escaped bytes are reserved separately.
    let execution = ExecutionCompleteness::unsupported_partial(
        QueryResource::MemoryBytes,
        [
            QueryResource::Rows,
            QueryResource::Edges,
            QueryResource::Results,
            QueryResource::SourceBytes,
            QueryResource::JsonBytes,
            QueryResource::Tokens,
            QueryResource::Depth,
            QueryResource::Paths,
            QueryResource::Capability,
        ],
    );
    let mut plan = quote_plan(generation, QueryBudget::new());
    plan.operators.extend([
        QueryOperator::VfsSnapshotRead,
        QueryOperator::ContentHashVerify,
    ]);
    plan.estimate = PlanEstimate {
        rows: u64::MAX,
        edges: u64::MAX,
        results: u64::MAX,
        source_bytes: u64::MAX,
        memory_bytes: u64::MAX,
        json_bytes: u64::MAX,
        estimated_tokens: u64::MAX,
        duration_micros: u64::MAX,
    };
    let response = QueryResponse {
        plan,
        data: PrefixUpper {
            result: ResultUpper {
                generation: generation.metadata().generation(),
                chunks: ChunkFixedUppers(metadata),
                total_source_bytes: u64::MAX,
                total_text_bytes: u64::MAX,
                conditional_activity: [],
                execution,
            },
            prefix_truncated: false,
        },
        usage: QueryUsage {
            rows: u64::MAX,
            edges: u64::MAX,
            results: u64::MAX,
            source_bytes: u64::MAX,
            json_bytes: u64::MAX,
            estimated_tokens: u64::MAX,
            token_accounting: TokenAccountingProfile::Utf8ByteUpperBoundV1,
            memory_bytes: u64::MAX,
            elapsed_micros: u64::MAX,
        },
    };
    serialized_size(&response, u64::MAX, control)
}
