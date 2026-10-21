//! Join native GLSL certificates to exact current declaration/Definition pairs.
//! Cross-declaration call witnesses belong to the physical file; formal witnesses
//! belong to their written function. Duplicate raw evidence never donates a target.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, RelationEndpoint,
    RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
    source_for_span, span_contains,
};

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "glsl.formal_target.signature" | "glsl.call_target.signature"
        )
}

pub(super) fn required(fact: &SyntaxFact) -> bool {
    metadata(fact)
        || fact.kind() == SyntaxFactKind::Occurrence
            && matches!(
                fact.syntax_kind().as_str(),
                "glsl.formal_value.reference" | "glsl.direct_file_function.reference"
            )
}

pub(super) fn call(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "glsl.direct_file_function.reference"
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|entry| *entry = None)
        .or_insert(Some(value));
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    owner_local: u64,
    span: SourceSpan,
    provenance: FactId,
    call: bool,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut all = HashMap::new();
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut occurrences = HashMap::new();
        let mut markers = HashMap::new();
        let mut cardinality = HashMap::new();
        let mut entity_names = HashMap::new();
        let mut bindings = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        occurrences
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        cardinality
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        entity_names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        bindings
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut roots = 0;
        let mut root_id = None;
        let mut modules = Vec::new();
        let mut scopes = Vec::new();
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            let span = physical(fact.span())?;
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Root {
                roots += 1;
                if fact.syntax_kind().as_str() != "glsl.file.root"
                    || fact.parent().is_some()
                    || span != source.span()
                {
                    invalid = true;
                }
                root_id = Some(fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Module
                && fact.syntax_kind().as_str() == "glsl.file.module"
            {
                modules
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                modules.push(fact);
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "glsl.file.scope"
            {
                scopes
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                scopes.push(fact);
            }
            if fact.kind() == SyntaxFactKind::Declaration {
                unique(&mut declarations, span.start_byte(), fact);
                unique(&mut cardinality, (false, span), fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Occurrence {
                unique(&mut occurrences, span.end_byte(), fact);
                if fact.syntax_kind().as_str().ends_with(".definition") {
                    unique(&mut cardinality, (true, span), fact.local_id());
                    if let Some(parent) = fact.parent() {
                        unique(&mut definitions, parent, fact);
                    }
                }
            }
            if metadata(fact) {
                unique(&mut markers, span.end_byte(), fact);
            }
        }
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "glsl" {
                let owner = if entity.record.kind == EntityKind::Parameter {
                    entity.record.container
                } else {
                    None
                };
                unique(
                    &mut entity_names,
                    (
                        entity.record.kind,
                        owner,
                        entity.record.canonical_name.as_str(),
                    ),
                    entity.record.id,
                );
            }
        }
        for value in all.values() {
            cancel.check()?;
            invalid |= value.is_none();
        }
        if roots != 1 || invalid {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        let ([module], [scope]) = (modules.as_slice(), scopes.as_slice()) else {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        };
        if module.parent() != root_id
            || scope.parent() != Some(module.local_id())
            || physical(module.span())? != source.span()
            || physical(scope.span())? != source.span()
        {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let witness = physical(marker.span())?;
            let Some(site) = occurrences.get(&witness.end_byte()).copied().flatten() else {
                continue;
            };
            let is_call = call(site);
            if !required(site)
                || metadata(site)
                || is_call != (marker.syntax_kind().as_str() == "glsl.call_target.signature")
            {
                continue;
            }
            let Some(target_declaration) =
                declarations.get(&witness.start_byte()).copied().flatten()
            else {
                continue;
            };
            let Some(target) = entities.get(&target_declaration.local_id()) else {
                continue;
            };
            let Some(target_definition) = definitions
                .get(&target_declaration.local_id())
                .copied()
                .flatten()
            else {
                continue;
            };
            let mut ancestor = site.parent();
            let mut owner = None;
            let mut steps = 0;
            while let Some(local) = ancestor {
                cancel.check()?;
                if steps >= facts.len() {
                    break;
                }
                steps += 1;
                let Some(parent) = all.get(&local).copied().flatten() else {
                    break;
                };
                if let Some(entity) = entities.get(&local) {
                    if entity.record.kind == EntityKind::Function
                        && entity.record.language == "glsl"
                    {
                        owner = Some((parent, entity));
                        break;
                    }
                    if entity.record.kind != EntityKind::Variable
                        || entity.record.language != "glsl"
                        || parent.kind() != SyntaxFactKind::Declaration
                        || parent.syntax_kind().as_str() != "glsl.variable.declaration"
                    {
                        break;
                    }
                    ancestor = parent.parent();
                    continue;
                }
                if parent.kind() != SyntaxFactKind::Scope {
                    break;
                }
                ancestor = parent.parent();
            }
            let Some((owner_declaration, owner)) = owner else {
                continue;
            };
            let Some(owner_definition) = definitions
                .get(&owner_declaration.local_id())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(target_source) = target.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let target_span = physical(target_declaration.span())?;
            let owner_span = physical(owner_declaration.span())?;
            let site_span = physical(site.span())?;
            let target_name_span = physical(target_definition.span())?;
            let owner_name_span = physical(owner_definition.span())?;
            let expected_kind = if is_call {
                EntityKind::Function
            } else {
                EntityKind::Parameter
            };
            if owner_declaration.parent() != Some(scope.local_id())
                || is_call && target_declaration.parent() != Some(scope.local_id())
            {
                continue;
            }
            if target.record.kind != expected_kind
                || target.record.language != "glsl"
                || owner_declaration.syntax_kind().as_str() != "glsl.function.declaration"
                || target_declaration.syntax_kind().as_str()
                    != if is_call {
                        "glsl.function.declaration"
                    } else {
                        "glsl.parameter.declaration"
                    }
                || target_definition.syntax_kind().as_str() != "glsl.name.definition"
                || owner_definition.syntax_kind().as_str() != "glsl.name.definition"
                || target.definition_local_id != Some(target_definition.local_id())
                || owner.definition_local_id != Some(owner_definition.local_id())
                || target.record.container != Some(target.direct_parent)
                || owner.record.container != Some(owner.direct_parent)
                || target_source != &source_for_span(source, target_span)
                || owner_source != &source_for_span(source, owner_span)
                || target.record.repository != source.repository()
                || target.record.generation != source.generation()
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || target.record.provenance != owner.record.provenance
                || text(target_definition.span())? != target.record.canonical_name
                || text(owner_definition.span())? != owner.record.canonical_name
                || text(site.span())? != target.record.canonical_name
                || !span_contains(target_span, target_name_span)
                || !span_contains(owner_span, owner_name_span)
                || !span_contains(owner_span, site_span)
                || cardinality.get(&(false, target_span))
                    != Some(&Some(target_declaration.local_id()))
                || cardinality.get(&(false, owner_span))
                    != Some(&Some(owner_declaration.local_id()))
                || cardinality.get(&(true, target_name_span))
                    != Some(&Some(target_definition.local_id()))
                || cardinality.get(&(true, owner_name_span))
                    != Some(&Some(owner_definition.local_id()))
            {
                continue;
            }
            let name_owner = if is_call {
                None
            } else {
                target.record.container
            };
            if entity_names.get(&(
                expected_kind,
                name_owner,
                target.record.canonical_name.as_str(),
            )) != Some(&Some(target.record.id))
            {
                continue;
            }
            if is_call {
                if target_span.start_byte() > owner_span.start_byte() {
                    continue;
                }
                // A same-owner witness is strictly contained by its Function
                // Declaration; cross-declaration witnesses belong to the file Scope.
                let recursive = target_declaration.local_id() == owner_declaration.local_id();
                let expected_parent = if recursive {
                    if target.record.id != owner.record.id || !span_contains(owner_span, witness) {
                        continue;
                    }
                    owner_declaration.local_id()
                } else {
                    scope.local_id()
                };
                if marker.parent() != Some(expected_parent) {
                    continue;
                }
            } else if marker.parent() != Some(owner_declaration.local_id())
                || target_declaration.parent() != Some(owner_declaration.local_id())
                || target.record.container
                    != Some(rootlight_ir::ContainerRef::Entity(owner.record.id))
                || !span_contains(owner_span, witness)
                || target_name_span.end_byte() > site_span.start_byte()
            {
                continue;
            }
            bindings.insert(
                site.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner.record.id,
                    owner_local: owner_declaration.local_id(),
                    span: site_span,
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

    pub(super) fn owner<'entity>(
        &self,
        fact: &SyntaxFact,
        entities: &'entity HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<&'entity MaterializedEntity>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok(entities.get(&binding.owner_local).filter(|owner| {
            owner.record.id == binding.owner
                && owner.record.kind == EntityKind::Function
                && owner.record.language == "glsl"
                && owner.record.provenance == binding.provenance
        }))
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        let Some(owner) = owner else { return Ok(None) };
        if !required(fact)
            || metadata(fact)
            || call(fact) != binding.call
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || occurrence.role
                != if binding.call {
                    OccurrenceRole::CallSite
                } else {
                    OccurrenceRole::Reference
                }
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || owner.record.id != binding.owner
            || occurrence.enclosing != Some(binding.owner)
            || span != binding.span
            || occurrence.source != source_for_span(&self.source, span)
            || occurrence.file != self.source.span().file()
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.provenance != binding.provenance
            || owner.record.provenance != binding.provenance
            || occurrence.syntactic_text_hash != content_hash(text.as_bytes())
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}

pub(super) fn relation(
    occurrence: &OccurrenceRecord,
    owner: &MaterializedEntity,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<RelationRecord, AdapterError> {
    cancel.check()?;
    let mut relation = lexical_reference_relation(occurrence, target)?;
    if occurrence.role == OccurrenceRole::CallSite {
        relation.subject = RelationEndpoint::Entity(owner.record.id);
        relation.predicate = RelationPredicate::Calls;
        relation
            .evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        relation.evidence.derivation.extend([
            FactRef::Entity(owner.record.id),
            FactRef::Fact(occurrence.id),
            FactRef::Entity(target),
        ]);
        relation.evidence.derivation.sort_unstable();
        relation.evidence.derivation.dedup();
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| provider_failure("glsl-written-call-identity"))?;
    }
    cancel.check()?;
    Ok(relation)
}
