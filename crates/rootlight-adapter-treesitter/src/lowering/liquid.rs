//! Requalify Liquid loop reads against complete current physical source bytes.
//! Exact native endpoints join unique same-pass Variable definitions; source
//! identity, parent structure and mutation barriers are independent of spelling.

use super::{MaterializedEntity, provider_failure, source_for_span};
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};
use std::{collections::HashMap, ops::ControlFlow};
use tree_sitter::ParseOptions;

#[cfg(test)]
mod tests;

pub(super) const VALUE: &str = "liquid.loop_value.reference";

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence && fact.syntax_kind().as_str() == VALUE
}

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            VALUE | "liquid.runtime_value_unavailable.reference"
        )
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::Variable && label == "liquid.loop_variable.declaration"
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "liquid.source_file.root") => Some(
            "liquid-written-for-items-and-local-value-reads-only-runtime-template-evaluation-unavailable",
        ),
        (SyntaxFactKind::Root, "liquid.native_context_unavailable.root") => {
            Some("liquid-complete-clean-native-source-proof-unavailable")
        }
        (SyntaxFactKind::Signature, "liquid.runtime_construct_unavailable.signature") => Some(
            "liquid-assignment-capture-import-and-other-runtime-construct-semantics-unavailable",
        ),
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}

pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) logical: &'a [u8],
    pub(super) original: &'a [u8],
    pub(super) encoding: &'a str,
    pub(super) complete: bool,
    pub(super) maximum: usize,
}

