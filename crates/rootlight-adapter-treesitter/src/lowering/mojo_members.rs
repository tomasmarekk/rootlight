//! Join native self-member endpoints to exact current declarations and formals.
//! A name validates an already selected source endpoint; it never selects a
//! symbol. Missing, duplicated or foreign witnesses preserve the binding gap.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

#[cfg(test)]
mod tests;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "mojo.self_field_read.reference" | "mojo.self_method_call.call"
        )
}

pub(super) fn call(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "mojo.self_method_call.call"
}

pub(super) struct MemberTargets<'a> {
    facts: &'a HashMap<u64, &'a SyntaxFact>,
    entities: &'a HashMap<u64, MaterializedEntity>,
    definitions: HashMap<u64, Option<(u64, &'a MaterializedEntity)>>,
    symbols: HashMap<SymbolId, Option<(u64, &'a MaterializedEntity)>>,
    markers: HashMap<(u64, &'static str), Option<&'a SyntaxFact>>,
    raw_declarations: HashMap<(SourceSpan, &'a str), usize>,
    raw_definitions: HashMap<SourceSpan, usize>,
    source: &'a SourceRef,
    provenance: FactId,
    root_valid: bool,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

impl<'a> MemberTargets<'a> {
    pub(super) fn new(
        facts: &'a HashMap<u64, &'a SyntaxFact>,
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        provenance: FactId,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut result = Self {
            facts,
            entities,
            definitions: HashMap::new(),
            symbols: HashMap::new(),
            markers: HashMap::new(),
            raw_declarations: HashMap::new(),
            raw_definitions: HashMap::new(),
            source,
            provenance,
            root_valid: false,
        };
        result
            .definitions
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .symbols
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .raw_declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .raw_definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut roots = 0_usize;
        let mut valid = true;
        for fact in facts.values().copied() {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Root {
                roots = roots.saturating_add(1);
                let span = physical(fact.span())?;
                valid &= fact.syntax_kind().as_str() == "mojo.source_file.root"
                    && fact.parent().is_none()
                    && source.span().start_byte() == 0
                    && span.start_byte() == source.span().start_byte()
                    && span.file() == source.span().file()
                    && span.end_byte() == source.span().end_byte();
            }
            // Equal source captures can share one materialized entity. Keep their
            // raw cardinality so duplicate evidence cannot certify that entity.
            if fact.kind() == SyntaxFactKind::Declaration {
                let key = (physical(fact.span())?, fact.syntax_kind().as_str());
                let count = result.raw_declarations.entry(key).or_default();
                *count = count.saturating_add(1);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "mojo.name.definition"
            {
                let count = result
                    .raw_definitions
                    .entry(physical(fact.span())?)
                    .or_default();
                *count = count.saturating_add(1);
            }
            let label = match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Signature, "mojo.self_field_target.signature") => "field",
                (SyntaxFactKind::Signature, "mojo.self_method_target.signature") => "method",
                (SyntaxFactKind::Signature, "mojo.self_formal_target.signature") => "self",
                _ => continue,
            };
            let span = physical(fact.span())?;
            let valid = span.start_byte() < span.end_byte()
                && span.file() == source.span().file()
                && span.start_byte() >= source.span().start_byte()
                && span.end_byte() <= source.span().end_byte();
            result
                .markers
                .entry((span.end_byte(), label))
                .and_modify(|value| *value = None)
                .or_insert(valid.then_some(fact));
        }
        result.root_valid = roots == 1 && valid;
        for (local, entity) in entities {
            cancel.check()?;
            result
                .symbols
                .entry(entity.record.id)
                .and_modify(|value| *value = None)
                .or_insert(Some((*local, entity)));
            let Some(definition) = entity
                .definition_local_id
                .and_then(|id| facts.get(&id).copied())
            else {
                continue;
            };
            let span = physical(definition.span())?;
            result
                .definitions
                .entry(span.start_byte())
                .and_modify(|value| *value = None)
                .or_insert(Some((*local, entity)));
        }
        Ok(result)
    }

    fn parent_entity(
        &self,
        fact: &SyntaxFact,
        cancel: &Cancellation,
    ) -> Result<Option<(u64, &'a MaterializedEntity)>, AdapterError> {
        let mut parent = fact.parent();
        let mut steps = 0_usize;
        while let Some(local) = parent {
            cancel.check()?;
            if steps >= self.facts.len() {
                return Ok(None);
            }
            steps = steps
                .checked_add(1)
                .ok_or_else(|| provider_failure("mojo-member-parent-depth"))?;
            if let Some(entity) = self.entities.get(&local) {
                return Ok(Some((local, entity)));
            }
            let Some(fact) = self.facts.get(&local) else {
                return Ok(None);
            };
            parent = fact.parent();
        }
        Ok(None)
    }

    fn valid_entity<'text>(
        &self,
        selected: (u64, &MaterializedEntity),
        kind: EntityKind,
        label: &str,
        text: &mut impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        physical: &mut impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let (local, entity) = selected;
        cancel.check()?;
        let Some(source) = entity.record.evidence.source.as_ref() else {
            return Ok(false);
        };
        let Some(declaration) = self.facts.get(&local).copied() else {
            return Ok(false);
        };
        let Some(definition) = entity
            .definition_local_id
            .and_then(|id| self.facts.get(&id).copied())
        else {
            return Ok(false);
        };
        let definition_span = physical(definition.span())?;
        if entity.record.kind != kind
            || entity.record.language != "mojo"
            || entity.record.repository != self.source.repository()
            || entity.record.generation != self.source.generation()
            || entity.record.provenance != self.provenance
            || entity.record.container != Some(entity.direct_parent)
            || !same_source(source, self.source)
            || source.span().file() != self.source.span().file()
            || source.span().start_byte() < self.source.span().start_byte()
            || source.span().end_byte() > self.source.span().end_byte()
            || entity.identity_claim.symbol != entity.record.id
            || entity.identity_claim.derived_symbol() != entity.record.id
            || entity.identity_claim.repository != entity.record.repository
            || entity.identity_claim.language != entity.record.language
            || entity.identity_claim.kind != entity.record.kind
            || entity.identity_claim.container != entity.record.container
            || entity.identity_claim.declared_identity != entity.record.canonical_name
            || declaration.kind() != SyntaxFactKind::Declaration
            || declaration.syntax_kind().as_str() != label
            || physical(declaration.span())? != source.span()
            || self.raw_declarations.get(&(source.span(), label)).copied() != Some(1)
            || self.raw_definitions.get(&definition_span).copied() != Some(1)
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.syntax_kind().as_str() != "mojo.name.definition"
            || definition.parent() != Some(local)
            || definition_span.file() != self.source.span().file()
            || !super::span_contains(source.span(), definition_span)
            || text(definition.span())? != entity.record.canonical_name
        {
            return Ok(false);
        }
        let parent = self.parent_entity(declaration, cancel)?;
        match (entity.direct_parent, parent) {
            (ContainerRef::Entity(expected), Some((_, actual))) if expected == actual.record.id => {
            }
            (ContainerRef::File(file), None) if file == self.source.span().file() => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub(super) fn resolve<'text>(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !self.root_valid
            || !required(fact)
            || self.facts.get(&fact.local_id()).copied() != Some(fact)
        {
            return Ok(None);
        }
        let Some(owner) = owner else { return Ok(None) };
        let Some((owner_local, selected_owner)) =
            self.symbols.get(&owner.record.id).copied().flatten()
        else {
            return Ok(None);
        };
        if selected_owner.record != owner.record
            || !self.valid_entity(
                (owner_local, owner),
                EntityKind::Method,
                "mojo.method.declaration",
                &mut text,
                &mut physical,
                cancel,
            )?
        {
            return Ok(None);
        }
        let ContainerRef::Entity(struct_id) = owner.direct_parent else {
            return Ok(None);
        };
        let Some((struct_local, structure)) = self.symbols.get(&struct_id).copied().flatten()
        else {
            return Ok(None);
        };
        if !self.valid_entity(
            (struct_local, structure),
            EntityKind::Struct,
            "mojo.struct.declaration",
            &mut text,
            &mut physical,
            cancel,
        )? {
            return Ok(None);
        }
        let span = physical(fact.span())?;
        let written = text(fact.span())?;
        let expected_role = if call(fact) {
            OccurrenceRole::CallSite
        } else {
            OccurrenceRole::Read
        };
        let hash = rootlight_ids::content_hash(written.as_bytes());
        if span.file() != self.source.span().file()
            || span.start_byte() < self.source.span().start_byte()
            || span.end_byte() > self.source.span().end_byte()
            || occurrence.role != expected_role
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || occurrence.enclosing != Some(owner.record.id)
            || occurrence.target != (OccurrenceTarget::Unresolved { text_hash: hash })
            || occurrence.syntactic_text_hash != hash
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != span
            || !same_source(&occurrence.source, self.source)
            || occurrence.evidence.source.as_ref() != Some(&occurrence.source)
            || occurrence.provenance != self.provenance
            || self
                .parent_entity(fact, cancel)?
                .map(|(_, entity)| entity.record.id)
                != Some(owner.record.id)
        {
            return Ok(None);
        }
        let Some(member_marker) = self
            .markers
            .get(&(span.end_byte(), if call(fact) { "method" } else { "field" }))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let Some(self_marker) = self
            .markers
            .get(&(span.end_byte(), "self"))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        if self
            .parent_entity(member_marker, cancel)?
            .map(|(_, entity)| entity.record.id)
            != Some(struct_id)
            || self
                .parent_entity(self_marker, cancel)?
                .map(|(_, entity)| entity.record.id)
                != Some(owner.record.id)
        {
            return Ok(None);
        }
        let target_start = physical(member_marker.span())?.start_byte();
        let self_start = physical(self_marker.span())?.start_byte();
        let Some((target_local, target)) = self.definitions.get(&target_start).copied().flatten()
        else {
            return Ok(None);
        };
        let Some((formal_local, formal)) = self.definitions.get(&self_start).copied().flatten()
        else {
            return Ok(None);
        };
        if !self.valid_entity(
            (formal_local, formal),
            EntityKind::Parameter,
            "mojo.parameter.declaration",
            &mut text,
            &mut physical,
            cancel,
        )? || formal.direct_parent != ContainerRef::Entity(owner.record.id)
            || formal.record.canonical_name != "self"
            || !self.valid_entity(
                (target_local, target),
                if call(fact) {
                    EntityKind::Method
                } else {
                    EntityKind::Field
                },
                if call(fact) {
                    "mojo.method.declaration"
                } else {
                    "mojo.field.declaration"
                },
                &mut text,
                &mut physical,
                cancel,
            )?
            || target.direct_parent != ContainerRef::Entity(struct_id)
            || target.record.canonical_name != written
        {
            return Ok(None);
        }
        let mut self_count = 0_usize;
        let mut member_count = 0_usize;
        let mut target_parameters = 0_usize;
        let mut target_self_valid = false;
        for (local, entity) in self.entities {
            cancel.check()?;
            if entity.direct_parent == ContainerRef::Entity(struct_id)
                && entity.record.canonical_name == target.record.canonical_name
            {
                member_count = member_count.saturating_add(1);
            }
            if entity.record.kind != EntityKind::Parameter {
                continue;
            }
            if entity.direct_parent == ContainerRef::Entity(owner.record.id)
                && entity.record.canonical_name == "self"
            {
                self_count = self_count.saturating_add(1);
            }
            if call(fact) && entity.direct_parent == ContainerRef::Entity(target.record.id) {
                target_parameters = target_parameters.saturating_add(1);
                target_self_valid |= entity.record.canonical_name == "self"
                    && self.valid_entity(
                        (*local, entity),
                        EntityKind::Parameter,
                        "mojo.parameter.declaration",
                        &mut text,
                        &mut physical,
                        cancel,
                    )?;
            }
        }
        if member_count != 1
            || self_count != 1
            || call(fact) && (target_parameters != 1 || !target_self_valid)
        {
            return Ok(None);
        }
        Ok(Some(target.record.id))
    }
}

pub(super) fn relation(
    occurrence: &OccurrenceRecord,
    owner: &MaterializedEntity,
    symbol: SymbolId,
    cancel: &Cancellation,
) -> Result<RelationRecord, AdapterError> {
    cancel.check()?;
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    if occurrence.role == OccurrenceRole::CallSite {
        relation.predicate = RelationPredicate::Calls;
        relation.subject = RelationEndpoint::Entity(owner.record.id);
        relation
            .evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        relation.evidence.derivation.extend([
            FactRef::Entity(owner.record.id),
            FactRef::Fact(occurrence.id),
            FactRef::Entity(symbol),
        ]);
        relation.evidence.derivation.sort_unstable();
        relation.evidence.derivation.dedup();
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| provider_failure("mojo-self-member-relation-identity"))?;
    }
    Ok(relation)
}
