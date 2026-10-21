//! Join a native instance-call certificate to current written class/method facts.
//! This records the selected declaration, never a unique runtime implementation.

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
    source_for_span, span_contains,
};
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "php.this_method.reference"
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "php.this_class_context.signature" | "php.this_method_context.signature"
        )
}

fn unique<K: Eq + Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|current| *current = None)
        .or_insert(Some(value));
}

#[derive(Clone, Copy)]
struct AsciiName<'name>(&'name str);
impl PartialEq for AsciiName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}
impl Eq for AsciiName<'_> {}
impl Hash for AsciiName<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
        state.write_u8(0);
    }
}

struct Binding {
    owner_local: u64,
    owner: SymbolId,
    target: SymbolId,
    span: SourceSpan,
    provenance: FactId,
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
        let mut definition_spans = HashMap::new();
        let mut signatures = HashMap::new();
        let mut entity_ids = HashMap::new();
        let mut occurrence_spans = HashMap::new();
        let mut bindings = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definition_spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        signatures
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        occurrence_spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        entity_ids
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        bindings
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut invalid = false;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            let span = physical(fact.span())?;
            if fact.kind() == SyntaxFactKind::Root {
                let prefix = SourceSpan::new(fact.span().file(), 0, fact.span().start_byte())
                    .map_err(|_| SinkError::InvalidIr)?;
                invalid |= root.replace(fact.local_id()).is_some()
                    || fact.syntax_kind().as_str() != "php.program.root"
                    || fact.parent().is_some()
                    || span.file() != source.span().file()
                    || span.end_byte() != source.span().end_byte()
                    || source.span().start_byte() != 0
                    || !matches!(text(prefix)?, "" | "\u{feff}");
            }
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Declaration {
                unique(&mut declarations, span, fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Occurrence {
                unique(&mut occurrence_spans, span, fact.local_id());
                if fact.syntax_kind().as_str().ends_with(".definition") {
                    unique(&mut definitions, fact.parent(), fact);
                    unique(&mut definition_spans, span, fact.local_id());
                }
            }
            if fact.kind() == SyntaxFactKind::Signature {
                unique(&mut signatures, span, fact);
            }
        }
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut entity_ids, entity.record.id, *local);
        }
        if invalid || root.is_none() || all.values().any(Option::is_none) {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        let mut classes = HashMap::new();
        let mut methods = HashMap::new();
        let mut names = HashMap::new();
        classes
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        methods
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for class_pass in [true, false] {
            for (local, entity) in entities {
                cancel.check()?;
                let class = entity.record.kind == EntityKind::Class;
                if class != class_pass
                    || entity.record.language != "php"
                    || (!class
                        && !matches!(
                            entity.record.kind,
                            EntityKind::Method | EntityKind::Constructor
                        ))
                {
                    continue;
                }
                let Some(declaration) = all.get(local).copied().flatten() else {
                    continue;
                };
                let Some(definition) = definitions.get(&Some(*local)).copied().flatten() else {
                    continue;
                };
                let span = physical(declaration.span())?;
                let name_span = physical(definition.span())?;
                let Some(context) = signatures.get(&span).copied().flatten() else {
                    continue;
                };
                let name = text(definition.span())?;
                let Some((&first, tail)) = name.as_bytes().split_first() else {
                    continue;
                };
                if !(first.is_ascii_alphabetic() || first == b'_')
                    || !tail
                        .iter()
                        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    continue;
                }
                if declaration.kind() != SyntaxFactKind::Declaration
                    || declaration.syntax_kind().as_str()
                        != if class {
                            "php.class.declaration"
                        } else if entity.record.kind == EntityKind::Constructor {
                            "php.constructor.declaration"
                        } else {
                            "php.method.declaration"
                        }
                    || declarations.get(&span) != Some(&Some(*local))
                    || entity_ids.get(&entity.record.id) != Some(&Some(*local))
                    || definition.syntax_kind().as_str() != "php.name.definition"
                    || definition_spans.get(&name_span) != Some(&Some(definition.local_id()))
                    || entity.definition_local_id != Some(definition.local_id())
                    || context.syntax_kind().as_str()
                        != if class {
                            "php.this_class_context.signature"
                        } else {
                            "php.this_method_context.signature"
                        }
                    || context.parent() != Some(*local)
                    || entity.record.repository != source.repository()
                    || entity.record.generation != source.generation()
                    || entity.record.evidence.source.as_ref()
                        != Some(&source_for_span(source, span))
                    || entity.record.canonical_name != name
                    || !span_contains(span, name_span)
                {
                    continue;
                }
                if class {
                    classes.insert(*local, entity);
                    continue;
                }
                let mut ancestor = declaration.parent();
                let mut owner = None;
                for _ in 0..facts.len() {
                    cancel.check()?;
                    let Some(parent) = ancestor else { break };
                    let Some(raw) = all.get(&parent).copied().flatten() else {
                        break;
                    };
                    if let Some(class) = classes.get(&parent).copied() {
                        owner = Some((parent, class));
                        break;
                    }
                    if raw.kind() != SyntaxFactKind::Scope
                        || !matches!(
                            raw.syntax_kind().as_str(),
                            "php.declaration_list.scope" | "php.block.scope"
                        )
                        || !span_contains(physical(raw.span())?, span)
                    {
                        break;
                    }
                    ancestor = raw.parent();
                }
                let Some((class_local, class)) = owner else {
                    continue;
                };
                let Some(class_source) = class.record.evidence.source.as_ref() else {
                    continue;
                };
                if entity.direct_parent != ContainerRef::Entity(class.record.id)
                    || entity.record.container != Some(entity.direct_parent)
                    || entity.record.provenance != class.record.provenance
                    || !span_contains(class_source.span(), span)
                {
                    continue;
                }
                methods.insert(*local, (class_local, entity));
                unique(&mut names, (class_local, AsciiName(name)), *local);
            }
        }
        for fact in facts.iter().filter(|fact| required(fact)) {
            cancel.check()?;
            let span = physical(fact.span())?;
            if occurrence_spans.get(&span) != Some(&Some(fact.local_id())) {
                continue;
            }
            let mut ancestor = fact.parent();
            let mut owner = None;
            for _ in 0..facts.len() {
                cancel.check()?;
                let Some(local) = ancestor else { break };
                let Some(raw) = all.get(&local).copied().flatten() else {
                    break;
                };
                if let Some((class, entity)) = methods.get(&local).copied() {
                    owner = Some((local, class, entity));
                    break;
                }
                if raw.kind() != SyntaxFactKind::Scope
                    || raw.syntax_kind().as_str() != "php.block.scope"
                    || !span_contains(physical(raw.span())?, span)
                {
                    break;
                }
                ancestor = raw.parent();
            }
            let Some((owner_local, class, owner)) = owner else {
                continue;
            };
            let Some(target_local) = names
                .get(&(class, AsciiName(text(fact.span())?)))
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some((_, target)) = methods.get(&target_local).copied() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            if target.record.kind != EntityKind::Method
                || !span_contains(owner_source.span(), span)
                || target.record.provenance != owner.record.provenance
            {
                continue;
            }
            bindings.insert(
                fact.local_id(),
                Binding {
                    owner_local,
                    owner: owner.record.id,
                    target: target.record.id,
                    span,
                    provenance: owner.record.provenance,
                },
            );
        }
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    pub(super) fn owner<'a>(
        &self,
        fact: &SyntaxFact,
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<&'a MaterializedEntity>, AdapterError> {
        cancel.check()?;
        Ok(self.bindings.get(&fact.local_id()).and_then(|binding| {
            entities.get(&binding.owner_local).filter(|entity| {
                entity.record.id == binding.owner && entity.record.provenance == binding.provenance
            })
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
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || occurrence.role != OccurrenceRole::CallSite
            || occurrence.enclosing != Some(binding.owner)
            || owner.record.id != binding.owner
            || occurrence.provenance != binding.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source != source_for_span(&self.source, span)
            || span != binding.span
            || occurrence.syntactic_text_hash != content_hash(text.as_bytes())
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
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
        .map_err(|_| provider_failure("php-written-call-identity"))?;
    Ok(relation)
}
