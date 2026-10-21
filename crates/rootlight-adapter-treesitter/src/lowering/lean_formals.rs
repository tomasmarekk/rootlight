//! Join a retained native Lean formal-read certificate to its exact current owner.
//! Required full-function metadata is not part of symbol identity. Missing,
//! duplicate or foreign declaration/definition witnesses leave an explicit gap.

use std::collections::{HashMap, hash_map::Entry};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) struct ParameterTargets<'a> {
    names: HashMap<(SymbolId, rootlight_ids::ContentHash), Option<(&'a MaterializedEntity, u64)>>,
    source: &'a SourceRef,
    markers: HashMap<SourceSpan, Option<&'a SyntaxFact>>,
    signatures: HashMap<SourceSpan, usize>,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    let mut selected = right.clone();
    selected = super::source_for_span(&selected, left.span());
    *left == selected
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

fn hash(text: &str, cancel: &Cancellation) -> Result<rootlight_ids::ContentHash, AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in text.as_bytes().chunks(64) {
        cancel.check()?;
        hash.update(chunk);
    }
    Ok(rootlight_ids::ContentHash::from_bytes(
        *hash.finalize().as_bytes(),
    ))
}

fn prefix(text: &str, token: &[u8], cancel: &Cancellation) -> Result<bool, AdapterError> {
    let Some(tail) = text.as_bytes().strip_prefix(token) else {
        return Ok(false);
    };
    if token == b"def" && tail.is_empty() {
        return Ok(false);
    }
    for chunk in tail.chunks(64) {
        cancel.check()?;
        if !chunk.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Reserve a possible RefersTo only for a retained, source-contained certificate.
/// Unknown/recovered native output is excluded by the caller on both paths.
pub(super) fn certified_reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "lean.formal_body.reference"
}

impl<'a> ParameterTargets<'a> {
    /// Index exact declaration/Definition pairs after current source projection.
    /// All allocations and native-fact joins remain cancellable and fallible.
    pub(super) fn new<'text>(
        facts: &'a [SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut by_id = HashMap::new();
        let mut contexts = HashMap::<SourceSpan, Option<&SyntaxFact>>::new();
        let mut markers = HashMap::<SourceSpan, Option<&SyntaxFact>>::new();
        markers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut signatures = HashMap::<SourceSpan, usize>::new();
        signatures
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut witnesses = HashMap::<(SourceSpan, &'static str), u8>::new();
        witnesses
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut owners = HashMap::<SymbolId, Option<(u64, &MaterializedEntity)>>::new();
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            owners
                .entry(entity.record.id)
                .and_modify(|entry| *entry = None)
                .or_insert(Some((*local, entity)));
        }
        let mut roots = 0_usize;
        let mut module = None;
        let mut file_scope = None;
        let mut root_id = None;
        let mut recovered = false;
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Signature {
                let span = physical(fact.span())?;
                signatures.entry(span).and_modify(|n| *n = 2).or_insert(1);
                if fact.syntax_kind().as_str() == "lean.formal_body_target.signature" {
                    markers
                        .entry(span)
                        .and_modify(|entry| *entry = None)
                        .or_insert(Some(fact));
                }
            }
            let witness = match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Declaration, _) => Some("declaration"),
                (SyntaxFactKind::Occurrence, label)
                    if label.ends_with(".definition") || label.ends_with(".definition_part") =>
                {
                    Some("definition")
                }
                _ => None,
            };
            if let Some(kind) = witness {
                // Native witness ambiguity must survive materialized capture deduplication.
                witnesses
                    .entry((physical(fact.span())?, kind))
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
            by_id
                .entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            if fact.kind() == SyntaxFactKind::Root {
                let prefix = SourceSpan::new(fact.span().file(), 0, fact.span().start_byte())
                    .map_err(|_| SinkError::InvalidIr)?;
                let physical_root = physical(fact.span())?;
                if fact.syntax_kind().as_str() != "lean.file.root"
                    || fact.parent().is_some()
                    || source.span().start_byte() != 0
                    || physical_root.file() != source.span().file()
                    || physical_root != source.span()
                    || !matches!(text(prefix)?, "" | "\u{feff}")
                {
                    return Ok(Self {
                        names: HashMap::new(),
                        source,
                        markers,
                        signatures,
                    });
                }
                roots = roots.saturating_add(1);
                root_id = Some(fact.local_id());
            }
            if fact.kind() == SyntaxFactKind::Module
                && (module.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "lean.file_context.module"
                    || physical(fact.span())? != source.span())
            {
                roots = 2;
            }
            if fact.kind() == SyntaxFactKind::Scope
                && physical(fact.span())? == source.span()
                && (file_scope.replace(fact).is_some()
                    || fact.syntax_kind().as_str() != "lean.file_context.scope")
            {
                roots = 2;
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "lean.formal_binding_context.signature"
            {
                contexts
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let span = physical(fact.span())?;
                contexts
                    .entry(span)
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(fact));
            }
        }
        let mut names = HashMap::new();
        let mut ids_valid = true;
        for value in by_id.values() {
            cancel.check()?;
            ids_valid &= value.is_some();
        }
        let complete_file = match (root_id, module, file_scope) {
            (Some(root), Some(module), Some(scope)) => {
                module.parent() == Some(root)
                    && matches!(scope.parent(), Some(parent) if parent == root || parent == module.local_id())
            }
            _ => false,
        };
        if roots != 1 || recovered || !ids_valid || !complete_file {
            return Ok(Self {
                names,
                source,
                markers,
                signatures,
            });
        }
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, parameter) in entities {
            cancel.check()?;
            let ContainerRef::Entity(owner_id) = parameter.direct_parent else {
                continue;
            };
            if parameter.record.language != "lean" || parameter.record.kind != EntityKind::Parameter
            {
                continue;
            }
            let key = (owner_id, hash(&parameter.record.canonical_name, cancel)?);
            // Count every same-owner declaration before validation, including a
            // malformed competitor; it must not disappear from ambiguity checks.
            match names.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry.insert(None);
                    continue;
                }
                Entry::Vacant(entry) => {
                    entry.insert(None);
                }
            }
            let Some((owner_local, owner)) = owners.get(&owner_id).copied().flatten() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(context) = contexts.get(&owner_source.span()).copied().flatten() else {
                continue;
            };
            if owner.record.language != "lean"
                || owner.record.kind != EntityKind::Function
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || owner.record.provenance != parameter.record.provenance
                || parameter.record.container != Some(parameter.direct_parent)
                || !same_source(owner_source, source)
                || !same_source(parameter_source, source)
                || context.parent() != Some(owner_local)
                || owner.direct_parent != ContainerRef::File(source.span().file())
                || owner.record.container != Some(owner.direct_parent)
                || signatures.get(&owner_source.span()) != Some(&1)
                || !super::span_contains(owner_source.span(), parameter_source.span())
            {
                continue;
            }
            let Some(declaration) = by_id.get(local).copied().flatten().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "lean.parameter.declaration"
            }) else {
                continue;
            };
            let Some(owner_declaration) =
                by_id.get(&owner_local).copied().flatten().filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "lean.function.declaration"
                })
            else {
                continue;
            };
            let Some(owner_definition) = owner
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let Some(definition) = parameter
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            if owner_definition.span().start_byte() < owner_declaration.span().start_byte()
                || definition.span().start_byte() < declaration.span().start_byte()
            {
                continue;
            }
            let owner_prefix = SourceSpan::new(
                owner_declaration.span().file(),
                owner_declaration.span().start_byte(),
                owner_definition.span().start_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            let parameter_prefix = SourceSpan::new(
                declaration.span().file(),
                declaration.span().start_byte(),
                definition.span().start_byte(),
            )
            .map_err(|_| SinkError::InvalidIr)?;
            if witnesses.get(&(physical(owner_declaration.span())?, "declaration")) != Some(&1)
                || witnesses.get(&(physical(declaration.span())?, "declaration")) != Some(&1)
                || witnesses.get(&(physical(owner_definition.span())?, "definition")) != Some(&1)
                || witnesses.get(&(physical(definition.span())?, "definition")) != Some(&1)
                || owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "lean.name.definition"
                || owner_definition.parent() != Some(owner_local)
                || owner_declaration.parent() != file_scope.map(SyntaxFact::local_id)
                || declaration.parent() != Some(owner_local)
                || !prefix(text(owner_prefix)?, b"def", cancel)?
                || !prefix(text(parameter_prefix)?, b"(", cancel)?
                || physical(owner_declaration.span())? != owner_source.span()
                || !super::span_contains(owner_source.span(), physical(owner_definition.span())?)
                || !equal(
                    text(owner_definition.span())?,
                    &owner.record.canonical_name,
                    cancel,
                )?
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "lean.name.definition"
                || definition.parent() != Some(*local)
                || physical(declaration.span())? != parameter_source.span()
                || !super::span_contains(parameter_source.span(), physical(definition.span())?)
                || !equal(
                    text(definition.span())?,
                    &parameter.record.canonical_name,
                    cancel,
                )?
            {
                continue;
            }
            names.insert(key, Some((parameter, owner_local)));
        }
        Ok(Self {
            names,
            source,
            markers,
            signatures,
        })
    }

    /// Resolve only the native certificate against a unique same-owner formal.
    /// Physical spans come from the existing SourceCoordinates projection.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner else { return Ok(None) };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !certified_reference(fact)
            || occurrence.syntax_kind != "lean.formal_body.reference"
            || occurrence.role != OccurrenceRole::Read
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.language != "lean"
            || owner.record.kind != EntityKind::Function
            || owner.record.repository != self.source.repository()
            || owner.record.generation != self.source.generation()
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.provenance != owner.record.provenance
            || !same_source(&occurrence.source, self.source)
            || !same_source(owner_source, self.source)
            || occurrence.source.span() != physical_span
            || !super::span_contains(owner_source.span(), physical_span)
            || occurrence.syntactic_text_hash != hash(text, cancel)?
            || physical_span.end_byte() != owner_source.span().end_byte()
        {
            return Ok(None);
        }
        let Some((parameter, owner_local)) = self
            .names
            .get(&(owner.record.id, hash(text, cancel)?))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        if fact.parent() != Some(owner_local)
            || !equal(text, &parameter.record.canonical_name, cancel)?
        {
            return Ok(None);
        }
        let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let window = SourceSpan::new(
            physical_span.file(),
            parameter_source.span().start_byte(),
            physical_span.end_byte(),
        )
        .map_err(|_| SinkError::InvalidIr)?;
        let Some(marker) = self.markers.get(&window).copied().flatten() else {
            return Ok(None);
        };
        if self.signatures.get(&window) != Some(&1) || marker.parent() != fact.parent() {
            return Ok(None);
        }
        if parameter.record.provenance != occurrence.provenance
            || parameter_source.span().end_byte() > physical_span.start_byte()
        {
            return Ok(None);
        }
        Ok(Some(parameter.record.id))
    }
}
