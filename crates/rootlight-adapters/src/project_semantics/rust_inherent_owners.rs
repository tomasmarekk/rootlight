//! Project ownership projection after legacy Rust symbols have been materialized.
//!
//! The shared source proof selects candidates; retained records must independently
//! agree before their semantic containers change. Identity recipes stay lexical.

use super::*;
use rootlight_adapter_sdk::{RustInherentOwnerProof, rust_inherent_owner_plan};
use rootlight_ir::{
    ExtensionEnvelope, SYMBOL_IDENTITY_CLAIM_NAMESPACE,
    decode_symbol_identity_claim_envelope_with_checkpoint,
};

#[derive(Debug)]
pub(super) struct DefinitionOwner {
    source: SourceRef,
    method: SymbolId,
    owner: SymbolId,
}

type DefinitionOwners = BTreeMap<SourceSpan, DefinitionOwner>;

#[derive(Default)]
struct RecordIndex {
    entities: BTreeMap<SymbolId, Option<usize>>,
    claims: BTreeMap<SymbolId, Option<usize>>,
    declares: BTreeMap<SymbolId, Option<usize>>,
    contains: BTreeSet<SymbolId>,
    semantic: BTreeMap<SymbolId, Option<usize>>,
    declarations: BTreeMap<(FileId, u64), Option<usize>>,
    definitions: BTreeMap<(FileId, u64), Option<usize>>,
}

#[derive(Default)]
struct Candidates {
    symbols: BTreeSet<SymbolId>,
    declarations: BTreeSet<(FileId, u64)>,
}

fn insert_unique<K: Ord>(map: &mut BTreeMap<K, Option<usize>>, key: K, index: usize) {
    map.entry(key)
        .and_modify(|value| *value = None)
        .or_insert(Some(index));
}

impl RecordIndex {
    fn new(
        builder: &ProjectFactsBuilder<'_, '_, '_>,
        candidates: &Candidates,
    ) -> Result<Self, AdapterError> {
        let mut index = Self::default();
        for (position, record) in builder.records.iter().enumerate() {
            check_periodically(position, builder.cancellation)?;
            match record {
                IrRecord::Entity(entity) if candidates.symbols.contains(&entity.id) => {
                    insert_unique(&mut index.entities, entity.id, position);
                }
                IrRecord::Extension(envelope)
                    if envelope.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE =>
                {
                    if let [FactRef::Entity(symbol)] = envelope.evidence.derivation.as_slice()
                        && candidates.symbols.contains(symbol)
                    {
                        insert_unique(&mut index.claims, *symbol, position);
                    }
                }
                IrRecord::Relation(relation) => {
                    if let RelationEndpoint::Entity(symbol) = relation.object
                        && candidates.symbols.contains(&symbol)
                    {
                        if relation.predicate == RelationPredicate::Declares {
                            insert_unique(&mut index.declares, symbol, position);
                        } else if relation.predicate == RelationPredicate::Contains {
                            index.contains.insert(symbol);
                        }
                    }
                }
                _ => {}
            }
        }
        for (position, entity) in builder.entities.iter().enumerate() {
            check_periodically(position, builder.cancellation)?;
            if candidates.symbols.contains(&entity.symbol) {
                insert_unique(&mut index.semantic, entity.symbol, position);
            }
        }
        for (position, declaration) in builder.declarations.iter().enumerate() {
            check_periodically(position, builder.cancellation)?;
            let key = (declaration.file, declaration.local_id);
            if candidates.declarations.contains(&key) {
                insert_unique(&mut index.declarations, key, position);
            }
        }
        for (position, occurrence) in builder.occurrences.iter().enumerate() {
            check_periodically(position, builder.cancellation)?;
            if occurrence.role == OccurrenceRole::Definition
                && let Some(declaration) = occurrence.enclosing_declaration
                && candidates
                    .declarations
                    .contains(&(occurrence.file, declaration))
            {
                insert_unique(
                    &mut index.definitions,
                    (occurrence.file, declaration),
                    position,
                );
            }
        }
        Ok(index)
    }
}

