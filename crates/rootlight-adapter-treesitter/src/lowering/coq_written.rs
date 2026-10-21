//! Join native direct-body references to current written callable identities.
//! Complete root/module/header and singular raw Declaration/Definition witnesses
//! are required before materialized entities can supply a reference target.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::{MaterializedEntity, source_for_span, span_contains};

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|entry| *entry = None)
        .or_insert(Some(value));
}

fn hash(text: &str, cancel: &Cancellation) -> Result<ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        hash.update(chunk);
    }
    Ok(ContentHash::from_bytes(*hash.finalize().as_bytes()))
}

fn equal(left: &str, right: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.as_bytes().chunks(64).zip(right.as_bytes().chunks(64)) {
        cancel.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

fn declaration_prefix(text: &str, cancel: &Cancellation) -> Result<bool, AdapterError> {
    let Some(space) = text.strip_prefix("Definition") else {
        return Ok(false);
    };
    if space.is_empty() {
        return Ok(false);
    }
    for chunk in space.as_bytes().chunks(64) {
        cancel.check()?;
        if !chunk.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Binding {
    span: SourceSpan,
    owner: SymbolId,
    target: SymbolId,
    provenance: FactId,
    spelling: ContentHash,
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
        let mut starts = HashMap::new();
        let mut definitions = HashMap::new();
        let mut definition_spans = HashMap::new();
        let mut headers = HashMap::new();
        let mut signature_spans = HashMap::new();
        let mut names = HashMap::new();
        let mut references = HashMap::new();
        let mut markers = HashMap::new();
        let mut bindings = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        declarations
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        starts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definition_spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        headers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        signature_spans
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        references
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        bindings
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let (mut root, mut module, mut context, mut invalid) = (None, None, None, false);
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            let span = physical(fact.span())?;
            invalid |=
                !span_contains(source.span(), span) || fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Signature {
                unique(&mut signature_spans, span, fact.local_id());
            }
            match fact.kind() {
                SyntaxFactKind::Root => {
                    invalid |= root.replace(fact).is_some()
                        || fact.parent().is_some()
                        || fact.syntax_kind().as_str() != "coq.source_file.root"
                        || span.end_byte() != source.span().end_byte()
                        || source.span().start_byte() != 0;
                    let prefix = SourceSpan::new(fact.span().file(), 0, fact.span().start_byte())
                        .map_err(|_| SinkError::InvalidIr)?;
                    invalid |= !matches!(text(prefix)?, "" | "\u{feff}");
                }
                SyntaxFactKind::Module => {
                    invalid |= module.replace(fact).is_some()
                        || fact.syntax_kind().as_str() != "coq.file.module";
                }
                SyntaxFactKind::Declaration => {
                    unique(&mut declarations, span, fact.local_id());
                    unique(&mut starts, span.start_byte(), fact.local_id());
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "coq.proof_checking.signature" =>
                {
                    invalid |= context.replace(fact).is_some();
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "coq.written_header.signature" =>
                {
                    unique(&mut headers, fact.parent(), fact);
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str() == "coq.written_target.signature" =>
                {
                    unique(&mut markers, span.end_byte(), fact);
                }
                SyntaxFactKind::Occurrence
                    if fact.syntax_kind().as_str().ends_with(".definition") =>
                {
                    unique(&mut definitions, fact.parent(), fact);
                    unique(&mut definition_spans, span, fact.local_id());
                }
                SyntaxFactKind::Occurrence
                    if fact.syntax_kind().as_str() == "coq.written_reference.reference" =>
                {
                    unique(&mut references, span.end_byte(), fact);
                }
                _ => {}
            }
        }
        let (Some(root), Some(module), Some(context)) = (root, module, context) else {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        };
        if invalid
            || all.values().any(Option::is_none)
            || module.parent() != Some(root.local_id())
            || module.span() != root.span()
            || context.parent() != Some(module.local_id())
            || context.span() != root.span()
        {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        let mut materialized_module = None;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.kind == EntityKind::Module
                && materialized_module.replace((*local, entity)).is_some()
            {
                return Ok(Self {
                    source: source.clone(),
                    bindings,
                });
            }
        }
        let Some((module_local, module_entity)) = materialized_module else {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        };
        let module_span = physical(module.span())?;
        if module_local != module.local_id()
            || module_entity.record.language != "coq"
            || module_entity.record.repository != source.repository()
            || module_entity.record.generation != source.generation()
            || module_entity.direct_parent != ContainerRef::File(source.span().file())
            || module_entity.record.container != Some(module_entity.direct_parent)
            || module_entity.definition_local_id.is_some()
            || module_entity.record.evidence.source.as_ref()
                != Some(&source_for_span(source, module_span))
        {
            return Ok(Self {
                source: source.clone(),
                bindings,
            });
        }
        for fact in facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Declaration)
        {
            cancel.check()?;
            if fact.parent() != Some(module.local_id())
                || !matches!(
                    fact.syntax_kind().as_str(),
                    "coq.definition.declaration" | "coq.written_callable.declaration"
                )
            {
                return Ok(Self {
                    source: source.clone(),
                    bindings,
                });
            }
            let Some(definition) = definitions.get(&Some(fact.local_id())).copied().flatten()
            else {
                return Ok(Self {
                    source: source.clone(),
                    bindings,
                });
            };
            unique(
                &mut names,
                hash(text(definition.span())?, cancel)?,
                fact.local_id(),
            );
        }
        let mut validated = HashMap::new();
        validated
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "coq" || entity.record.kind != EntityKind::Function {
                continue;
            }
            let Some(declaration) = all.get(local).copied().flatten() else {
                continue;
            };
            let span = physical(declaration.span())?;
            let Some(definition) = definitions.get(&Some(*local)).copied().flatten() else {
                continue;
            };
            let Some(header) = headers.get(&Some(*local)).copied().flatten() else {
                continue;
            };
            let name = physical(definition.span())?;
            if definition.span().start_byte() < declaration.span().start_byte() {
                continue;
            }
            let prefix = SourceSpan::new(
                declaration.span().file(),
                declaration.span().start_byte(),
                definition.span().start_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            let header_span = physical(header.span())?;
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "coq.written_callable.declaration"
                || declaration.parent() != Some(module.local_id())
                || declarations.get(&span) != Some(&Some(*local))
                || starts.get(&span.start_byte()) != Some(&Some(*local))
                || definition.syntax_kind().as_str() != "coq.name.definition"
                || definition_spans.get(&name) != Some(&Some(definition.local_id()))
                || names.get(&hash(text(definition.span())?, cancel)?) != Some(&Some(*local))
                || signature_spans.get(&header_span) != Some(&Some(header.local_id()))
                || entity.definition_local_id != Some(definition.local_id())
                || !span_contains(header_span, name)
                || header_span.start_byte() != span.start_byte()
                || header_span.end_byte() >= span.end_byte()
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || entity.record.provenance != module_entity.record.provenance
                || entity.record.evidence.source.as_ref() != Some(&source_for_span(source, span))
                || !declaration_prefix(text(prefix)?, cancel)?
                || !equal(
                    text(definition.span())?,
                    &entity.record.canonical_name,
                    cancel,
                )?
            {
                continue;
            }
            validated.insert(*local, (entity, span, header_span));
        }
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let window = physical(marker.span())?;
            let Some(target_local) = starts.get(&window.start_byte()).copied().flatten() else {
                continue;
            };
            let Some((target, target_span, _)) = validated.get(&target_local) else {
                continue;
            };
            let Some(reference) = references.get(&window.end_byte()).copied().flatten() else {
                continue;
            };
            let Some(owner_local) = reference.parent() else {
                continue;
            };
            let Some((owner, owner_span, header_span)) = validated.get(&owner_local) else {
                continue;
            };
            let span = physical(reference.span())?;
            let Some(target_definition) = definitions.get(&Some(target_local)).copied().flatten()
            else {
                continue;
            };
            if signature_spans.get(&window) != Some(&Some(marker.local_id()))
                || marker.parent() != Some(module.local_id())
                || target_local == owner_local
                || target_span.end_byte() >= owner_span.start_byte()
                || !span_contains(*owner_span, span)
                || span.start_byte() != header_span.end_byte()
                || target.record.provenance != owner.record.provenance
                || !equal(
                    text(reference.span())?,
                    text(target_definition.span())?,
                    cancel,
                )?
            {
                continue;
            }
            bindings.insert(
                reference.local_id(),
                Binding {
                    span,
                    owner: owner.record.id,
                    target: target.record.id,
                    provenance: owner.record.provenance,
                    spelling: hash(text(reference.span())?, cancel)?,
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
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "coq.written_reference.reference"
            || site.syntax_kind != fact.syntax_kind().as_str()
            || site.role != OccurrenceRole::Reference
            || site.enclosing != Some(binding.owner)
            || site.provenance != binding.provenance
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || site.source != source_for_span(&self.source, binding.span)
            || site.syntactic_text_hash != binding.spelling
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}
