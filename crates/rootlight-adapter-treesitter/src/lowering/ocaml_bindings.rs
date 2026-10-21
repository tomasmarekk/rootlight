//! Join native OCaml binding certificates to exact current physical definitions.
//! Raw own Declaration/Definition cardinality is checked before materialized
//! selection, so duplicate or missing native witnesses cannot acquire a target.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "ocaml.match_formal.reference" | "ocaml.direct_file_call.call"
        )
}

pub(super) fn call(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "ocaml.direct_file_call.call"
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "ocaml.written_bindings_file.signature"
                | "ocaml.match_formal_target.signature"
                | "ocaml.direct_file_target.signature"
        )
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn same_source(reference: &SourceRef, source: &SourceRef) -> bool {
    *reference == super::source_for_span(source, reference.span())
}

fn count<K: std::hash::Hash + Eq>(map: &mut HashMap<K, usize>, key: K) {
    map.entry(key).and_modify(|value| *value = 2).or_insert(1);
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
    call: bool,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    /// Validate the real root/module and every raw current own witness before joining.
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut has_certificate = false;
        for fact in facts {
            cancel.check()?;
            has_certificate |= required(fact);
        }
        if !has_certificate {
            return Ok(Self {
                source: source.clone(),
                bindings: HashMap::new(),
            });
        }
        let mut all = HashMap::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut raw_declarations = HashMap::new();
        let mut raw_definitions = HashMap::new();
        let mut markers = HashMap::new();
        let mut sites = HashMap::new();
        for map in [
            &mut all,
            &mut declarations,
            &mut definitions,
            &mut markers,
            &mut sites,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        raw_declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        raw_definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut by_start = HashMap::new();
        let mut owners = HashMap::new();
        let mut by_symbol = HashMap::new();
        by_start
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        by_symbol
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut by_symbol, entity.record.id, (*local, entity));
            if let Some(reference) = entity.record.evidence.source.as_ref() {
                let tag = match entity.record.kind {
                    EntityKind::Function => Some(0_u8),
                    EntityKind::Parameter => Some(1_u8),
                    _ => None,
                };
                if let Some(tag) = tag {
                    unique(
                        &mut by_start,
                        (tag, reference.span().start_byte()),
                        (*local, entity),
                    );
                }
                if entity.record.kind == EntityKind::Function {
                    unique(&mut owners, reference.span(), (*local, entity));
                }
            }
        }
        let mut roots = 0usize;
        let mut root = None;
        let mut file_markers = 0usize;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            let span = physical(fact.span())?;
            if fact.kind() == SyntaxFactKind::Root {
                if fact.syntax_kind().as_str() != "ocaml.file.root"
                    || fact.parent().is_some()
                    || span != source.span()
                {
                    recovered = true;
                } else {
                    roots = roots.saturating_add(1);
                    root = Some(fact.local_id());
                }
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "ocaml.written_bindings_file.signature"
            {
                file_markers = file_markers.saturating_add(1);
                recovered |= span != source.span();
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && matches!(
                    fact.syntax_kind().as_str(),
                    "ocaml.function.declaration" | "ocaml.parameter.declaration"
                )
            {
                count(&mut raw_declarations, (fact.syntax_kind().as_str(), span));
                unique(&mut declarations, fact.local_id(), fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "ocaml.name.definition"
            {
                count(&mut raw_definitions, span);
                if let Some(parent) = fact.parent() {
                    unique(&mut definitions, parent, fact);
                }
            }
            if required(fact) {
                unique(&mut sites, span.end_byte(), fact);
            }
            if metadata(fact)
                && fact.syntax_kind().as_str() != "ocaml.written_bindings_file.signature"
            {
                unique(&mut markers, span.end_byte(), fact);
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        if roots != 1 || file_markers != 1 || recovered {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        let rooted = |fact: &SyntaxFact| -> Result<bool, AdapterError> {
            let mut ancestor = fact.parent();
            let mut remaining = facts.len();
            while let Some(local) = ancestor {
                cancel.check()?;
                let Some(next) = remaining.checked_sub(1) else {
                    return Ok(false);
                };
                remaining = next;
                if Some(local) == root {
                    return Ok(true);
                }
                let Some(parent) = all.get(&local).copied().flatten() else {
                    return Ok(false);
                };
                if !matches!(
                    parent.kind(),
                    SyntaxFactKind::Module | SyntaxFactKind::Scope | SyntaxFactKind::Declaration
                ) {
                    return Ok(false);
                }
                ancestor = parent.parent();
            }
            Ok(false)
        };
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            if !rooted(marker)? {
                continue;
            }
            let witness = physical(marker.span())?;
            let Some(site) = sites.get(&witness.end_byte()).copied().flatten() else {
                continue;
            };
            let is_call = call(site);
            if (is_call && marker.syntax_kind().as_str() != "ocaml.direct_file_target.signature")
                || (!is_call
                    && marker.syntax_kind().as_str() != "ocaml.match_formal_target.signature")
            {
                continue;
            }
            let tag = if is_call { 0_u8 } else { 1_u8 };
            let Some((target_local, target)) = by_start
                .get(&(tag, witness.start_byte()))
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(declaration) = declarations.get(&target_local).copied().flatten() else {
                continue;
            };
            let Some(definition) = definitions.get(&target_local).copied().flatten() else {
                continue;
            };
            let target_span = physical(declaration.span())?;
            let definition_span = physical(definition.span())?;
            if raw_declarations.get(&(declaration.syntax_kind().as_str(), target_span)) != Some(&1)
                || raw_definitions.get(&definition_span) != Some(&1)
                || target.definition_local_id != Some(definition.local_id())
                || definition.parent() != Some(target_local)
                || text(definition.span())? != target.record.canonical_name
            {
                continue;
            }
            let mut ancestor = site.parent();
            let mut remaining = facts.len();
            let mut owner = None;
            while let Some(local) = ancestor {
                cancel.check()?;
                let Some(next) = remaining.checked_sub(1) else {
                    break;
                };
                remaining = next;
                let Some(parent) = all.get(&local).copied().flatten() else {
                    break;
                };
                if parent.kind() == SyntaxFactKind::Declaration
                    && parent.syntax_kind().as_str() == "ocaml.function.declaration"
                {
                    owner = entities.get(&local).map(|entity| (local, entity));
                    break;
                }
                if parent.kind() == SyntaxFactKind::Scope
                    && parent.syntax_kind().as_str() == "ocaml.function.scope"
                {
                    let scope = physical(parent.span())?;
                    owner = owners.get(&scope).copied().flatten();
                    break;
                }
                if !matches!(
                    parent.kind(),
                    SyntaxFactKind::Scope | SyntaxFactKind::Module
                ) {
                    break;
                }
                ancestor = parent.parent();
            }
            let Some((owner_local, owner)) = owner else {
                continue;
            };
            let Some(owner_declaration) = declarations.get(&owner_local).copied().flatten() else {
                continue;
            };
            let Some(owner_definition) = definitions.get(&owner_local).copied().flatten() else {
                continue;
            };
            let owner_span = physical(owner_declaration.span())?;
            let owner_definition_span = physical(owner_definition.span())?;
            let Some(ContainerRef::Entity(module_id)) = owner.record.container else {
                continue;
            };
            let Some((module_local, module)) = by_symbol.get(&module_id).copied().flatten() else {
                continue;
            };
            let Some(module_fact) = all.get(&module_local).copied().flatten() else {
                continue;
            };
            let span = physical(site.span())?;
            let target_source = target.record.evidence.source.as_ref();
            let owner_source = owner.record.evidence.source.as_ref();
            let module_source = module.record.evidence.source.as_ref();
            if module_fact.kind() != SyntaxFactKind::Module
                || module_fact.syntax_kind().as_str() != "ocaml.file.module"
                || module.record.kind != EntityKind::Module
                || module.record.language != "ocaml"
                || owner.record.kind != EntityKind::Function
                || module_source
                    .is_none_or(|r| r.span() != source.span() || !same_source(r, source))
                || owner.record.language != "ocaml"
                || target.record.language != "ocaml"
                || target_source.is_none_or(|r| r.span() != target_span || !same_source(r, source))
                || owner_source.is_none_or(|r| r.span() != owner_span || !same_source(r, source))
                || owner.record.repository != source.repository()
                || target.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || target.record.generation != source.generation()
                || owner.direct_parent != ContainerRef::Entity(module_id)
                || target.record.container != Some(target.direct_parent)
                || target.record.provenance != owner.record.provenance
                || module.record.provenance != owner.record.provenance
                || raw_declarations.get(&(owner_declaration.syntax_kind().as_str(), owner_span))
                    != Some(&1)
                || raw_definitions.get(&owner_definition_span) != Some(&1)
                || owner.definition_local_id != Some(owner_definition.local_id())
                || text(owner_definition.span())? != owner.record.canonical_name
                || !super::span_contains(owner_span, span)
                || !super::span_contains(target_span, definition_span)
                || text(site.span())? != target.record.canonical_name
                || witness.end_byte() != span.end_byte()
                || witness.start_byte() != target_span.start_byte()
                || (is_call
                    && (target.direct_parent != ContainerRef::Entity(module_id)
                        || target_span.end_byte() >= owner_span.start_byte()
                        || target.record.id == owner.record.id))
                || (!is_call && target.direct_parent != ContainerRef::Entity(owner.record.id))
            {
                continue;
            }
            bindings.insert(
                site.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    span,
                    spelling: content_hash(text(site.span())?.as_bytes()),
                    provenance: owner.record.provenance,
                    call: is_call,
                },
            );
        }
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    /// Resolve only the retained native label and its exact current physical occurrence.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        physical: SourceSpan,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !required(fact)
            || site.syntax_kind != fact.syntax_kind().as_str()
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
            .filter(|b| {
                b.span == physical
                    && b.span == site.source.span()
                    && b.spelling == site.syntactic_text_hash
                    && site.enclosing == Some(b.owner)
                    && b.provenance == site.provenance
                    && site.role
                        == if b.call {
                            OccurrenceRole::CallSite
                        } else {
                            OccurrenceRole::Reference
                        }
            })
            .map(|b| b.target))
    }
}

pub(super) fn relation(
    site: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let mut relation = lexical_reference_relation(site, target)?;
    if site.syntax_kind == "ocaml.direct_file_call.call" {
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "ocaml"
                && owner.record.kind == EntityKind::Function
                && site.enclosing == Some(owner.record.id)
        }) else {
            return Ok(None);
        };
        relation.predicate = RelationPredicate::Calls;
        relation.subject = RelationEndpoint::Entity(owner.record.id);
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
            .map_err(|_| provider_failure("ocaml-written-call-identity"))?;
    }
    Ok(Some(relation))
}