struct Retained<'a> {
    symbol: SymbolId,
    draft: &'a DeclarationDraft,
    entity_index: usize,
    entity: &'a EntityRecord,
    claim_index: usize,
    claim: SymbolIdentityClaim,
}

fn retained<'a>(
    builder: &'a ProjectFactsBuilder<'_, '_, '_>,
    index: &RecordIndex,
    file: FileId,
    local: u64,
    kind: EntityKind,
    budget: &mut ProjectionBudget,
) -> Result<Option<Retained<'a>>, AdapterError> {
    let Some(draft) = index
        .declarations
        .get(&(file, local))
        .copied()
        .flatten()
        .and_then(|position| builder.declarations.get(position))
    else {
        return Ok(None);
    };
    let Some(symbol) = builder.symbol_by_declaration.get(&(file, local)).copied() else {
        return Ok(None);
    };
    let Some(entity_index) = index.entities.get(&symbol).copied().flatten() else {
        return Ok(None);
    };
    let Some(claim_index) = index.claims.get(&symbol).copied().flatten() else {
        return Ok(None);
    };
    let Some(IrRecord::Entity(entity)) = builder.records.get(entity_index) else {
        return Ok(None);
    };
    let Some(IrRecord::Extension(envelope)) = builder.records.get(claim_index) else {
        return Ok(None);
    };
    let Some(semantic) = index
        .semantic
        .get(&symbol)
        .copied()
        .flatten()
        .and_then(|position| builder.entities.get(position))
    else {
        return Ok(None);
    };
    if draft.kind != kind
        || draft.synthetic
        || draft.rust_local
        || entity.kind != kind
        || entity.language != "rust"
        || entity.flags.contains(&EntityFlag::Synthetic)
        || entity.canonical_name != draft.name
        || entity.evidence.source.as_ref() != Some(&draft.source)
        || semantic.file != file
        || semantic.span != draft.span
        || semantic.source != draft.source
        || semantic.kind != kind
        || semantic.name != draft.name
        || !entity.evidence.derivation.is_empty()
        || entity.repository != draft.source.repository()
        || entity.generation != draft.source.generation()
        || entity.provenance != builder.provenance_for_file(file)?
        || envelope.repository != entity.repository
        || envelope.generation != entity.generation
        || envelope.provenance != entity.provenance
        || envelope.evidence.source != entity.evidence.source
    {
        return Ok(None);
    }
    budget.charge(
        envelope
            .payload
            .len()
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(4096))
            .ok_or(SinkError::AccountingOverflow)?,
    )?;
    let decoded = decode_symbol_identity_claim_envelope_with_checkpoint(envelope, || {
        builder.cancellation.check().is_ok()
    });
    builder.cancellation.check()?;
    let claim = decoded.map_err(|_| provider_failure("project-rust-owner-claim"))?;
    if claim.symbol != symbol
        || claim.derived_symbol() != symbol
        || claim.repository != entity.repository
        || claim.language != entity.language
        || claim.kind != kind
        || claim.container != entity.container
        || claim.declared_identity != draft.name
        || claim.signature_discriminator.as_slice() != draft.signature.as_bytes()
        || claim.build_context_discriminator.as_slice()
            != builder.request.build_context().digest().as_bytes()
    {
        return Ok(None);
    }
    Ok(Some(Retained {
        symbol,
        draft,
        entity_index,
        entity,
        claim_index,
        claim,
    }))
}

