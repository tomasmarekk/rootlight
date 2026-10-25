//! Join a fresh complete-source census to unique native and materialized owners.
//! An initializer Variable never supplies the Method endpoint of a lexical Read.

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
    source_for_span,
};

mod source_proof;

pub(super) const REFERENCE: &str = "javascript.method_var.reference";
pub(super) const READ: &str = "javascript.method_var_read.read";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

fn geometry(span: SourceSpan) -> Option<(usize, usize)> {
    Some((
        usize::try_from(span.start_byte()).ok()?,
        usize::try_from(span.end_byte()).ok()?,
    ))
}

fn child(fact: &SyntaxFact, parent: &SyntaxFact) -> bool {
    fact.parent() == Some(parent.local_id()) && parent.depth().checked_add(1) == Some(fact.depth())
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

type GeometryMap<'a> = HashMap<(SyntaxFactKind, (usize, usize)), Option<&'a SyntaxFact>>;

fn at<'a>(
    map: &GeometryMap<'a>,
    kind: SyntaxFactKind,
    span: (usize, usize),
    label: &str,
) -> Option<&'a SyntaxFact> {
    map.get(&(kind, span))
        .copied()
        .flatten()
        .filter(|fact| fact.syntax_kind().as_str() == label)
}

fn entity(
    entity: &MaterializedEntity,
    raw: (&SyntaxFact, &SyntaxFact),
    kind: EntityKind,
    parent: ContainerRef,
    source: &SourceRef,
    physical: SourceSpan,
    name: &str,
) -> bool {
    let (declaration, definition) = raw;
    entity.record.kind == kind
        && entity.record.language == "javascript"
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity.record.evidence.source.as_ref() == Some(&source_for_span(source, physical))
        && entity.definition_local_id == Some(definition.local_id())
        && child(definition, declaration)
        && entity.direct_parent == parent
        && entity.record.container == Some(parent)
        && entity.record.canonical_name == name
}

struct Binding {
    target: SymbolId,
    owner: SymbolId,
    owner_local: u64,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

/// The caller binds logical parsing to the complete original physical source.
pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) logical: &'a [u8],
    pub(super) original: &'a [u8],
    pub(super) maximum: usize,
}

