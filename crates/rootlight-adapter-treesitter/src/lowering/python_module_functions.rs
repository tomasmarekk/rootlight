//! Join a fresh guarded-call source proof to exact materialized function identity.
//! The companion Reference does not qualify or replace existing CallSite facts.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "python.module_function.reference"
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

fn raw_one<'a>(
    facts: &'a [SyntaxFact],
    mut select: impl FnMut(&SyntaxFact) -> bool,
    cancel: &Cancellation,
) -> Result<Option<&'a SyntaxFact>, AdapterError> {
    let mut found = None;
    for fact in facts {
        cancel.check()?;
        if select(fact) {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(fact);
        }
    }
    Ok(found)
}

pub(super) struct Targets<'a> {
    source: &'a SourceRef,
    binding: Option<(u64, SourceSpan, SymbolId, SymbolId, FactId, ContentHash)>,
}

impl<'a> Targets<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "current logical source and original physical identity are checked separately"
    )]
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        current: &[u8],
        original: &[u8],
        maximum: usize,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut output = Self {
            source,
            binding: None,
        };
        if !facts.iter().any(required) {
            return Ok(output);
        }
        if source.span().start_byte() != 0
            || u64::try_from(original.len()).ok() != Some(source.span().end_byte())
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
        let Some(proof) = crate::python_module_functions::source_proof(current, maximum, cancel)?
        else {
            return Ok(output);
        };
        let mut project = |(start, end)| {
            let start = u64::try_from(start)
                .map_err(|_| super::provider_failure("python-module-function-span"))?;
            let end = u64::try_from(end)
                .map_err(|_| super::provider_failure("python-module-function-span"))?;
            let logical = SourceSpan::new(source.span().file(), start, end)
                .map_err(|_| super::provider_failure("python-module-function-span"))?;
            physical(logical)
        };
        let site = project(proof.site)?;
        let declaration_span = project(proof.declaration)?;
        let name_span = project(proof.name)?;
        let written = std::str::from_utf8(&current[proof.name.0..proof.name.1])
            .map_err(|_| super::provider_failure("python-module-function-name"))?;
        let Some(root) = raw_one(facts, |fact| fact.kind() == SyntaxFactKind::Root, cancel)? else {
            return Ok(output);
        };
        if root.syntax_kind().as_str() != "python.module.root"
            || root.parent().is_some()
            || physical(root.span())? != source.span()
        {
            return Ok(output);
        }
        let Some(module) = raw_one(facts, |fact| fact.kind() == SyntaxFactKind::Module, cancel)?
        else {
            return Ok(output);
        };
        if module.syntax_kind().as_str() != "python.file.module"
            || module.parent() != Some(root.local_id())
            || physical(module.span())? != source.span()
        {
            return Ok(output);
        }
        let Some(reference) = raw_one(facts, required, cancel)? else {
            return Ok(output);
        };
        if physical(reference.span())? != site {
            return Ok(output);
        }
        let mut selected = None;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.kind != EntityKind::Function || entity.record.language != "python" {
                continue;
            }
            let Some(evidence) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            if evidence.span() != declaration_span {
                continue;
            }
            if selected.is_some() {
                return Ok(output);
            }
            selected = Some((*local, entity));
        }
        let Some((local, target)) = selected else {
            return Ok(output);
        };
        let Some(target_source) = target.record.evidence.source.as_ref() else {
            return Ok(output);
        };
        let Some(declaration) = raw_one(facts, |fact| fact.local_id() == local, cancel)? else {
            return Ok(output);
        };
        let Some(definition_id) = target.definition_local_id else {
            return Ok(output);
        };
        let Some(definition) = raw_one(facts, |fact| fact.local_id() == definition_id, cancel)?
        else {
            return Ok(output);
        };
        if declaration.kind() != SyntaxFactKind::Declaration
            || declaration.syntax_kind().as_str() != "python.function.declaration"
            || declaration.parent() != Some(module.local_id())
            || physical(declaration.span())? != declaration_span
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.syntax_kind().as_str() != "python.identifier.definition"
            || definition.parent() != Some(local)
            || physical(definition.span())? != name_span
            || target.record.canonical_name != written
            || target.record.repository != source.repository()
            || target.record.generation != source.generation()
            || !same_source(target_source, source)
        {
            return Ok(output);
        }
        for fact in [root, module, reference, declaration, definition] {
            let count = raw_one(
                facts,
                |other| {
                    other.kind() == fact.kind()
                        && other.syntax_kind() == fact.syntax_kind()
                        && other.span() == fact.span()
                },
                cancel,
            )?;
            if count.is_none()
                || raw_one(facts, |other| other.local_id() == fact.local_id(), cancel)?.is_none()
            {
                return Ok(output);
            }
        }
        let ContainerRef::Entity(owner_id) = target.direct_parent else {
            return Ok(output);
        };
        let mut owner = None;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.id == owner_id {
                if owner.is_some() {
                    return Ok(output);
                }
                owner = Some(entity);
            }
        }
        let Some(owner) = owner else {
            return Ok(output);
        };
        if entities
            .get(&module.local_id())
            .is_none_or(|module| module.record.id != owner_id)
        {
            return Ok(output);
        }
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(output);
        };
        if owner.record.kind != EntityKind::Module
            || owner.record.language != "python"
            || owner_source.span() != source.span()
            || !same_source(owner_source, source)
            || owner.record.repository != source.repository()
            || owner.record.generation != source.generation()
            || owner.record.provenance != target.record.provenance
            || owner.definition_local_id.is_some()
            || owner.direct_parent != ContainerRef::File(source.span().file())
            || owner.record.container != Some(owner.direct_parent)
            || target.record.container != Some(target.direct_parent)
        {
            return Ok(output);
        }
        output.binding = Some((
            reference.local_id(),
            site,
            target.record.id,
            owner_id,
            target.record.provenance,
            content_hash(written.as_bytes()),
        ));
        Ok(output)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some((local, site, target, owner, provenance, spelling)) = self.binding else {
            return Ok(None);
        };
        Ok((required(fact) && fact.local_id() == local && physical == site
            && occurrence.role == OccurrenceRole::Reference && occurrence.syntax_kind == "python.module_function.reference"
            && occurrence.source.span() == site && same_source(&occurrence.source, self.source)
            && occurrence.enclosing == Some(owner) && occurrence.provenance == provenance
            && occurrence.repository == self.source.repository() && occurrence.generation == self.source.generation()
            && occurrence.file == self.source.span().file() && occurrence.syntactic_text_hash == spelling
            && occurrence.evidence.source.as_ref() == Some(&occurrence.source)
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { text_hash } if text_hash == spelling))
            .then_some(target))
    }
}
