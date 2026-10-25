//! Resolve a certified default-export Reference through fresh module/Class joins.
//! Raw labels alone cannot supply a target, name geometry or current source.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::{MaterializedEntity, source_for_span};

mod source_proof;

pub(super) const REFERENCE: &str = "javascript.default_class_export.reference";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

fn extent(span: SourceSpan) -> Option<(usize, usize)> {
    Some((
        usize::try_from(span.start_byte()).ok()?,
        usize::try_from(span.end_byte()).ok()?,
    ))
}

fn child(fact: &SyntaxFact, parent: &SyntaxFact) -> bool {
    fact.parent() == Some(parent.local_id()) && parent.depth().checked_add(1) == Some(fact.depth())
}

type Geometry<'a> = HashMap<(SyntaxFactKind, (usize, usize)), Option<&'a SyntaxFact>>;

fn at<'a>(
    facts: &Geometry<'a>,
    kind: SyntaxFactKind,
    span: (usize, usize),
    label: &str,
) -> Option<&'a SyntaxFact> {
    facts
        .get(&(kind, span))
        .copied()
        .flatten()
        .filter(|fact| fact.syntax_kind().as_str() == label)
}

struct Binding {
    local: u64,
    target: SymbolId,
    module: SymbolId,
    provenance: FactId,
    span: SourceSpan,
    spelling: ContentHash,
}

pub(super) struct Targets {
    source: SourceRef,
    binding: Option<Binding>,
}

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
            binding: None,
        };
        if source.span().start_byte() != 0
            || usize::try_from(source.span().end_byte()).ok() != Some(original.len())
        {
            return Ok(output);
        }
        let mut hash = blake3::Hasher::new();
        for bytes in original.chunks(4096) {
            cancel.check()?;
            hash.update(bytes);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(output);
        }
        let Some(proof) = source_proof::collect(logical, maximum, cancel)? else {
            return Ok(output);
        };
        let mut ids = HashSet::new();
        let mut geometry = Geometry::new();
        let mut root = None;
        let mut module = None;
        let mut definition_owners = HashMap::new();
        for fact in facts {
            cancel.check()?;
            if fact.span().file() != source.span().file()
                || fact.kind() == SyntaxFactKind::ErrorRecovery
            {
                return Ok(output);
            }
            ids.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !ids.insert(fact.local_id()) {
                return Ok(output);
            }
            let Some(span) = extent(fact.span()) else {
                return Ok(output);
            };
            geometry
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            geometry
                .entry((fact.kind(), span))
                .and_modify(|old| *old = None)
                .or_insert(Some(fact));
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
                && let Some(parent) = fact.parent()
            {
                definition_owners
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                definition_owners
                    .entry(parent)
                    .and_modify(|old| *old = None)
                    .or_insert(Some(fact.local_id()));
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
        let (Some(root), Some(module), Some(class), Some(name), Some(signature), Some(reference)) = (
            root,
            module,
            at(
                &geometry,
                SyntaxFactKind::Declaration,
                proof.class,
                "javascript.class.declaration",
            ),
            at(
                &geometry,
                SyntaxFactKind::Occurrence,
                proof.name,
                "javascript.identifier.definition",
            ),
            at(
                &geometry,
                SyntaxFactKind::Signature,
                proof.site,
                "javascript.default_export_value.signature",
            ),
            at(&geometry, SyntaxFactKind::Occurrence, proof.site, REFERENCE),
        ) else {
            return Ok(output);
        };
        let (Some(owner), Some(target)) = (
            entities.get(&module.local_id()),
            entities.get(&class.local_id()),
        ) else {
            return Ok(output);
        };
        let spelling = text(reference.span())?;
        if root.parent().is_some()
            || root.depth() != 0
            || !child(module, root)
            || !child(class, module)
            || !child(name, class)
            || !child(signature, module)
            || !child(reference, module)
            || physical(root.span())? != source.span()
            || module.span() != root.span()
            || definition_owners.get(&class.local_id()) != Some(&Some(name.local_id()))
            || spelling != text(name.span())?
            || owner.record.kind != EntityKind::Module
            || owner.record.language != "javascript"
            || owner.record.repository != source.repository()
            || owner.record.generation != source.generation()
            || owner.record.container != Some(ContainerRef::File(source.span().file()))
            || owner.direct_parent != ContainerRef::File(source.span().file())
            || owner.definition_local_id.is_some()
            || owner.record.evidence.source.as_ref() != Some(source)
            || target.record.kind != EntityKind::Class
            || target.record.language != "javascript"
            || target.record.repository != source.repository()
            || target.record.generation != source.generation()
            || target.direct_parent != ContainerRef::Entity(owner.record.id)
            || target.record.container != Some(target.direct_parent)
            || target.record.canonical_name != spelling
            || target.definition_local_id != Some(name.local_id())
            || target.record.provenance != owner.record.provenance
            || target.record.evidence.source.as_ref()
                != Some(&source_for_span(source, physical(class.span())?))
        {
            return Ok(output);
        }
        // A materialized competing root binding cannot borrow the unique Class
        // selected by the independent native module census.
        for candidate in entities.values() {
            cancel.check()?;
            if candidate.record.id != target.record.id
                && candidate.record.language == "javascript"
                && candidate.record.container == Some(ContainerRef::Entity(owner.record.id))
                && candidate.record.canonical_name == spelling
            {
                return Ok(output);
            }
        }
        output.binding = Some(Binding {
            local: reference.local_id(),
            target: target.record.id,
            module: owner.record.id,
            provenance: owner.record.provenance,
            span: physical(reference.span())?,
            spelling: content_hash(spelling.as_bytes()),
        });
        Ok(output)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = &self.binding else {
            return Ok(None);
        };
        Ok((required(fact) && fact.local_id() == binding.local && span == binding.span
            && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == REFERENCE
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == span.file() && occurrence.source == source_for_span(&self.source, span)
            && occurrence.enclosing == Some(binding.module) && occurrence.provenance == binding.provenance
            && occurrence.syntactic_text_hash == binding.spelling && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == binding.spelling)).then_some(binding.target))
    }
}
