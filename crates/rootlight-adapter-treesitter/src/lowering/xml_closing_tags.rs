//! Bind physical XML closing names to their own written opening definitions.
//!
//! Requalification uses the complete current CST and exact fact/source ancestry;
//! namespace expansion, DTD validity and entity references remain outside this proof.

use std::{
    collections::{HashMap, HashSet},
    ops::{ControlFlow, Range},
};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};
use tree_sitter::{Node, ParseOptions};

use super::{MaterializedEntity, provider_failure, source_for_span};

pub(super) const REFERENCE: &str = "xml.end_name.reference";

/// Identifies only physical end-name occurrences eligible for source proof.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == REFERENCE
}

/// Complete current bytes and the maximum bounded requalification work.
pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) logical: &'a [u8],
    pub(super) original: &'a [u8],
    pub(super) maximum: usize,
}

struct Element {
    extent: Range<usize>,
    opening: Range<usize>,
    closing: Option<Range<usize>>,
    parent: Option<usize>,
}

fn spend(work: &mut usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    let Some(next) = work.checked_sub(1) else {
        return Ok(false);
    };
    *work = next;
    Ok(true)
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !spend(work, cancel)? {
            return Ok(None);
        }
        if child.kind() == kind && result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

type Names = (Range<usize>, Option<Range<usize>>);

fn names(
    node: Node<'_>,
    source: &[u8],
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Names>, AdapterError> {
    if node.is_missing() || node.has_error() {
        return Ok(None);
    }
    let empty = direct(node, "EmptyElemTag", work, cancel)?;
    let start = direct(node, "STag", work, cancel)?;
    let end = direct(node, "ETag", work, cancel)?;
    let (header, closing) = match (empty, start, end) {
        (Some(empty), None, None) if empty.byte_range() == node.byte_range() => (empty, None),
        (None, Some(start), Some(end))
            if start.start_byte() == node.start_byte()
                && end.end_byte() == node.end_byte()
                && start.end_byte() <= end.start_byte() =>
        {
            (start, Some(end))
        }
        _ => return Ok(None),
    };
    let Some(opening) = direct(header, "Name", work, cancel)? else {
        return Ok(None);
    };
    if opening.is_missing() || opening.has_error() {
        return Ok(None);
    }
    let closing = if let Some(end) = closing {
        let Some(name) = direct(end, "Name", work, cancel)? else {
            return Ok(None);
        };
        if name.is_missing()
            || name.has_error()
            || source.get(opening.byte_range()) != source.get(name.byte_range())
        {
            return Ok(None);
        }
        Some(name.byte_range())
    } else {
        None
    };
    if source.get(opening.byte_range()).is_none() {
        return Ok(None);
    }
    Ok(Some((opening.byte_range(), closing)))
}

fn census(
    current: &CurrentSource<'_>,
    cancel: &Cancellation,
) -> Result<Option<Vec<Element>>, AdapterError> {
    if current.maximum == 0
        || current.reference.span().start_byte() != 0
        || u64::try_from(current.original.len()).ok() != Some(current.reference.span().end_byte())
    {
        return Ok(None);
    }
    let mut work = current.maximum;
    let mut hasher = blake3::Hasher::new();
    for chunk in current.original.chunks(64) {
        if !spend(&mut work, cancel)? {
            return Ok(None);
        }
        hasher.update(chunk);
    }
    if ContentHash::from_bytes(*hasher.finalize().as_bytes()) != current.reference.content_hash() {
        return Ok(None);
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_xml::LANGUAGE_XML.into())
        .map_err(|_| provider_failure("xml-closing-consumer-grammar"))?;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match work.checked_sub(1) {
            Some(next) => {
                work = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut bytes =
        |offset: usize, _: tree_sitter::Point| current.logical.get(offset..).unwrap_or_default();
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
    // Prolog recovery does not invalidate a separate clean element subtree.
    // Every element and its native ancestry still passes the local error guard.
    if root.kind() != "document" || root.is_missing() {
        return Ok(None);
    }
    let mut cursor = root.walk();
    let mut stack = Vec::new();
    let mut elements = Vec::new();
    let mut owner = None;
    loop {
        if !spend(&mut work, cancel)? {
            return Ok(None);
        }
        stack
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        stack.push(owner);
        let node = cursor.node();
        if node.kind() == "element" {
            let Some((opening, closing)) = names(node, current.logical, &mut work, cancel)? else {
                return Ok(None);
            };
            elements
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            elements.push(Element {
                extent: node.byte_range(),
                opening,
                closing,
                parent: owner,
            });
            owner = Some(elements.len() - 1);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            owner = stack
                .pop()
                .ok_or_else(|| provider_failure("xml-closing-native-owner"))?;
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(Some(elements));
            }
        }
    }
}

fn span(file: rootlight_ids::FileId, range: &Range<usize>) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(range.start).map_err(|_| provider_failure("xml-closing-span"))?,
        u64::try_from(range.end).map_err(|_| provider_failure("xml-closing-span"))?,
    )
    .map_err(|_| provider_failure("xml-closing-span"))
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

struct Binding {
    logical: SourceSpan,
    physical: SourceSpan,
    parent: u64,
    depth: usize,
    symbol: SymbolId,
    provenance: FactId,
    spelling: ContentHash,
}

/// Exact source-backed closing sites and their independently proved targets.
pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

impl Targets {
    /// Rechecks current syntax, opening definitions and materialized ownership.
    pub(super) fn new<'a>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_>,
        mut text: impl FnMut(SourceSpan) -> Result<&'a str, AdapterError>,
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
        let Some(elements) = census(&current, cancel)? else {
            return Ok(result);
        };
        let mut exact = HashMap::new();
        let mut ids = HashSet::new();
        let mut symbols = HashSet::new();
        exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        symbols
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let file = current.reference.span().file();
        for fact in facts {
            cancel.check()?;
            if !ids.insert(fact.local_id()) {
                return Ok(result);
            }
            if fact.syntax_kind().as_str().starts_with("xml.") {
                if fact.span().file() != file || fact.kind() == SyntaxFactKind::ErrorRecovery {
                    return Ok(result);
                }
                unique(
                    &mut exact,
                    (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                    fact,
                );
            }
        }
        for entity in entities.values() {
            cancel.check()?;
            if !symbols.insert(entity.record.id) {
                return Ok(result);
            }
        }
        if exact.values().any(Option::is_none) {
            return Ok(result);
        }
        let lookup = |kind, label, extent| exact.get(&(kind, label, extent)).copied().flatten();
        let root_span = span(file, &(0..current.logical.len()))?;
        let Some(root) = lookup(SyntaxFactKind::Root, "xml.file.root", root_span) else {
            return Ok(result);
        };
        let Some(module) = lookup(SyntaxFactKind::Module, "xml.file.module", root_span) else {
            return Ok(result);
        };
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(result);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || module.parent() != Some(root.local_id())
            || module.depth() != 1
            || module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "xml"
            || module_entity.definition_local_id.is_some()
            || module_entity.direct_parent != ContainerRef::File(file)
            || module_entity.record.container != Some(module_entity.direct_parent)
            || module_entity.record.repository != current.reference.repository()
            || module_entity.record.generation != current.reference.generation()
            || module_entity.record.evidence.source.as_ref()
                != Some(&source_for_span(
                    current.reference,
                    physical(module.span())?,
                ))
        {
            return Ok(result);
        }
        let mut owners: Vec<Option<(&SyntaxFact, &MaterializedEntity)>> = Vec::new();
        owners
            .try_reserve(elements.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for element in &elements {
            cancel.check()?;
            let parent = element
                .parent
                .map_or(Some((module, module_entity)), |index| {
                    owners.get(index).copied().flatten()
                });
            let binding = if let Some((parent, owner)) = parent {
                let element_span = span(file, &element.extent)?;
                let opening = span(file, &element.opening)?;
                match (
                    lookup(
                        SyntaxFactKind::Declaration,
                        "xml.element.declaration",
                        element_span,
                    ),
                    lookup(SyntaxFactKind::Occurrence, "xml.name.definition", opening),
                ) {
                    (Some(declaration), Some(definition)) => entities
                        .get(&declaration.local_id())
                        .filter(|entity| {
                            declaration.parent() == Some(parent.local_id())
                                && declaration.depth() == parent.depth().saturating_add(1)
                                && definition.parent() == Some(declaration.local_id())
                                && definition.depth() == declaration.depth().saturating_add(1)
                                && entity.definition_local_id == Some(definition.local_id())
                                && entity.record.kind == EntityKind::MarkupElement
                                && entity.record.language == "xml"
                                && entity.record.repository == current.reference.repository()
                                && entity.record.generation == current.reference.generation()
                                && entity.record.provenance == module_entity.record.provenance
                                && entity.direct_parent == ContainerRef::Entity(owner.record.id)
                                && entity.record.container == Some(entity.direct_parent)
                        })
                        .map(|entity| (declaration, definition, entity, element_span, opening)),
                    _ => None,
                }
            } else {
                None
            };
            let Some((declaration, _, entity, extent, opening)) = binding else {
                owners.push(None);
                continue;
            };
            if entity.record.evidence.source.as_ref()
                != Some(&source_for_span(current.reference, physical(extent)?))
                || entity.record.canonical_name != text(opening)?
                || text(opening)?.as_bytes()
                    != current
                        .logical
                        .get(element.opening.clone())
                        .unwrap_or_default()
            {
                owners.push(None);
                continue;
            }
            owners.push(Some((declaration, entity)));
            let Some(closing) = &element.closing else {
                continue;
            };
            let logical = span(file, closing)?;
            let Some(reference) = lookup(SyntaxFactKind::Occurrence, REFERENCE, logical) else {
                continue;
            };
            if reference.parent() != Some(declaration.local_id())
                || reference.depth() != declaration.depth().saturating_add(1)
                || text(logical)?.as_bytes()
                    != current.logical.get(closing.clone()).unwrap_or_default()
                || text(logical)? != entity.record.canonical_name
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
                    logical,
                    physical: physical(logical)?,
                    parent: declaration.local_id(),
                    depth: reference.depth(),
                    symbol: entity.record.id,
                    provenance: entity.record.provenance,
                    spelling: content_hash(text(logical)?.as_bytes()),
                },
            );
        }
        Ok(result)
    }

    /// Resolves only the unchanged physical closing occurrence; propagates cancellation.
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
        if !required(fact)
            || fact.span() != binding.logical
            || fact.parent() != Some(binding.parent)
            || fact.depth() != binding.depth
            || site.syntax_kind != REFERENCE
            || site.role != OccurrenceRole::Reference
            || site.enclosing != Some(binding.symbol)
            || site.provenance != binding.provenance
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || physical != binding.physical
            || site.source != source_for_span(&self.source, binding.physical)
            || content_hash(text.as_bytes()) != binding.spelling
            || site.syntactic_text_hash != binding.spelling
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
        {
            return Ok(None);
        }
        Ok(Some(binding.symbol))
    }
}
