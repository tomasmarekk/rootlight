//! Consume native same-file Hare call witnesses in physical source coordinates.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Struct, "hare.struct.declaration")
            | (EntityKind::Field, "hare.field.declaration")
            | (EntityKind::Function, "hare.function.declaration")
            | (EntityKind::Parameter, "hare.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "hare.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "hare.direct_file_function.reference" | "hare.unresolved_call.reference",
        ) => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Comment, "hare.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "hare.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "hare.file.module") => &[
            (
                FactDomain::Entities,
                "hare-alias-enum-global-embedded-field-and-attribute-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "hare-general-reads-writes-type-and-qualified-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "hare-complete-translation-unit-cross-file-import-and-runtime-binding-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "hare.unsupported.declaration") => &[
            (FactDomain::Entities, "hare-written-declaration-unavailable"),
            (
                FactDomain::Occurrences,
                "hare-written-definition-unavailable",
            ),
            (FactDomain::Relations, "hare-written-owner-unavailable"),
        ],
        (SyntaxFactKind::Signature, "hare.construct_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "hare-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "hare-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "hare-source-construct-semantics-unavailable",
            ),
        ],
        _ => &[],
    };
    rows.iter().copied()
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
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
        for map in [
            &mut all,
            &mut declarations,
            &mut definitions,
            &mut calls,
            &mut markers,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        let mut by_symbol = HashMap::new();
        by_symbol
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            unique(&mut by_symbol, entity.record.id, entity);
        }
        let mut root = None;
        let mut roots = 0_usize;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "hare.source_file.root"
                && physical(fact.span())? == source.span()
            {
                roots = roots.saturating_add(1);
                root = Some(fact.local_id());
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Declaration, "hare.function.declaration") => {
                    unique(&mut declarations, physical(fact.span())?.start_byte(), fact)
                }
                (SyntaxFactKind::Occurrence, "hare.name.definition") => {
                    if let Some(parent) = fact.parent() {
                        unique(&mut definitions, parent, fact);
                    }
                }
                (SyntaxFactKind::Occurrence, "hare.direct_file_function.reference") => {
                    unique(&mut calls, physical(fact.span())?.end_byte(), fact)
                }
                (SyntaxFactKind::Signature, "hare.call_target.signature") => {
                    unique(&mut markers, physical(fact.span())?.end_byte(), fact)
                }
                _ => {}
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        if roots != 1 || recovered {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let mut ancestor = marker.parent();
            let mut remaining = facts.len();
            let mut rooted = false;
            while let Some(local) = ancestor {
                cancel.check()?;
                let Some(next) = remaining.checked_sub(1) else {
                    break;
                };
                remaining = next;
                if Some(local) == root {
                    rooted = true;
                    break;
                }
                let Some(parent) = all.get(&local).copied().flatten() else {
                    break;
                };
                if !matches!(
                    parent.kind(),
                    SyntaxFactKind::Module | SyntaxFactKind::Scope | SyntaxFactKind::Declaration
                ) {
                    break;
                }
                ancestor = parent.parent();
            }
            if !rooted {
                continue;
            }
            let witness = physical(marker.span())?;
            let Some(declaration) = declarations.get(&witness.start_byte()).copied().flatten()
            else {
                continue;
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(call) = calls.get(&witness.end_byte()).copied().flatten() else {
                continue;
            };
            let Some(target) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(target_source) = target.record.evidence.source.as_ref() else {
                continue;
            };
            let mut parent = call.parent();
            let mut owner = None;
            let mut remaining = facts.len();
            while let Some(local) = parent {
                cancel.check()?;
                let Some(next) = remaining.checked_sub(1) else {
                    break;
                };
                remaining = next;
                let Some(fact) = all.get(&local).copied().flatten() else {
                    break;
                };
                if fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "hare.function.declaration"
                {
                    owner = entities.get(&local);
                    break;
                }
                if fact.kind() == SyntaxFactKind::Scope
                    && fact.syntax_kind().as_str() == "hare.function.scope"
                    && let Some(declaration) = declarations
                        .get(&physical(fact.span())?.start_byte())
                        .copied()
                        .flatten()
                    && physical(declaration.span())? == physical(fact.span())?
                {
                    owner = entities.get(&declaration.local_id());
                    break;
                }
                parent = fact.parent();
            }
            let Some(owner) = owner else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(ContainerRef::Entity(module_id)) = owner.record.container else {
                continue;
            };
            let Some(module) = by_symbol.get(&module_id).copied().flatten() else {
                continue;
            };
            let Some(module_source) = module.record.evidence.source.as_ref() else {
                continue;
            };
            let span = physical(call.span())?;
            let name = text(definition.span())?;
            if module.record.language != "hare"
                || module.record.kind != EntityKind::Module
                || module_source.span() != source.span()
                || !same_source(module_source, source)
                || target.record.language != "hare"
                || target.record.kind != EntityKind::Function
                || owner.record.language != "hare"
                || owner.record.kind != EntityKind::Function
                || !same_source(target_source, source)
                || !same_source(owner_source, source)
                || target.record.repository != source.repository()
                || target.record.generation != source.generation()
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || target.record.container != Some(ContainerRef::Entity(module_id))
                || target.direct_parent != ContainerRef::Entity(module_id)
                || owner.direct_parent != ContainerRef::Entity(module_id)
                || target_source.span() != physical(declaration.span())?
                || target.definition_local_id != Some(definition.local_id())
                || target.record.canonical_name != name
                || text(call.span())? != name
                || physical(definition.span())?.start_byte() < target_source.span().start_byte()
                || physical(definition.span())?.end_byte() > target_source.span().end_byte()
                || span.start_byte() < owner_source.span().start_byte()
                || span.end_byte() > owner_source.span().end_byte()
                || witness.start_byte() != target_source.span().start_byte()
                || witness.end_byte() != span.end_byte()
                || span.file() != source.span().file()
                || target.record.provenance != owner.record.provenance
            {
                continue;
            }
            bindings.insert(
                call.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    span,
                    spelling: content_hash(name.as_bytes()),
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
        physical_span: SourceSpan,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "hare.direct_file_function.reference"
            || site.syntax_kind != "hare.direct_file_function.reference"
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
                binding.span == physical_span
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
        owner.record.language == "hare"
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
    if site.syntax_kind != "hare.direct_file_function.reference"
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
        .map_err(|_| provider_failure("hare-direct-call-identity"))?;
    Ok(Some(relation))
}
