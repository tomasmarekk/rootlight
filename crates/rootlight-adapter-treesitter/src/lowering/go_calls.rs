//! Join native bare-call certificates to exact current Go function witnesses.
//! Raw declaration, Definition and full-function context cardinality is checked
//! before entity deduplication can hide a missing or competing witness.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
    source_for_span, span_contains,
};

/// Recognize the call certificate independently of written function-value uses.
pub(super) fn call(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "go.direct_file_function.reference"
}

/// Recognize a native function argument identity without implying an invocation.
pub(super) fn value_reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "go.function_value.reference"
}

/// Recognize a native bare-callee site without claiming a project target.
pub(super) fn package_candidate(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "go.package_function_candidate.reference"
}

/// Retain both certificate forms in exact syntax artifact accounting.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    call(fact) || value_reference(fact)
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|entry| *entry = None)
        .or_insert(Some(value));
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
        let mut contexts = HashMap::new();
        let mut occurrences = HashMap::new();
        let mut value_contexts = HashMap::new();
        let mut value_sites = HashMap::new();
        let mut names = HashMap::new();
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
        contexts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        occurrences
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        value_contexts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        value_sites
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
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
                    || fact.syntax_kind().as_str() != "go.file.root"
                    || fact.parent().is_some()
                    || span.file() != source.span().file()
                    || span.end_byte() != source.span().end_byte()
                    || source.span().start_byte() != 0
                    || !matches!(text(prefix)?, "" | "\u{feff}");
            }
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Declaration {
                unique(&mut declarations, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
            {
                unique(&mut definitions, fact.parent(), fact);
                unique(&mut definition_spans, span, fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Signature
                && matches!(
                    fact.syntax_kind().as_str(),
                    "go.call_formal_context.signature" | "go.composite_formal_context.signature"
                )
            {
                unique(&mut contexts, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Signature {
                unique(&mut value_contexts, span, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence {
                unique(&mut value_sites, span, fact.local_id());
            }
            if required(fact) {
                unique(&mut occurrences, span, fact.local_id());
            }
        }
        if invalid || root.is_none() || all.values().any(Option::is_none) {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        let root = root.expect("the single current root was checked");
        let mut validated = HashMap::new();
        validated
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "go" || entity.record.kind != EntityKind::Function {
                continue;
            }
            unique(&mut names, entity.record.canonical_name.as_str(), *local);
            let Some(evidence) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(declaration) = all.get(local).copied().flatten() else {
                continue;
            };
            let Some(definition) = definitions.get(&Some(*local)).copied().flatten() else {
                continue;
            };
            let span = physical(declaration.span())?;
            let Some(context) = contexts.get(&span).copied().flatten() else {
                continue;
            };
            let name_span = physical(definition.span())?;
            let name = entity.record.canonical_name.as_str();
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "go.function.declaration"
                || declaration.parent() != Some(root)
                || declarations
                    .get(&span)
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(*local)
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "go.identifier.definition"
                || entity.definition_local_id != Some(definition.local_id())
                || definition_spans.get(&name_span) != Some(&Some(definition.local_id()))
                || context.parent() != Some(*local)
                || entity.direct_parent != ContainerRef::File(source.span().file())
                || entity.record.container != Some(entity.direct_parent)
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || evidence != &source_for_span(source, span)
                || !span_contains(span, name_span)
                || text(definition.span())? != name
                || name == "_"
                || !name.is_ascii()
            {
                continue;
            }
            validated.insert(*local, entity);
        }
        for fact in facts.iter().filter(|fact| required(fact)) {
            cancel.check()?;
            let span = physical(fact.span())?;
            if occurrences.get(&span) != Some(&Some(fact.local_id())) {
                continue;
            }
            if value_reference(fact) {
                let Some(context) = value_contexts.get(&span).copied().flatten() else {
                    continue;
                };
                // The native paired carrier supplies independent site authority;
                // an ordinary argument relabel alone cannot borrow a file function.
                if context.syntax_kind().as_str() != "go.function_value_context.signature"
                    || context.parent() != fact.parent()
                    || context.depth() != fact.depth()
                    || context.span() != fact.span()
                    || value_sites.get(&span) != Some(&Some(fact.local_id()))
                    || text(context.span())? != text(fact.span())?
                {
                    continue;
                }
            }
            let Some(target_local) = names.get(text(fact.span())?).copied().flatten() else {
                continue;
            };
            let Some(target) = validated.get(&target_local).copied() else {
                continue;
            };
            let mut ancestor = fact.parent();
            let mut owner = None;
            for _ in 0..facts.len() {
                cancel.check()?;
                let Some(local) = ancestor else { break };
                let Some(raw) = all.get(&local).copied().flatten() else {
                    break;
                };
                if let Some(entity) = entities.get(&local) {
                    if entity.record.kind == EntityKind::Function {
                        owner = validated.get(&local).copied().map(|entity| (local, entity));
                        break;
                    }
                    if entity.record.language != "go" || entity.record.kind != EntityKind::Variable
                    {
                        break;
                    }
                }
                ancestor = raw.parent();
            }
            let Some((owner_local, owner)) = owner else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            if !span_contains(owner_source.span(), span)
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
        let expected_role = if value_reference(fact) {
            OccurrenceRole::Reference
        } else {
            OccurrenceRole::CallSite
        };
        if !required(fact)
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || occurrence.role != expected_role
            || occurrence.enclosing != Some(binding.owner)
            || owner.record.id != binding.owner
            || occurrence.provenance != binding.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source != source_for_span(&self.source, span)
            || occurrence.source.span() != binding.span
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
    if occurrence.role == OccurrenceRole::Reference {
        return lexical_reference_relation(occurrence, target);
    }
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
        .map_err(|_| provider_failure("go-written-call-identity"))?;
    Ok(relation)
}
