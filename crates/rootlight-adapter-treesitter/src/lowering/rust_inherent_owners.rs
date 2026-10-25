//! Authenticated Rust method owners with combined native workspace admission.
//! Optional staging preserves lexical records and explicit gaps on refusal.

use super::*;
use std::mem::size_of;

#[cfg(test)]
#[path = "rust_inherent_owners_tests.rs"]
mod tests;

pub(super) struct Outcome {
    pub(super) gaps: Vec<SourceSpan>,
    pub(super) modeled_peak: usize,
    #[cfg(test)]
    optional_index_entries: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    RequiredGaps,
    OptionalIndex,
    OptionalUpdates,
}

#[derive(Default)]
struct Allocation {
    fail: Option<Stage>,
}

impl Allocation {
    fn admit(&self, stage: Stage) -> Result<(), AdapterError> {
        if self.fail == Some(stage) {
            return Err(SinkError::AllocationFailed.into());
        }
        Ok(())
    }
}

fn add(left: usize, right: usize) -> Result<usize, AdapterError> {
    left.checked_add(right)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn slots(count: usize, width: usize) -> Result<usize, AdapterError> {
    count
        .checked_mul(width)
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn signature_bytes(signature: &Option<SignatureEvidence>) -> Result<usize, AdapterError> {
    match signature {
        Some(SignatureEvidence::Contiguous(text)) => Ok(text.capacity()),
        Some(SignatureEvidence::Projected(projection)) => add(
            slots(projection.text().len(), 2)?,
            slots(
                projection.parts().len(),
                2 * size_of::<rootlight_ir::SignatureSourcePart>(),
            )?,
        ),
        None => Ok(0),
    }
}

// This describes retained known Rust lowering owners, not allocator/RSS peaks.
// Existing non-Rust plans have no Rust captures; preflight weights retain their
// fixed wrappers and the pending output's collection/string upper bounds.
pub(super) fn resident_bytes(
    plan: &EntityPlan,
    entities: &HashMap<u64, MaterializedEntity>,
    parse: &ParseOutput,
    source_bytes: usize,
    view_owned_bytes: usize,
    budget: &LoweringBudget,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut bytes = add(size_of::<EntityPlan>(), slots(source_bytes, 2)?)?;
    bytes = add(bytes, view_owned_bytes)?;
    // Includes physical parse storage and the optional inverse-coordinate copy.
    bytes = add(bytes, slots(parse.facts().len(), 1024)?)?;
    for fact in parse.facts() {
        cancellation.check()?;
        bytes = add(bytes, slots(fact.syntax_kind().as_str().len(), 4)?)?;
    }
    bytes = add(bytes, slots(parse.diagnostics().len(), 256)?)?;
    for diagnostic in parse.diagnostics() {
        cancellation.check()?;
        bytes = add(bytes, slots(diagnostic.code().as_str().len(), 2)?)?;
    }
    bytes = add(bytes, slots(budget.records, 256)?)?;
    bytes = add(bytes, slots(budget.nested_items, 64)?)?;
    bytes = add(bytes, slots(budget.string_bytes, 4)?)?;
    bytes = add(bytes, plan.rust_bindings.modeled_owned_bytes(cancellation)?)?;
    bytes = add(bytes, slots(plan.nearest_entity_ancestor.capacity(), 128)?)?;
    for count in [
        plan.haskell_annotations.capacity(),
        plan.haskell_orphan_signatures.len(),
        plan.haskell_module_gaps.len(),
        plan.nix_path_gaps.len(),
        plan.groovy_name_gaps.len(),
        plan.unsupported_scope_entities.len(),
        plan.duplicate_data_keys.len(),
        plan.yaml_aliases.capacity(),
        plan.yaml_key_sources.capacity(),
        plan.yaml_warnings.capacity(),
    ] {
        bytes = add(bytes, slots(count, 256)?)?;
    }

    bytes = add(
        bytes,
        slots(plan.drafts.capacity(), size_of::<EntityDraft>())?,
    )?;
    for draft in &plan.drafts {
        cancellation.check()?;
        for string in [&draft.name, &draft.signature, &draft.language] {
            bytes = add(bytes, string.capacity())?;
        }
        for string in [&draft.data_qualified_name, &draft.qualified_prefix]
            .into_iter()
            .flatten()
        {
            bytes = add(bytes, string.capacity())?;
        }
        bytes = add(bytes, signature_bytes(&draft.signature_evidence)?)?;
    }
    bytes = add(
        bytes,
        slots(
            entities.capacity(),
            size_of::<(u64, MaterializedEntity)>() + 128,
        )?,
    )?;
    for entity in entities.values() {
        cancellation.check()?;
        for string in [
            &entity.record.language,
            &entity.record.canonical_name,
            &entity.record.display_name,
            &entity.record.qualified_name,
            &entity.identity_claim.language,
            &entity.identity_claim.declared_identity,
        ] {
            bytes = add(bytes, string.capacity())?;
        }
        bytes = add(bytes, entity.identity_claim.container_identity.capacity())?;
        bytes = add(
            bytes,
            entity.identity_claim.signature_discriminator.capacity(),
        )?;
        bytes = add(
            bytes,
            entity.identity_claim.build_context_discriminator.capacity(),
        )?;
        bytes = add(
            bytes,
            slots(entity.record.flags.capacity(), size_of::<EntityFlag>())?,
        )?;
        bytes = add(
            bytes,
            slots(
                entity.record.evidence.derivation.capacity(),
                size_of::<FactRef>(),
            )?,
        )?;
        bytes = add(bytes, signature_bytes(&entity.signature_evidence)?)?;
    }
    Ok(bytes)
}

fn eligible(draft: &EntityDraft, entities: &HashMap<u64, MaterializedEntity>) -> bool {
    draft.language == "rust"
        && draft.kind == EntityKind::Method
        && draft.scope_identity.is_some()
        && draft.parent_entity == draft.lexical_module
        && entities.get(&draft.local_id).is_some_and(|entity| {
            entity.record.language == "rust" && entity.record.kind == EntityKind::Method
        })
}

pub(super) fn has_candidates(
    drafts: &[EntityDraft],
    entities: &HashMap<u64, MaterializedEntity>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    for draft in drafts {
        cancellation.check()?;
        if eligible(draft, entities) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn required_gaps(
    drafts: &[EntityDraft],
    entities: &HashMap<u64, MaterializedEntity>,
    resident: usize,
    maximum: usize,
    allocation: &Allocation,
    cancellation: &Cancellation,
) -> Result<(Vec<SourceSpan>, usize), AdapterError> {
    let mut count = 0usize;
    for draft in drafts {
        cancellation.check()?;
        if eligible(draft, entities) {
            count = add(count, 1)?;
        }
    }
    let required = add(resident, slots(count, size_of::<SourceSpan>())?)?;
    require_resource_limit(ResourceKind::ReportedMemoryBytes, required, maximum)?;
    allocation.admit(Stage::RequiredGaps)?;
    let mut gaps = Vec::new();
    gaps.try_reserve_exact(count)
        .map_err(|_| SinkError::AllocationFailed)?;
    let peak = add(resident, slots(gaps.capacity(), size_of::<SourceSpan>())?)?;
    require_resource_limit(ResourceKind::ReportedMemoryBytes, peak, maximum)?;
    for draft in drafts {
        cancellation.check()?;
        if eligible(draft, entities) {
            gaps.push(draft.span);
        }
    }
    Ok((gaps, peak))
}

#[expect(
    clippy::too_many_arguments,
    reason = "source and all retained owners share one combined admission"
)]
pub(super) fn reconcile(
    entities: &mut HashMap<u64, MaterializedEntity>,
    drafts: &[EntityDraft],
    facts: &[SyntaxFact],
    source: &SourceRef,
    bytes: &[u8],
    complete: bool,
    resident: usize,
    limits: &AnalysisLimits,
    cancellation: &Cancellation,
) -> Result<Outcome, AdapterError> {
    let allocation = Allocation::default();
    let maximum = limits.max_reported_memory_bytes();
    let (gaps, base) = required_gaps(
        drafts,
        entities,
        resident,
        maximum,
        &allocation,
        cancellation,
    )?;
    let empty = |gaps, modeled_peak| Outcome {
        gaps,
        modeled_peak,
        #[cfg(test)]
        optional_index_entries: 0,
    };
    if gaps.is_empty() || !complete {
        return Ok(empty(gaps, base));
    }
    // This is configured credit, not observed cost. A mathematical ceiling above
    // usize::MAX admits every representable cost while actual models stay checked.
    let work = limits
        .max_source_bytes()
        .saturating_mul(limits.max_syntax_nodes());
    let native_work = native_work(drafts.len(), entities.len(), resident)?;
    let Some(remaining_work) = work.checked_sub(native_work) else {
        return Ok(empty(gaps, base));
    };
    let sdk_bound = add(slots(bytes.len(), 128)?, slots(facts.len(), 512)?)?;
    let sdk_peak = add(base, sdk_bound)?;
    if sdk_peak > maximum {
        return Ok(empty(gaps, base));
    }
    let plan = match rootlight_adapter_sdk::rust_inherent_owner_plan(
        facts,
        source,
        bytes,
        complete,
        limits.syntax_stream().max_records(),
        limits.max_source_bytes(),
        limits.max_syntax_nodes(),
        remaining_work,
        limits.ir().max_string_bytes,
        maximum - base,
        cancellation,
    ) {
        Ok(plan) => plan,
        Err(AdapterError::Sink(SinkError::AllocationFailed)) => return Ok(empty(gaps, sdk_peak)),
        Err(error) => return Err(error),
    };
    consume_plan(
        entities,
        drafts,
        source,
        plan,
        gaps,
        resident,
        sdk_peak,
        maximum,
        &allocation,
        cancellation,
    )
}

fn native_work(
    drafts: usize,
    entities: usize,
    payload_model: usize,
) -> Result<usize, AdapterError> {
    let entries = add(drafts, entities)?;
    let logarithm = usize::try_from(usize::BITS - entries.max(1).leading_zeros())
        .map_err(|_| SinkError::AccountingOverflow)?;
    add(
        slots(slots(entries, add(logarithm, 16)?)?, 8)?,
        payload_model,
    )
}

fn sort<T>(
    values: &mut [T],
    cancellation: &Cancellation,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering + Copy,
) -> Result<(), AdapterError> {
    fn sift<T>(
        values: &mut [T],
        mut parent: usize,
        cancellation: &Cancellation,
        compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    ) -> Result<(), AdapterError> {
        loop {
            cancellation.check()?;
            let child = parent
                .checked_mul(2)
                .and_then(|value| value.checked_add(1))
                .ok_or(SinkError::AccountingOverflow)?;
            if child >= values.len() {
                return Ok(());
            }
            let right = child.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            let selected =
                if right < values.len() && compare(&values[child], &values[right]).is_lt() {
                    right
                } else {
                    child
                };
            if !compare(&values[parent], &values[selected]).is_lt() {
                return Ok(());
            }
            values.swap(parent, selected);
            parent = selected;
        }
    }
    for parent in (0..values.len() / 2).rev() {
        sift(values, parent, cancellation, compare)?;
    }
    for end in (1..values.len()).rev() {
        cancellation.check()?;
        values.swap(0, end);
        sift(&mut values[..end], 0, cancellation, compare)?;
    }
    cancellation.check()?;
    Ok(())
}

struct Index<'a> {
    local: u64,
    draft: &'a EntityDraft,
    identity_valid: bool,
    gap: Option<usize>,
}
struct Update {
    local: u64,
    owner: SymbolId,
    gap: usize,
}

fn optional_bytes(drafts: usize, entities: usize, proofs: usize) -> Result<usize, AdapterError> {
    add(
        add(
            slots(drafts, size_of::<Index<'_>>())?,
            slots(entities, size_of::<SymbolId>())?,
        )?,
        slots(proofs, size_of::<Update>())?,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "staging holds mandatory gaps and SDK proof under the same resident budget"
)]
fn consume_plan(
    entities: &mut HashMap<u64, MaterializedEntity>,
    drafts: &[EntityDraft],
    source: &SourceRef,
    plan: rootlight_adapter_sdk::RustInherentOwnerPlan,
    mut gaps: Vec<SourceSpan>,
    resident: usize,
    previous_peak: usize,
    maximum: usize,
    allocation: &Allocation,
    cancellation: &Cancellation,
) -> Result<Outcome, AdapterError> {
    if plan.source != *source {
        return Err(provider_failure("rust-inherent-owner-frame"));
    }
    let rootlight_adapter_sdk::RustInherentOwnerPlan {
        proofs,
        fields,
        unproven_methods,
        ..
    } = plan;
    drop(fields);
    drop(unproven_methods);
    let base = add(resident, slots(gaps.capacity(), size_of::<SourceSpan>())?)?;
    let held = slots(
        proofs.capacity(),
        size_of::<rootlight_adapter_sdk::RustInherentOwnerProof>(),
    )?;
    let mut peak = previous_peak.max(add(base, held)?);
    let empty = |gaps, modeled_peak| Outcome {
        gaps,
        modeled_peak,
        #[cfg(test)]
        optional_index_entries: 0,
    };
    if proofs.is_empty() {
        return Ok(empty(gaps, peak));
    }
    let bound = add(
        add(base, held)?,
        optional_bytes(drafts.len(), entities.len(), proofs.len())?,
    )?;
    if bound > maximum {
        return Ok(empty(gaps, peak));
    }
    let staged = (|| -> Result<_, AdapterError> {
        allocation.admit(Stage::OptionalIndex)?;
        let mut index = Vec::new();
        index
            .try_reserve_exact(drafts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut symbols = Vec::new();
        symbols
            .try_reserve_exact(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut updates = Vec::new();
        allocation.admit(Stage::OptionalUpdates)?;
        updates
            .try_reserve_exact(proofs.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let actual = add(
            add(base, held)?,
            add(
                add(
                    slots(index.capacity(), size_of::<Index<'_>>())?,
                    slots(symbols.capacity(), size_of::<SymbolId>())?,
                )?,
                slots(updates.capacity(), size_of::<Update>())?,
            )?,
        )?;
        require_resource_limit(ResourceKind::ReportedMemoryBytes, actual, maximum)?;
        peak = peak.max(actual);
        let mut gap = 0usize;
        for draft in drafts {
            cancellation.check()?;
            let owns_gap = eligible(draft, entities);
            let identity_valid = entities.get(&draft.local_id).is_some_and(|entity| {
                same_frame(entity, source)
                    && !entity.record.flags.contains(&EntityFlag::Synthetic)
                    && entity.identity_claim.repository == entity.record.repository
                    && entity.identity_claim.language == entity.record.language
                    && entity.identity_claim.kind == entity.record.kind
                    && entity.identity_claim.declared_identity == entity.record.canonical_name
                    && entity.identity_claim.symbol == entity.record.id
                    && entity.identity_claim.derived_symbol() == entity.record.id
            });
            index.push(Index {
                local: draft.local_id,
                draft,
                identity_valid,
                gap: owns_gap.then_some(gap),
            });
            if owns_gap {
                gap = add(gap, 1)?;
            }
        }
        for entity in entities.values() {
            cancellation.check()?;
            symbols.push(entity.record.id);
        }
        sort(&mut index, cancellation, |a, b| a.local.cmp(&b.local))?;
        sort(&mut symbols, cancellation, Ord::cmp)?;
        let lookup = |local| {
            index
                .binary_search_by_key(&local, |entry| entry.local)
                .ok()
                .map(|position| &index[position])
        };
        let unique = |symbol| {
            symbols
                .partition_point(|candidate| *candidate < symbol)
                .checked_add(1)
                == Some(symbols.partition_point(|candidate| *candidate <= symbol))
        };
        for proof in &proofs {
            cancellation.check()?;
            let (Some(method), Some(owner), Some(method_index), Some(owner_index)) = (
                entities.get(&proof.method),
                entities.get(&proof.owner),
                lookup(proof.method),
                lookup(proof.owner),
            ) else {
                continue;
            };
            let Some(gap) = method_index.gap else {
                continue;
            };
            let expected = if let Some(module) = proof.lexical_module {
                let Some(module) = entities.get(&module).filter(|entity| {
                    entity.record.kind == EntityKind::Module && same_frame(entity, source)
                }) else {
                    continue;
                };
                ContainerRef::Entity(module.record.id)
            } else {
                ContainerRef::File(source.span().file())
            };
            let valid = method.record.kind == EntityKind::Method
                && owner.record.kind == EntityKind::Struct
                && method.record.id != owner.record.id
                && unique(method.record.id)
                && unique(owner.record.id)
                && method_index.identity_valid
                && owner_index.identity_valid
                && method.definition_local_id == Some(proof.definition)
                && owner.definition_local_id == Some(proof.owner_definition)
                && method_index.draft.lexical_module == proof.lexical_module
                && owner_index.draft.lexical_module == proof.lexical_module
                && method.record.provenance == owner.record.provenance
                && [method, owner].into_iter().all(|entity| {
                    entity.direct_parent == expected
                        && entity.record.container == Some(expected)
                        && entity.identity_claim.container == Some(expected)
                })
                && method
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|written| written.span() == method_index.draft.span)
                && owner
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|written| written.span() == owner_index.draft.span);
            if valid {
                updates.push(Update {
                    local: proof.method,
                    owner: owner.record.id,
                    gap,
                });
            }
        }
        sort(&mut updates, cancellation, |a, b| a.gap.cmp(&b.gap))?;
        if updates.windows(2).any(|pair| pair[0].gap == pair[1].gap) {
            return Err(provider_failure("rust-inherent-owner-duplicate"));
        }
        Ok(updates)
    })();
    let updates = match staged {
        Ok(updates) => updates,
        Err(AdapterError::Sink(SinkError::AllocationFailed)) => return Ok(empty(gaps, peak)),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::ReportedMemoryBytes,
            ..
        })) => return Ok(empty(gaps, peak)),
        Err(error) => return Err(error),
    };
    cancellation.check()?;
    let mut proven = updates.iter().peekable();
    let mut position = 0usize;
    gaps.retain(|_| {
        let keep = proven.peek().is_none_or(|update| update.gap != position);
        if !keep {
            proven.next();
        }
        position += 1;
        keep
    });
    cancellation.check()?;
    // Symbol derivation remains lexical; only the proved semantic parent moves.
    for update in updates {
        let method = entities
            .get_mut(&update.local)
            .expect("staged local remains in immutable entity map");
        let parent = ContainerRef::Entity(update.owner);
        method.record.container = Some(parent);
        method.identity_claim.container = Some(parent);
        method.direct_parent = parent;
    }
    Ok(Outcome {
        gaps,
        modeled_peak: peak,
        #[cfg(test)]
        optional_index_entries: drafts.len(),
    })
}

fn same_frame(entity: &MaterializedEntity, source: &SourceRef) -> bool {
    entity.record.language == "rust"
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity
            .record
            .evidence
            .source
            .as_ref()
            .is_some_and(|written| {
                written.repository() == source.repository()
                    && written.generation() == source.generation()
                    && written.content_hash() == source.content_hash()
                    && span_contains(source.span(), written.span())
            })
}
