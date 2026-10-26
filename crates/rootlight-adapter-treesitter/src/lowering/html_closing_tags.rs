//! Requalify closing tag certificates against the current complete HTML source.
//! Exact native ancestry, opening Definitions and materialized source identities
//! supply the target; an unresolved label never grants spelling-based lookup.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::ParseOptions;

use super::{MaterializedEntity, provider_failure, source_for_span};
use crate::html_closing_tags::Proof;

pub(super) const REFERENCE: &str = "html.closing_tag.reference";
pub(super) const CONTEXT: &str = "html.closing_tag_context.signature";
pub(super) const GAP: &str = "html-closing-tag-source-proof-unavailable";

/// Keep both exact carriers in the native artifact's required partition.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    (fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE)
        || (fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == CONTEXT)
}

/// Join decoded host syntax to its complete physical source and work allowance.
pub(super) struct CurrentSource<'source> {
    pub(super) reference: &'source SourceRef,
    pub(super) logical: &'source [u8],
    pub(super) original: &'source [u8],
    pub(super) complete: bool,
    pub(super) maximum: usize,
}

type Census = ((usize, usize), Vec<Proof>);

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

fn span(file: rootlight_ids::FileId, value: (usize, usize)) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(value.0).map_err(|_| provider_failure("html-closing-span"))?,
        u64::try_from(value.1).map_err(|_| provider_failure("html-closing-span"))?,
    )
    .map_err(|_| provider_failure("html-closing-span"))
}

