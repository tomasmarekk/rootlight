//! Future conditional projection bounds for resident source-prefix metadata.
//! Inventory validation shares the real read path; assessed range variants are
//! bounded independently because context and clipping require source bodies.

use super::*;
use crate::service::serialized_size;

#[derive(Clone, Copy, Default)]
pub(crate) struct PrefixActivityUpper {
    pub memory: u64,
    pub rows: u64,
    pub json: u64,
}

pub(crate) fn prefix_metadata_upper(
    generation: &GenerationSnapshot,
    references: &[SourceRef],
    query_budget: crate::QueryBudget,
    tracker: &mut UsageTracker,
    control: &QueryControl<'_>,
) -> Result<Vec<PrefixActivityUpper>, QueryError> {
    let count = references.len();
    tracker.add_memory(mandatory_memory(count)?)?;
    let mut upper = Vec::new();
    upper
        .try_reserve_exact(count)
        .map_err(|_| QueryError::MemoryUnavailable)?;
    upper.resize(count, PrefixActivityUpper::default());
    let mut files = BTreeMap::new();
    for source in references {
        control.check()?;
        let file = generation
            .find_file(source.span().file())
            .ok_or(QueryError::IndexDrift)?;
        if applicable(&file.language) {
            files.insert(
                file.id,
                SelectedFile {
                    content_hash: file.content_hash,
                    byte_length: file.byte_length,
                },
            );
        }
    }
    if files.is_empty() {
        return Ok(upper);
    }
    let document = generation.document();
    let mut inventories = BTreeMap::new();
    let mut decoder_high_water = 0;
    let mut decoder_prefix = [0u64; rootlight_source::HARD_MAX_SOURCE_SELECTORS];
    let mut limiting = Vec::new();
    let mut budget = ProjectionBudget {
        tracker,
        control,
        limiting_resources: &mut limiting,
    };
    for envelope in &document.extensions {
        if !budget.rows(1)? {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::Rows,
                limit: query_budget.max_rows(),
            });
        }
        if envelope.namespace != VBA_COMPILATION_NAMESPACE {
            continue;
        }
        let source = envelope
            .evidence
            .source
            .as_ref()
            .ok_or(QueryError::IndexDrift)?;
        if !files.contains_key(&source.span().file()) {
            continue;
        }
        let decode_memory = decoder_memory(envelope.payload.len())?;
        if !budget.memory(decode_memory.saturating_sub(decoder_high_water))? {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: query_budget.max_memory_bytes(),
            });
        }
        if !budget.rows(checked_usize_to_u64(rootlight_ir::MAX_VBA_COMPILATION_BRANCHES)? + 1)? {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::Rows,
                limit: query_budget.max_rows(),
            });
        }
        decoder_high_water = decoder_high_water.max(decode_memory);
        let model =
            decode_vba_compilation_envelope(envelope).map_err(|_| QueryError::IndexDrift)?;
        if !validate_inventory(document, &files, envelope, &model, &mut budget)? {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::Rows,
                limit: query_budget.max_rows(),
            });
        }
        let key = (
            model.scope.file(),
            model.scope.start_byte(),
            model.scope.end_byte(),
        );
        validate_scope(&inventories, key)?;
        let retained = inventory_memory(&model)?;
        if !budget.memory(retained)? {
            return Err(QueryError::BudgetExceeded {
                resource: QueryResource::MemoryBytes,
                limit: query_budget.max_memory_bytes(),
            });
        }
        let first = references
            .iter()
            .position(|reference| reference.span().file() == model.scope.file())
            .ok_or(QueryError::IndexDrift)?;
        let parser = document
            .provenance
            .binary_search_by_key(&envelope.provenance, |record| record.id)
            .ok()
            .and_then(|index| document.provenance.get(index))
            .ok_or(QueryError::IndexDrift)?;
        let decode_rows = checked_usize_to_u64(rootlight_ir::MAX_VBA_COMPILATION_BRANCHES)?
            .checked_add(2)
            .and_then(|rows| rows.checked_add(u64::try_from(parser.input_sources.len()).ok()?))
            .ok_or(QueryError::MemoryUnavailable)?;
        for item in &mut upper[first..] {
            item.memory = item
                .memory
                .checked_add(retained)
                .ok_or(QueryError::MemoryUnavailable)?;
            item.rows = item
                .rows
                .checked_add(decode_rows)
                .ok_or(QueryError::MemoryUnavailable)?;
        }
        for maximum in &mut decoder_prefix[first..count] {
            *maximum = (*maximum).max(decode_memory);
        }
        inventories.insert(
            key,
            Inventory {
                model,
                fact_id: envelope.id,
                parser_provenance_id: envelope.provenance,
            },
        );
    }
    let mut assessments = 0usize;
    let mut accumulated_rows = 0u64;
    let mut accumulated_json = 0u64;
    for (index, reference) in references.iter().enumerate() {
        control.check()?;
        if let Some(file) = files.get(&reference.span().file()) {
            assessments += 1;
            let bounds = inventories.range(
                (reference.span().file(), 0, 0)..=(reference.span().file(), u64::MAX, u64::MAX),
            );
            let mut inventory_count = 0u64;
            let mut maximum_activity = 0u64;
            let assessed = SourceRef::new(
                reference.repository(),
                reference.generation(),
                SourceSpan::new(reference.span().file(), file.byte_length, file.byte_length)
                    .map_err(|_| QueryError::IndexDrift)?,
                reference.content_hash(),
                None,
            );
            let mut candidate = ConditionalActivityAssessment {
                result_index: u32::try_from(index).map_err(|_| QueryError::MemoryUnavailable)?,
                basis: ConditionalActivityBasis::ReturnedRange,
                activity: ConditionalActivityState::Unproven,
                assessed_source_ref: Some(assessed),
                inventory: None,
                reason: Some(ConditionalActivityReason::TransportUnavailable),
            };
            let mut maximum_json = serialized_size(&candidate, u64::MAX, control)?;
            for (_, inventory) in bounds {
                tracker.add_rows(1)?;
                inventory_count = inventory_count
                    .checked_add(1)
                    .ok_or(QueryError::MemoryUnavailable)?;
                maximum_activity = maximum_activity.max(
                    checked_usize_to_u64(inventory.model.branches.len())?
                        .checked_mul(4)
                        .and_then(|rows| rows.checked_add(1))
                        .ok_or(QueryError::MemoryUnavailable)?,
                );
                candidate.inventory = Some(ConditionalActivityInventory {
                    fact_id: inventory.fact_id,
                    parser_provenance_id: inventory.parser_provenance_id,
                    scope: ConditionalActivityScope {
                        start_byte: inventory.model.scope.start_byte(),
                        end_byte: inventory.model.scope.end_byte(),
                    },
                    complete: false,
                });
                maximum_json = maximum_json.max(serialized_size(&candidate, u64::MAX, control)?);
            }
            let probe_rows = inventory_count
                .checked_add(maximum_activity)
                .and_then(|rows| rows.checked_add(1))
                .ok_or(QueryError::MemoryUnavailable)?;
            accumulated_rows = accumulated_rows
                .checked_add(probe_rows)
                .ok_or(QueryError::MemoryUnavailable)?;
            accumulated_json = accumulated_json
                .checked_add(maximum_json)
                .and_then(|bytes| bytes.checked_add(1))
                .ok_or(QueryError::MemoryUnavailable)?;
        }
        if assessments != 0 {
            upper[index].memory = upper[index]
                .memory
                .checked_add(decoder_prefix[index])
                .and_then(|memory| memory.checked_add(mandatory_memory(assessments).ok()?))
                .ok_or(QueryError::MemoryUnavailable)?;
            upper[index].rows = upper[index]
                .rows
                .checked_add(checked_usize_to_u64(document.extensions.len())?)
                .and_then(|rows| rows.checked_add(accumulated_rows))
                .ok_or(QueryError::MemoryUnavailable)?;
            upper[index].json = accumulated_json;
        }
    }
    Ok(upper)
}
