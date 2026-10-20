//! Join provider-certified Prolog goals with exact current-file predicate IDs.
//! The provider owns the complete sentence census; context signatures carry
//! source witnesses and never donate callable identity or generic resolution.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind, predicate_head};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "prolog.goal_context.signature" | "prolog.goal_target.signature"
        )
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    metadata(fact)
        || fact.kind() == SyntaxFactKind::Occurrence
            && fact.syntax_kind().as_str() == "prolog.direct_goal.reference"
}

pub(super) fn call_ends(
    facts: &[SyntaxFact],
    cancel: &Cancellation,
) -> Result<HashMap<(rootlight_ids::FileId, u64), ()>, AdapterError> {
    let mut ends = HashMap::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Signature
            && fact.syntax_kind().as_str() == "prolog.goal_target.signature"
        {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct CallTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl CallTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut all = HashMap::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut calls = HashMap::new();
        let mut markers = HashMap::new();
        let mut goals = HashMap::new();
        for map in [
            &mut all,
            &mut declarations,
            &mut definitions,
            &mut calls,
            &mut markers,
            &mut goals,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        let mut root = None;
        let mut root_count = 0;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "prolog.source_file"
                && physical(fact.span())? == source.span()
            {
                root_count += 1;
                root = Some(fact.local_id());
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Declaration, "prolog.predicate.declaration") => {
                    unique(&mut declarations, physical(fact.span())?.start_byte(), fact)
                }
                (SyntaxFactKind::Occurrence, "prolog.predicate.definition") => {
                    if let Some(parent) = fact.parent() {
                        unique(&mut definitions, parent, fact);
                    }
                }
                (SyntaxFactKind::Occurrence, "prolog.direct_goal.reference") => {
                    unique(&mut calls, physical(fact.span())?.end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "prolog.goal_target.signature") => {
                    unique(&mut markers, physical(fact.span())?.end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "prolog.goal_context.signature") => {
                    unique(&mut goals, physical(fact.span())?.start_byte(), fact)
                }
                _ => {}
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        if root_count != 1 || recovered {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let witness = physical(marker.span())?;
            let Some(target_fact) = declarations.get(&witness.start_byte()).copied().flatten()
            else {
                continue;
            };
            let Some(call) = calls.get(&witness.end_byte()).copied().flatten() else {
                continue;
            };
            let span = physical(call.span())?;
            let Some(goal) = goals.get(&span.start_byte()).copied().flatten() else {
                continue;
            };
            let Some(owner_fact) = call.parent().and_then(|id| all.get(&id).copied().flatten())
            else {
                continue;
            };
            let Some(target) = entities.get(&target_fact.local_id()) else {
                continue;
            };
            let Some(owner) = entities.get(&owner_fact.local_id()) else {
                continue;
            };
            let Some(definition) = definitions.get(&target_fact.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(owner_definition) = definitions.get(&owner_fact.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(target_source) = target.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let Some((target_name, target_arity)) = predicate_head(text(definition.span())?) else {
                continue;
            };
            let Some((goal_name, goal_arity)) = predicate_head(text(goal.span())?) else {
                continue;
            };
            let Some((canonical_name, canonical_arity)) =
                target.record.canonical_name.rsplit_once('/')
            else {
                continue;
            };
            let goal_span = physical(goal.span())?;
            if marker.parent() != root
                || marker.depth() != 1
                || target_fact.parent() != root
                || owner_fact.parent() != root
                || target_fact.depth() != 1
                || owner_fact.depth() != 1
                || owner_fact.kind() != SyntaxFactKind::Declaration
                || owner_fact.syntax_kind().as_str() != "prolog.predicate.declaration"
                || goal.parent() != Some(owner_fact.local_id())
                || goal.depth() != 2
                || call.depth() != 2
                || target.record.kind != EntityKind::Function
                || owner.record.kind != EntityKind::Function
                || target.record.language != "prolog"
                || owner.record.language != "prolog"
                || target.record.repository != source.repository()
                || owner.record.repository != source.repository()
                || target.record.generation != source.generation()
                || owner.record.generation != source.generation()
                || !same_source(target_source, source)
                || !same_source(owner_source, source)
                || target.direct_parent != ContainerRef::File(source.span().file())
                || owner.direct_parent != ContainerRef::File(source.span().file())
                || target.record.container != Some(target.direct_parent)
                || owner.record.container != Some(owner.direct_parent)
                || target_source.span() != physical(target_fact.span())?
                || owner_source.span() != physical(owner_fact.span())?
                || target.definition_local_id != Some(definition.local_id())
                || owner.definition_local_id != Some(owner_definition.local_id())
                || physical(definition.span())?.start_byte() != target_source.span().start_byte()
                || physical(definition.span())?.end_byte() > target_source.span().end_byte()
                || physical(owner_definition.span())?.start_byte()
                    != owner_source.span().start_byte()
                || physical(owner_definition.span())?.end_byte() > span.start_byte()
                || target_name != goal_name
                || text(call.span())? != goal_name
                || !goal_name.is_ascii()
                || target_arity != goal_arity
                || canonical_name != target_name
                || canonical_arity.parse::<usize>().ok() != Some(target_arity)
                || goal_span.start_byte() != span.start_byte()
                || goal_span.end_byte() < span.end_byte()
                || goal_span.end_byte() > owner_source.span().end_byte()
                || target_source.span().start_byte() > span.start_byte()
                || span.start_byte() < owner_source.span().start_byte()
                || target.record.provenance != owner.record.provenance
                || span.file() != source.span().file()
            {
                continue;
            }
            bindings.insert(
                call.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    span,
                    spelling: content_hash(text(call.span())?.as_bytes()),
                    provenance: owner.record.provenance,
                },
            );
        }
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        physical: SourceSpan,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "prolog.direct_goal.reference"
            || site.syntax_kind != "prolog.direct_goal.reference"
            || site.role != OccurrenceRole::CallSite
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || !same_source(&site.source, &self.source)
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.span == physical
                    && binding.span == site.source.span()
                    && binding.spelling == site.syntactic_text_hash
                    && site.enclosing == Some(binding.owner)
                    && binding.provenance == site.provenance
            })
            .map(|binding| binding.target))
    }
}

pub(super) fn call_relation(
    site: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.language == "prolog"
            && owner.record.kind == EntityKind::Function
            && owner
                .record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| same_source(source, &site.source))
    }) else {
        return Ok(None);
    };
    if site.syntax_kind != "prolog.direct_goal.reference"
        || site.role != OccurrenceRole::CallSite
        || site.enclosing != Some(owner.record.id)
        || site.target != (OccurrenceTarget::Resolved { symbol: target })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(site, target)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(site.id),
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("prolog-call-relation-identity"))?;
    Ok(Some(relation))
}