fn matches_projection(
    input: &CurrentSource<'_>,
    remaining: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if input.encoding == "utf-8" {
        if input.original.len() != input.logical.len() {
            return Ok(false);
        }
        for (raw, logical) in input.original.chunks(64).zip(input.logical.chunks(64)) {
            cancel.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                return Ok(false);
            };
            *remaining = next;
            if raw != logical {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    let little = match input.encoding {
        "utf-16le" if input.original.starts_with(&[0xff, 0xfe]) => true,
        "utf-16be" if input.original.starts_with(&[0xfe, 0xff]) => false,
        _ => return Ok(false),
    };
    if !input.original.len().is_multiple_of(2) {
        return Ok(false);
    }
    let units = input.original.chunks_exact(2).map(|chunk| {
        let pair = [chunk[0], chunk[1]];
        if little {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    let mut offset = 0usize;
    for scalar in char::decode_utf16(units) {
        cancel.check()?;
        let Some(next) = remaining.checked_sub(1) else {
            return Ok(false);
        };
        *remaining = next;
        let Ok(scalar) = scalar else { return Ok(false) };
        let mut bytes = [0; 4];
        let encoded = scalar.encode_utf8(&mut bytes).as_bytes();
        let Some(end) = offset.checked_add(encoded.len()) else {
            return Ok(false);
        };
        if input.logical.get(offset..end) != Some(encoded) {
            return Ok(false);
        }
        offset = end;
    }
    Ok(offset == input.logical.len())
}

struct Probe(Cancellation);
impl tree_sitter_liquid::CancellationProbe for Probe {
    fn is_cancelled(&self) -> bool {
        self.0.check().is_err()
    }
}

fn source_census(
    input: &CurrentSource<'_>,
    cancel: &Cancellation,
) -> Result<Option<crate::liquid_bindings::Census>, AdapterError> {
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
    if ContentHash::from_bytes(*hash.finalize().as_bytes()) != input.reference.content_hash()
        || remaining == 0
    {
        return Ok(None);
    }
    if !matches_projection(input, &mut remaining, cancel)? || remaining == 0 {
        return Ok(None);
    }
    let scope = match tree_sitter_liquid::ScannerScope::begin(
        Probe(cancel.clone()),
        u64::try_from(remaining.min(2_000_000))
            .map_err(|_| provider_failure("liquid-current-source-work"))?,
    ) {
        Ok(scope) => scope,
        Err(status) => {
            cancel.check()?;
            return match status {
                tree_sitter_liquid::ScannerRefusal::OwnerAllocation
                | tree_sitter_liquid::ScannerRefusal::NativeAllocationUnavailable => {
                    Err(SinkError::AllocationFailed.into())
                }
                tree_sitter_liquid::ScannerRefusal::WorkLimit => Ok(None),
                _ => Err(provider_failure("liquid-current-source-scanner")),
            };
        }
    };
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_liquid::LANGUAGE.into())
        .map_err(|_| provider_failure("liquid-current-source-grammar"))?;
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
    drop(parser);
    let scanner = scope.finish();
    cancel.check()?;
    if exhausted {
        return Ok(None);
    }
    match scanner {
        Ok(()) => {}
        Err(tree_sitter_liquid::ScannerRefusal::WorkLimit) => return Ok(None),
        Err(
            tree_sitter_liquid::ScannerRefusal::OwnerAllocation
            | tree_sitter_liquid::ScannerRefusal::NativeAllocationUnavailable,
        ) => return Err(SinkError::AllocationFailed.into()),
        Err(_) => return Err(provider_failure("liquid-current-source-scanner")),
    }
    let Some(tree) = tree else { return Ok(None) };
    crate::liquid_bindings::census(tree.root_node(), input.logical, remaining, true, cancel)
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn span(
    file: rootlight_ids::FileId,
    range: crate::liquid_bindings::Range,
) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(range.0).map_err(|_| provider_failure("liquid-binding-span"))?,
        u64::try_from(range.1).map_err(|_| provider_failure("liquid-binding-span"))?,
    )
    .map_err(|_| provider_failure("liquid-binding-span"))
}

struct Binding {
    target: SymbolId,
    logical: SourceSpan,
    physical: SourceSpan,
    hash: ContentHash,
    provenance: FactId,
    parent: u64,
    depth: usize,
    enclosing: SymbolId,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
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
        let mut result = Self {
            source: current.reference.clone(),
            bindings: HashMap::new(),
        };
        if facts.len() > current.maximum || entities.len() > current.maximum {
            return Ok(result);
        }
        let Some(census) = source_census(&current, cancel)? else {
            return Ok(result);
        };
        let file = current.reference.span().file();
        let mut all = HashMap::new();
        let mut exact = HashMap::new();
        let mut entity_ids = HashMap::new();
        all.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        entity_ids
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            unique(&mut all, fact.local_id(), fact);
            if !fact.syntax_kind().as_str().starts_with("liquid.")
                || fact.kind() == SyntaxFactKind::ErrorRecovery
                || fact.span().file() != file
            {
                return Ok(result);
            }
            unique(
                &mut exact,
                (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                fact,
            );
        }
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut entity_ids, entity.record.id, *local);
        }
        if all.values().any(Option::is_none) || exact.values().any(Option::is_none) {
            return Ok(result);
        }
        let lookup = |kind, label, extent| exact.get(&(kind, label, extent)).copied().flatten();
        let root_span = span(file, (0, current.logical.len()))?;
        let Some(root) = lookup(SyntaxFactKind::Root, "liquid.source_file.root", root_span) else {
            return Ok(result);
        };
        let Some(module) = lookup(SyntaxFactKind::Module, "liquid.file.module", root_span) else {
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
                    "liquid.source_file.root" | "liquid.file.module"
                )
            })
            .count()
            != 2
        {
            return Ok(result);
        }
        let Some(module_entity) = entities.get(&module.local_id()).filter(|entity| {
            entity.record.kind == EntityKind::Module
                && entity.record.language == "liquid"
                && entity.record.repository == current.reference.repository()
                && entity.record.generation == current.reference.generation()
                && entity.direct_parent == ContainerRef::File(file)
                && entity.record.container == Some(entity.direct_parent)
                && entity.definition_local_id.is_none()
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
        let mut definitions = HashMap::new();
        definitions
            .try_reserve(census.definitions.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for proof in census.definitions {
            cancel.check()?;
            let name = span(file, proof.name)?;
            let Some(declaration) = lookup(
                SyntaxFactKind::Declaration,
                "liquid.loop_variable.declaration",
                name,
            ) else {
                continue;
            };
            let Some(definition) = lookup(
                SyntaxFactKind::Occurrence,
                "liquid.loop_name.definition",
                name,
            ) else {
                continue;
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                continue;
            };
            if declaration.parent() != Some(module.local_id())
                || declaration.depth() != 2
                || definition.parent() != Some(declaration.local_id())
                || definition.depth() != 3
                || entity.definition_local_id != Some(definition.local_id())
                || entity.record.kind != EntityKind::Variable
                || entity.record.language != "liquid"
                || entity.record.repository != current.reference.repository()
                || entity.record.generation != current.reference.generation()
                || entity.record.provenance != module_entity.record.provenance
                || entity.direct_parent != ContainerRef::Entity(module_entity.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || entity_ids.get(&entity.record.id) != Some(&Some(declaration.local_id()))
                || entity.record.evidence.source.as_ref()
                    != Some(&source_for_span(current.reference, physical(name)?))
                || entity.record.canonical_name != text(name)?
                || text(name)?.as_bytes()
                    != current
                        .logical
                        .get(proof.name.0..proof.name.1)
                        .unwrap_or_default()
            {
                continue;
            }
            unique(&mut definitions, proof.name, entity);
        }
        for proof in census.reads {
            cancel.check()?;
            let Some(entity) = definitions.get(&proof.definition).copied().flatten() else {
                continue;
            };
            let extent = span(file, proof.name)?;
            let Some(read) = lookup(SyntaxFactKind::Occurrence, VALUE, extent) else {
                continue;
            };
            if read.parent() != Some(module.local_id())
                || read.depth() != 2
                || text(extent)? != entity.record.canonical_name
                || text(extent)?.as_bytes()
                    != current
                        .logical
                        .get(proof.name.0..proof.name.1)
                        .unwrap_or_default()
            {
                continue;
            }
            result
                .bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.bindings.insert(
                read.local_id(),
                Binding {
                    target: entity.record.id,
                    logical: extent,
                    physical: physical(extent)?,
                    hash: content_hash(text(extent)?.as_bytes()),
                    provenance: entity.record.provenance,
                    parent: module.local_id(),
                    depth: read.depth(),
                    enclosing: module_entity.record.id,
                },
            );
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !required(fact)
            || occurrence.syntax_kind != VALUE
            || occurrence.role != OccurrenceRole::Read
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.source.repository() != self.source.repository()
            || occurrence.repository != self.source.repository()
            || occurrence.source.generation() != self.source.generation()
            || occurrence.generation != self.source.generation()
            || occurrence.source.content_hash() != self.source.content_hash()
            || occurrence.source.span().file() != self.source.span().file()
            || occurrence.file != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.logical == fact.span()
                    && binding.physical == physical
                    && physical == occurrence.source.span()
                    && binding.hash == occurrence.syntactic_text_hash
                    && binding.provenance == occurrence.provenance
                    && fact.parent() == Some(binding.parent)
                    && fact.depth() == binding.depth
                    && occurrence.enclosing == Some(binding.enclosing)
            })
            .map(|binding| binding.target))
    }
}