fn definition_matches(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    index: &RecordIndex,
    input: &ProjectSourceInput<'_>,
    facts: &BTreeMap<u64, &SyntaxFact>,
    declaration: &Retained<'_>,
    definition: u64,
) -> Option<SourceRef> {
    let fact = facts.get(&definition)?;
    let occurrence = index
        .definitions
        .get(&(declaration.draft.file, declaration.draft.local_id))
        .copied()
        .flatten()
        .and_then(|position| builder.occurrences.get(position))?;
    let source = source_for_span(input, fact.span());
    let start = usize::try_from(fact.span().start_byte()).ok()?;
    let end = usize::try_from(fact.span().end_byte()).ok()?;
    let spelling = input.source().bytes().get(start..end)?;
    (is_definition_fact(fact)
        && occurrence.name == declaration.draft.name
        && occurrence.source == source
        && occurrence.spelling_hash == content_hash(spelling)
        && occurrence.file == declaration.draft.file
        && occurrence.enclosing_declaration == Some(declaration.draft.local_id))
    .then_some(source)
}

struct Replacement {
    method_span: SourceSpan,
    entity_index: usize,
    entity: EntityRecord,
    claim_index: usize,
    claim: ExtensionEnvelope,
    declares_index: usize,
    declares: RelationRecord,
    contains: RelationRecord,
    definition: DefinitionOwner,
}

fn prepare(
    builder: &ProjectFactsBuilder<'_, '_, '_>,
    index: &RecordIndex,
    input: &ProjectSourceInput<'_>,
    facts: &BTreeMap<u64, &SyntaxFact>,
    proof: &RustInherentOwnerProof,
    kind: EntityKind,
    budget: &mut ProjectionBudget,
) -> Result<Option<Replacement>, AdapterError> {
    let file = input.source().source_ref().span().file();
    let Some(method) = retained(builder, index, file, proof.method, kind, budget)? else {
        return Ok(None);
    };
    let Some(owner) = retained(
        builder,
        index,
        file,
        proof.owner,
        EntityKind::Struct,
        budget,
    )?
    else {
        return Ok(None);
    };
    let Some(method_fact) = facts.get(&proof.method) else {
        return Ok(None);
    };
    let Some(owner_fact) = facts.get(&proof.owner) else {
        return Ok(None);
    };
    let Some(definition) =
        definition_matches(builder, index, input, facts, &method, proof.definition)
    else {
        return Ok(None);
    };
    if definition_matches(builder, index, input, facts, &owner, proof.owner_definition).is_none()
        || method.draft.source != source_for_span(input, method_fact.span())
        || owner.draft.source != source_for_span(input, owner_fact.span())
        || method.draft.parent_declaration
            != if kind == EntityKind::Field {
                Some(proof.owner)
            } else {
                proof.lexical_module
            }
        || owner.draft.parent_declaration != proof.lexical_module
        || (kind == EntityKind::Method && method.draft.scope_identity.is_none())
        || method.symbol == owner.symbol
        || index.contains.contains(&method.symbol)
    {
        return Ok(None);
    }
    let old_parent = if let Some(module) = proof.lexical_module {
        let Some(module) = retained(builder, index, file, module, EntityKind::Module, budget)?
        else {
            return Ok(None);
        };
        ContainerRef::Entity(module.symbol)
    } else {
        ContainerRef::File(file)
    };
    let member_parent = if kind == EntityKind::Field {
        ContainerRef::Entity(owner.symbol)
    } else {
        old_parent
    };
    if method.entity.container != Some(member_parent) || owner.entity.container != Some(old_parent)
    {
        return Ok(None);
    }
    let Some(declares_index) = index.declares.get(&method.symbol).copied().flatten() else {
        return Ok(None);
    };
    let Some(IrRecord::Relation(declares)) = builder.records.get(declares_index) else {
        return Ok(None);
    };
    let old_subject = match member_parent {
        ContainerRef::File(file) => RelationEndpoint::File(file),
        ContainerRef::Entity(symbol) => RelationEndpoint::Entity(symbol),
        ContainerRef::Repository(_) => return Ok(None),
    };
    if declares.subject != old_subject
        || declares.repository != method.entity.repository
        || declares.generation != method.entity.generation
        || declares.provenance != method.entity.provenance
        || declares.evidence.source.as_ref() != Some(&method.draft.source)
        || declares.evidence_kind != EvidenceKind::Syntax
        || !declares.evidence.derivation.is_empty()
        || declares.confidence != confidence(EXACT_CONFIDENCE)?
        || derive_relation_record_id(declares).ok() != Some(declares.id)
    {
        return Ok(None);
    }

    budget.slots(8)?;
    budget.charge(method.entity.canonical_name.len())?;
    budget.charge(method.entity.display_name.len())?;
    budget.charge(method.entity.qualified_name.len())?;
    let mut entity = method.entity.clone();
    entity.container = Some(ContainerRef::Entity(owner.symbol));
    let mut claim = method.claim;
    // The old discriminator identifies the written impl independently of where
    // its uniquely proved Struct happens to be declared in this source frame.
    claim.container = entity.container;
    if claim.derived_symbol() != method.symbol {
        return Err(provider_failure("project-rust-owner-identity"));
    }
    let envelope = new_symbol_identity_claim_envelope(
        &claim,
        entity.generation,
        entity.provenance,
        method.draft.source.clone(),
    )
    .map_err(|_| provider_failure("project-rust-owner-envelope"))?;
    let mut declares = declares.clone();
    declares.subject = RelationEndpoint::Entity(owner.symbol);
    declares.id = derive_relation_record_id(&declares)
        .map_err(|_| provider_failure("project-rust-owner-declares"))?;
    let mut contains = declares.clone();
    contains.predicate = RelationPredicate::Contains;
    contains.evidence.derivation = vec![
        FactRef::Entity(owner.symbol),
        FactRef::Entity(method.symbol),
    ];
    // The SDK publishes sorted evidence, so hash that same canonical order.
    contains.evidence.derivation.sort_unstable();
    contains.id = derive_relation_record_id(&contains)
        .map_err(|_| provider_failure("project-rust-owner-contains"))?;
    Ok(Some(Replacement {
        method_span: method.draft.span,
        entity_index: method.entity_index,
        entity,
        claim_index: method.claim_index,
        claim: envelope,
        declares_index,
        declares,
        contains,
        definition: DefinitionOwner {
            source: definition,
            method: method.symbol,
            owner: owner.symbol,
        },
    }))
}