impl Targets {
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_>,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let CurrentSource {
            reference: source,
            logical,
            original,
            maximum,
        } = current;
        let mut output = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        if source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(original.len())
        {
            return Ok(output);
        }
        let mut hash = blake3::Hasher::new();
        for chunk in original.chunks(4096) {
            cancel.check()?;
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(output);
        }
        let proofs = source_proof::collect(logical, maximum, cancel)?;
        if proofs.is_empty() {
            return Ok(output);
        }
        let mut ids = HashMap::new();
        let mut spans = GeometryMap::new();
        let mut definitions = HashMap::new();
        let mut names = HashMap::new();
        let mut root = None;
        let mut module = None;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.span().file() != source.span().file()
            {
                return Ok(output);
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if ids.insert(fact.local_id(), fact).is_some() {
                return Ok(output);
            }
            let Some(span) = geometry(fact.span()) else {
                return Ok(output);
            };
            spans
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique(&mut spans, (fact.kind(), span), fact);
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
                && let Some(parent) = fact.parent()
            {
                definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(&mut definitions, parent, fact);
            }
            match fact.kind() {
                SyntaxFactKind::Root => {
                    if fact.syntax_kind().as_str() != "javascript.program.root"
                        || root.replace(fact).is_some()
                    {
                        return Ok(output);
                    }
                }
                SyntaxFactKind::Module
                    if fact.syntax_kind().as_str() != "javascript.file.module"
                        || module.replace(fact).is_some() =>
                {
                    return Ok(output);
                }
                _ => {}
            }
        }
        let (Some(root), Some(module)) = (root, module) else {
            return Ok(output);
        };
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(output);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || !child(module, root)
            || physical(root.span())? != source.span()
            || module.span() != root.span()
            || module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "javascript"
            || module_entity.record.repository != source.repository()
            || module_entity.record.generation != source.generation()
            || module_entity.record.evidence.source.as_ref() != Some(source)
            || module_entity.direct_parent != ContainerRef::File(source.span().file())
            || module_entity.record.container != Some(module_entity.direct_parent)
            || module_entity.definition_local_id.is_some()
        {
            return Ok(output);
        }
        for entity in entities
            .values()
            .filter(|entity| entity.record.language == "javascript")
        {
            cancel.check()?;
            names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique(
                &mut names,
                (
                    entity.record.kind,
                    entity.record.container,
                    entity.record.canonical_name.as_str(),
                ),
                entity.record.id,
            );
        }
        for reference in facts.iter().filter(|fact| required(fact)) {
            cancel.check()?;
            let Some(site) = geometry(reference.span()) else {
                continue;
            };
            let Some(proof) = proofs.get(&site) else {
                continue;
            };
            if at(&spans, SyntaxFactKind::Occurrence, site, REFERENCE).map(SyntaxFact::local_id)
                != Some(reference.local_id())
            {
                continue;
            }
            let (
                Some(class),
                Some(class_name),
                Some(owner),
                Some(owner_name),
                Some(scope),
                Some(body),
                Some(parameters),
                Some(declaration),
                Some(definition),
                Some(hoisted),
            ) = (
                at(
                    &spans,
                    SyntaxFactKind::Declaration,
                    proof.class,
                    "javascript.class.declaration",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Occurrence,
                    proof.class_name,
                    "javascript.identifier.definition",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Declaration,
                    proof.method,
                    "javascript.method.declaration",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Occurrence,
                    proof.method_name,
                    "javascript.property_identifier.definition",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Scope,
                    proof.method,
                    "javascript.method.scope",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Scope,
                    proof.body,
                    "javascript.block.scope",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Signature,
                    proof.parameters,
                    "javascript.parameters.signature",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Declaration,
                    proof.declaration,
                    "javascript.variable.declaration",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Occurrence,
                    proof.definition,
                    "javascript.identifier.definition",
                ),
                at(
                    &spans,
                    SyntaxFactKind::Signature,
                    proof.declaration,
                    "javascript.hoisted_binding.signature",
                ),
            )
            else {
                continue;
            };
            if !child(class, module)
                || !child(class_name, class)
                || !child(scope, class)
                || !child(owner, scope)
                || !child(owner_name, owner)
                || !child(body, owner)
                || !child(parameters, owner)
                || !child(declaration, body)
                || !child(definition, declaration)
                || !child(hoisted, declaration)
                || definitions
                    .get(&class.local_id())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(class_name.local_id())
                || definitions
                    .get(&owner.local_id())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(owner_name.local_id())
                || definitions
                    .get(&declaration.local_id())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(definition.local_id())
            {
                continue;
            }
            let expected_context = if proof.initializer {
                let Some(context) = at(
                    &spans,
                    SyntaxFactKind::Declaration,
                    proof.context,
                    "javascript.variable.declaration",
                ) else {
                    continue;
                };
                if !child(context, body) {
                    continue;
                }
                context
            } else {
                body
            };
            if !child(reference, expected_context) {
                continue;
            }
            let (Some(class_entity), Some(owner_entity), Some(target)) = (
                entities.get(&class.local_id()),
                entities.get(&owner.local_id()),
                entities.get(&declaration.local_id()),
            ) else {
                continue;
            };
            let class_text = text(class_name.span())?;
            let owner_text = text(owner_name.span())?;
            let target_text = text(definition.span())?;
            let site_text = text(reference.span())?;
            if site_text != target_text
                || !entity(
                    class_entity,
                    (class, class_name),
                    EntityKind::Class,
                    ContainerRef::Entity(module_entity.record.id),
                    source,
                    physical(class.span())?,
                    class_text,
                )
                || !entity(
                    owner_entity,
                    (owner, owner_name),
                    EntityKind::Method,
                    ContainerRef::Entity(class_entity.record.id),
                    source,
                    physical(owner.span())?,
                    owner_text,
                )
                || !entity(
                    target,
                    (declaration, definition),
                    EntityKind::Variable,
                    ContainerRef::Entity(owner_entity.record.id),
                    source,
                    physical(declaration.span())?,
                    target_text,
                )
                || class_entity.record.provenance != module_entity.record.provenance
                || owner_entity.record.provenance != module_entity.record.provenance
                || target.record.provenance != module_entity.record.provenance
            {
                continue;
            }
            let unique_entities = [class_entity, owner_entity, target].iter().all(|entity| {
                names.get(&(
                    entity.record.kind,
                    entity.record.container,
                    entity.record.canonical_name.as_str(),
                )) == Some(&Some(entity.record.id))
            });
            if !unique_entities {
                continue;
            }
            let span = physical(reference.span())?;
            output
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            output.bindings.insert(
                reference.local_id(),
                Binding {
                    target: target.record.id,
                    owner: owner_entity.record.id,
                    owner_local: owner.local_id(),
                    span,
                    spelling: content_hash(site_text.as_bytes()),
                    provenance: module_entity.record.provenance,
                },
            );
        }
        Ok(output)
    }

    pub(super) fn owner<'entity>(
        &self,
        fact: &SyntaxFact,
        entities: &'entity HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<&'entity MaterializedEntity>, AdapterError> {
        cancel.check()?;
        Ok(self.bindings.get(&fact.local_id()).and_then(|binding| {
            entities.get(&binding.owner_local).filter(|owner| {
                owner.record.id == binding.owner
                    && owner.record.kind == EntityKind::Method
                    && owner.record.language == "javascript"
                    && owner.record.provenance == binding.provenance
            })
        }))
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        Ok((required(fact) && span == binding.span && occurrence.source == source_for_span(&self.source, span)
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == span.file() && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == REFERENCE
            && occurrence.syntactic_text_hash == binding.spelling && occurrence.enclosing == Some(binding.owner)
            && occurrence.provenance == binding.provenance && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)).then_some(binding.target))
    }
}

pub(super) fn relation(
    read: &OccurrenceRecord,
    owner: &MaterializedEntity,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<RelationRecord, AdapterError> {
    cancel.check()?;
    let mut relation = lexical_reference_relation(read, target)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Reads;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(read.id),
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("javascript-method-var-read-relation"))?;
    cancel.check()?;
    Ok(relation)
}