fn source_census(
    input: &CurrentSource<'_>,
    cancel: &Cancellation,
) -> Result<Option<Census>, AdapterError> {
    cancel.check()?;
    if !input.complete
        || input.maximum == 0
        || input.reference.span().start_byte() != 0
        || u64::try_from(input.original.len()).ok() != Some(input.reference.span().end_byte())
    {
        return Ok(None);
    }
    let mut remaining = input.maximum;
    let mut hash = blake3::Hasher::new();
    for chunk in input.original.chunks(64) {
        cancel.check()?;
        let Some(next) = remaining.checked_sub(1) else {
            return Ok(None);
        };
        remaining = next;
        hash.update(chunk);
    }
    if ContentHash::from_bytes(*hash.finalize().as_bytes()) != input.reference.content_hash() {
        return Ok(None);
    }
    if remaining == 0 {
        return Ok(None);
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_html::LANGUAGE.into())
        .map_err(|_| provider_failure("html-closing-consumer-grammar"))?;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match remaining.checked_sub(1) {
            Some(next) => {
                remaining = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut bytes =
        |offset: usize, _: tree_sitter::Point| input.logical.get(offset..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut bytes,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    if exhausted {
        return Ok(None);
    }
    let Some(tree) = tree else { return Ok(None) };
    let root = tree.root_node();
    let Some(proofs) = crate::html_closing_tags::census(root, input.logical, remaining, cancel)?
    else {
        return Ok(None);
    };
    // Root/module carriers include the admitted BOM in their logical envelope.
    // Element and name proofs retain the native CST byte extents.
    Ok(Some(((0, input.logical.len()), proofs)))
}

struct Binding {
    logical: SourceSpan,
    physical: SourceSpan,
    parent: u64,
    depth: usize,
    target: SymbolId,
    provenance: FactId,
    hash: ContentHash,
}

/// Retain same-pass targets after the independent current-source proof census.
pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    /// Reparse current bytes and require unique, exact native/materialized joins.
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_>,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut result = Self {
            source: current.reference.clone(),
            bindings: HashMap::new(),
        };
        if facts.len() > current.maximum || entities.len() > current.maximum {
            return Ok(result);
        }
        let Some((root_range, proofs)) = source_census(&current, cancel)? else {
            return Ok(result);
        };
        let file = current.reference.span().file();
        let mut all = HashMap::new();
        let mut exact = HashMap::new();
        let mut definitions = HashMap::new();
        let mut entity_ids = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        definitions
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        entity_ids
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            if !fact.syntax_kind().as_str().starts_with("html.") {
                continue;
            }
            if fact.kind() == SyntaxFactKind::ErrorRecovery {
                return Ok(result);
            }
            if fact.span().file() != file {
                return Ok(result);
            }
            unique(
                &mut exact,
                (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                fact,
            );
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
            {
                unique(&mut definitions, fact.span(), fact);
            }
        }
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut entity_ids, entity.record.id, *local);
        }
        if all.values().any(Option::is_none) || exact.values().any(Option::is_none) {
            return Ok(result);
        }
        let lookup = |kind, label, extent| exact.get(&(kind, label, extent)).copied().flatten();
        let root_span = span(file, root_range)?;
        let Some(root) = lookup(SyntaxFactKind::Root, "html.file.root", root_span) else {
            return Ok(result);
        };
        let Some(module) = lookup(SyntaxFactKind::Module, "html.file.module", root_span) else {
            return Ok(result);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || module.parent() != Some(root.local_id())
            || module.depth() != 1
        {
            return Ok(result);
        }
        if facts
            .iter()
            .filter(|fact| {
                matches!(
                    fact.syntax_kind().as_str(),
                    "html.file.root" | "html.file.module"
                )
            })
            .count()
            != 2
        {
            return Ok(result);
        }
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module
                && entity.record.language == "html"
                && entity.record.repository == current.reference.repository()
                && entity.record.generation == current.reference.generation()
                && entity.definition_local_id.is_none()
                && entity.direct_parent == ContainerRef::File(file)
                && entity.record.container == Some(entity.direct_parent)
                && entity_ids.get(&entity.record.id) == Some(&Some(module.local_id()))
        }) else {
            return Ok(result);
        };
        if module_entity.record.evidence.source.as_ref()
            != Some(&source_for_span(
                current.reference,
                physical(module.span())?,
            ))
        {
            return Ok(result);
        }
        let mut by_element = HashMap::new();
        by_element
            .try_reserve(proofs.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for proof in &proofs {
            unique(&mut by_element, proof.element, proof);
        }
        let mut work = current.maximum;
        for proof in &proofs {
            cancel.check()?;
            if proof.opening.0 != proof.element.0
                || proof.closing.1 != proof.element.1
                || proof.opening.1 > proof.closing.0
                || proof.opening_name.0 < proof.opening.0
                || proof.opening_name.1 > proof.opening.1
                || proof.closing_name.0 < proof.closing.0
                || proof.closing_name.1 > proof.closing.1
            {
                continue;
            }
            let mut parent = module;
            let mut owner = module_entity;
            let mut valid = true;
            for extent in proof
                .ancestors
                .iter()
                .rev()
                .chain(std::iter::once(&proof.element))
            {
                cancel.check()?;
                let Some(next) = work.checked_sub(1) else {
                    return Ok(Self {
                        source: result.source,
                        bindings: HashMap::new(),
                    });
                };
                work = next;
                let Some(written) = by_element.get(extent).copied().flatten() else {
                    valid = false;
                    break;
                };
                let element_span = span(file, *extent)?;
                let name_span = span(file, written.opening_name)?;
                let Some(declaration) = lookup(
                    SyntaxFactKind::Declaration,
                    "html.element.declaration",
                    element_span,
                ) else {
                    valid = false;
                    break;
                };
                let Some(definition) = definitions.get(&name_span).copied().flatten() else {
                    valid = false;
                    break;
                };
                let Some(entity) = entities.get(&declaration.local_id()) else {
                    valid = false;
                    break;
                };
                if declaration.parent() != Some(parent.local_id())
                    || declaration.depth() != parent.depth().saturating_add(1)
                    || definition.kind() != SyntaxFactKind::Occurrence
                    || definition.syntax_kind().as_str() != "html.tag_name.definition"
                    || definition.parent() != Some(declaration.local_id())
                    || definition.depth() != declaration.depth().saturating_add(1)
                    || entity.definition_local_id != Some(definition.local_id())
                    || entity.record.kind != EntityKind::MarkupElement
                    || entity.record.language != "html"
                    || entity.record.repository != current.reference.repository()
                    || entity.record.generation != current.reference.generation()
                    || entity.record.provenance != module_entity.record.provenance
                    || entity.record.container != Some(ContainerRef::Entity(owner.record.id))
                    || entity.direct_parent != ContainerRef::Entity(owner.record.id)
                    || entity_ids.get(&entity.record.id) != Some(&Some(declaration.local_id()))
                    || entity.record.evidence.source.as_ref()
                        != Some(&source_for_span(current.reference, physical(element_span)?))
                    || entity.record.canonical_name != text(name_span)?
                    || text(name_span)?.as_bytes()
                        != current
                            .logical
                            .get(written.opening_name.0..written.opening_name.1)
                            .unwrap_or_default()
                {
                    valid = false;
                    break;
                }
                parent = declaration;
                owner = entity;
            }
            if !valid {
                continue;
            }
            let closing_span = span(file, proof.closing_name)?;
            let Some(reference) = lookup(SyntaxFactKind::Occurrence, REFERENCE, closing_span)
            else {
                continue;
            };
            let Some(marker) = lookup(SyntaxFactKind::Signature, CONTEXT, closing_span) else {
                continue;
            };
            if reference.parent() != Some(parent.local_id())
                || marker.parent() != reference.parent()
                || reference.depth() != parent.depth().saturating_add(1)
                || marker.depth() != reference.depth()
                || marker.local_id() == reference.local_id()
                || text(closing_span)?.as_bytes()
                    != current
                        .logical
                        .get(proof.closing_name.0..proof.closing_name.1)
                        .unwrap_or_default()
                || !text(closing_span)?.eq_ignore_ascii_case(owner.record.canonical_name.as_str())
            {
                continue;
            }
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.bindings.insert(
                reference.local_id(),
                Binding {
                    logical: closing_span,
                    physical: physical(closing_span)?,
                    parent: parent.local_id(),
                    depth: reference.depth(),
                    target: owner.record.id,
                    provenance: owner.record.provenance,
                    hash: content_hash(text(closing_span)?.as_bytes()),
                },
            );
        }
        Ok(result)
    }

    /// Resolve only the exact source-owned closing site; propagate cancellation.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        site: &OccurrenceRecord,
        physical: SourceSpan,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(binding) = self.bindings.get(&fact.local_id()) else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != REFERENCE
            || fact.span() != binding.logical
            || fact.parent() != Some(binding.parent)
            || fact.depth() != binding.depth
            || site.syntax_kind != REFERENCE
            || site.role != OccurrenceRole::Reference
            || site.enclosing != Some(binding.target)
            || site.provenance != binding.provenance
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || physical != binding.physical
            || site.source != source_for_span(&self.source, binding.physical)
            || content_hash(text.as_bytes()) != binding.hash
            || site.syntactic_text_hash != binding.hash
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.target))
    }
}