struct ProjectionBudget {
    bytes: usize,
    limit: usize,
}

impl ProjectionBudget {
    fn charge(&mut self, bytes: usize) -> Result<(), AdapterError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(SinkError::AccountingOverflow)?;
        if self.bytes > self.limit {
            return Err(rootlight_adapter_sdk::ReportError::ResourceLimit {
                resource: rootlight_adapter_sdk::ResourceKind::ReportedMemoryBytes,
                observed: self.bytes,
                limit: self.limit,
            }
            .into());
        }
        Ok(())
    }

    fn slots(&mut self, count: usize) -> Result<(), AdapterError> {
        // This conservative admission model bounds the new indices and staged
        // replacements; it is not a measurement of allocator or process peaks.
        self.charge(
            count
                .checked_mul(512)
                .ok_or(SinkError::AccountingOverflow)?,
        )
    }
}

fn optional_capacity_refusal(error: &AdapterError) -> bool {
    matches!(
        error,
        AdapterError::InvalidReport(rootlight_adapter_sdk::ReportError::ResourceLimit {
            resource: rootlight_adapter_sdk::ResourceKind::ReportedMemoryBytes,
            ..
        }) | AdapterError::Sink(SinkError::AllocationFailed)
    )
}

pub(super) fn reconcile(
    builder: &mut ProjectFactsBuilder<'_, '_, '_>,
) -> Result<DefinitionOwners, AdapterError> {
    let mut owners = DefinitionOwners::new();
    if !builder.declarations.iter().any(|draft| {
        matches!(draft.kind, EntityKind::Method | EntityKind::Field) && !draft.synthetic
    }) {
        return Ok(owners);
    }
    let mut gaps = BTreeSet::new();
    let eligible_files = builder
        .parsed
        .iter()
        .filter(|parsed| {
            parsed.input.encoding().as_str() == "utf-8"
                && parsed.input.origins().is_empty()
                && parsed.parse_status == CoverageStatus::Complete
        })
        .map(|parsed| parsed.input.source().source_ref().span().file())
        .collect::<BTreeSet<_>>();
    // A declined optional byte or memory profile may return no candidates.
    // Existing authored impl methods still need unknown ownership coverage.
    for (position, draft) in builder.declarations.iter().enumerate() {
        check_periodically(position, builder.cancellation)?;
        if eligible_files.contains(&draft.file)
            && matches!(draft.kind, EntityKind::Method | EntityKind::Field)
            && !draft.synthetic
            && (draft.kind == EntityKind::Field || draft.scope_identity.is_some())
            && builder
                .symbol_by_declaration
                .contains_key(&(draft.file, draft.local_id))
        {
            gaps.insert(draft.span);
        }
    }
    let planned = (|| -> Result<Vec<Replacement>, AdapterError> {
        let mut budget = ProjectionBudget {
            bytes: 0,
            limit: builder.request.limits().max_reported_memory_bytes(),
        };
        let mut proposals = BTreeMap::<usize, Vec<(RustInherentOwnerProof, EntityKind)>>::new();
        let mut candidates = Candidates::default();
        for (parsed_index, parsed) in builder.parsed.iter().enumerate() {
            builder.cancellation.check()?;
            let input = parsed.input;
            let file = input.source().source_ref().span().file();
            if !eligible_files.contains(&file) {
                continue;
            }
            let plan = rust_inherent_owner_plan(
                &parsed.facts,
                input.source().source_ref(),
                input.source().bytes(),
                true,
                builder.request.limits().syntax_stream().max_records(),
                builder.request.limits().max_source_bytes(),
                builder.request.limits().max_syntax_nodes(),
                // This configured credit admits every representable cost when its
                // mathematical ceiling exceeds usize::MAX; actual costs stay checked.
                builder
                    .request
                    .limits()
                    .max_source_bytes()
                    .saturating_mul(builder.request.limits().max_syntax_nodes()),
                builder.request.limits().ir().max_string_bytes,
                budget
                    .limit
                    .checked_sub(budget.bytes)
                    .ok_or(SinkError::AccountingOverflow)?,
                builder.cancellation,
            )?;
            if plan.source != *input.source().source_ref() {
                return Err(provider_failure("project-rust-owner-source"));
            }
            let mut members = Vec::new();
            members
                .try_reserve_exact(
                    plan.proofs
                        .len()
                        .checked_add(plan.fields.len())
                        .ok_or(SinkError::AccountingOverflow)?,
                )
                .map_err(|_| SinkError::AllocationFailed)?;
            members.extend(
                plan.proofs
                    .into_iter()
                    .map(|proof| (proof, EntityKind::Method)),
            );
            members.extend(plan.fields.into_iter().map(|field| {
                (
                    RustInherentOwnerProof {
                        method: field.field,
                        definition: field.definition,
                        owner: field.owner,
                        owner_definition: field.owner_definition,
                        lexical_module: field.lexical_module,
                    },
                    EntityKind::Field,
                )
            }));
            if members.is_empty() {
                continue;
            }
            budget.slots(
                members
                    .len()
                    .checked_mul(32)
                    .ok_or(SinkError::AccountingOverflow)?,
            )?;
            for (proof, _) in &members {
                builder.cancellation.check()?;
                for local in [Some(proof.method), Some(proof.owner), proof.lexical_module]
                    .into_iter()
                    .flatten()
                {
                    candidates.declarations.insert((file, local));
                    if let Some(symbol) = builder.symbol_by_declaration.get(&(file, local)) {
                        candidates.symbols.insert(*symbol);
                    }
                }
            }
            proposals.insert(parsed_index, members);
        }
        // The optional proof must not create a project-sized parallel index when
        // every file declined it. Candidate keys bound every retained index.
        if proposals.is_empty() {
            return Ok(Vec::new());
        }
        let index = RecordIndex::new(builder, &candidates)?;
        let mut replacements = Vec::new();
        let mut changed_symbols = BTreeSet::new();
        let mut definition_spans = BTreeSet::new();
        for (parsed_index, proofs) in proposals {
            let parsed = builder
                .parsed
                .get(parsed_index)
                .ok_or_else(|| provider_failure("project-rust-owner-input"))?;
            budget.slots(parsed.facts.len())?;
            let facts = parsed
                .facts
                .iter()
                .map(|fact| (fact.local_id(), fact))
                .collect::<BTreeMap<_, _>>();
            for (position, (proof, kind)) in proofs.iter().enumerate() {
                check_periodically(position, builder.cancellation)?;
                if let Some(replacement) = prepare(
                    builder,
                    &index,
                    parsed.input,
                    &facts,
                    proof,
                    *kind,
                    &mut budget,
                )? {
                    if !changed_symbols.insert(replacement.definition.method)
                        || !definition_spans.insert(replacement.definition.source.span())
                    {
                        return Err(provider_failure("project-rust-owner-duplicate"));
                    }
                    replacements
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    replacements.push(replacement);
                }
            }
        }
        Ok(replacements)
    })();
    let replacements = match planned {
        Ok(replacements) => replacements,
        // No records have changed. Optional projection allocation/admission must
        // preserve the legacy project rather than force a new global fallback.
        Err(error) if optional_capacity_refusal(&error) => Vec::new(),
        Err(error) => return Err(error),
    };
    builder.cancellation.check()?;
    builder
        .records
        .try_reserve(
            replacements
                .len()
                .checked_add(gaps.len())
                .ok_or(SinkError::AccountingOverflow)?,
        )
        .map_err(|_| SinkError::AllocationFailed)?;
    for replacement in replacements {
        builder.cancellation.check()?;
        *builder
            .records
            .get_mut(replacement.entity_index)
            .ok_or_else(|| provider_failure("project-rust-owner-record"))? =
            IrRecord::Entity(replacement.entity);
        *builder
            .records
            .get_mut(replacement.claim_index)
            .ok_or_else(|| provider_failure("project-rust-owner-record"))? =
            IrRecord::Extension(replacement.claim);
        *builder
            .records
            .get_mut(replacement.declares_index)
            .ok_or_else(|| provider_failure("project-rust-owner-record"))? =
            IrRecord::Relation(replacement.declares);
        let file = replacement.definition.source.span().file();
        builder
            .records
            .push(IrRecord::Relation(replacement.contains));
        builder
            .state_mut_by_file(file)?
            .increment(FactDomain::Relations)?;
        gaps.remove(&replacement.method_span);
        owners.insert(replacement.definition.source.span(), replacement.definition);
    }
    for span in gaps {
        builder.cancellation.check()?;
        let input = builder.input_for_file(span.file())?.clone();
        builder.push_source_gap(
            &input,
            span,
            FactDomain::Relations,
            "rust-inherent-owner-proof-unavailable",
        )?;
    }
    Ok(owners)
}

pub(super) fn definition_enclosing(
    owners: &DefinitionOwners,
    draft: &OccurrenceDraft,
    target: &OccurrenceTarget,
) -> Option<SymbolId> {
    let proof = owners.get(&draft.source.span())?;
    (draft.role == OccurrenceRole::Definition
        && draft.source == proof.source
        && matches!(target, OccurrenceTarget::Resolved { symbol } if *symbol == proof.method))
    .then_some(proof.owner)
}
