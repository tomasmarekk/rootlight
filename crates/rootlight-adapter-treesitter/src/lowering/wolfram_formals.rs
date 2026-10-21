//! Join a retained native Wolfram formal-read certificate to its exact current owner.
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
    names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>>,
    source: &'a SourceRef,
    markers: HashMap<SourceSpan, Option<&'a SyntaxFact>>,
    signatures: HashMap<SourceSpan, usize>,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    let mut selected = right.clone();
    selected = super::source_for_span(&selected, left.span());
    *left == selected
}

/// Reserve a possible RefersTo only for a retained, source-contained certificate.
/// Unknown/recovered native output is excluded by the caller on both paths.
pub(super) fn certified_reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "wolfram.formal_argument.reference"
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
        let mut contexts = HashMap::<SourceSpan, usize>::new();
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
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Signature {
                let span = physical(fact.span())?;
                signatures.entry(span).and_modify(|n| *n = 2).or_insert(1);
                if fact.syntax_kind().as_str() == "wolfram.formal_target_context.signature" {
                    markers
                        .entry(span)
                        .and_modify(|entry| *entry = None)
                        .or_insert(Some(fact));
                }
            }
            let witness = match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Declaration, "wolfram.function.declaration") => Some("function"),
                (SyntaxFactKind::Declaration, "wolfram.parameter.declaration") => Some("parameter"),
                (SyntaxFactKind::Occurrence, "wolfram.name.definition") => Some("definition"),
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
                if fact.syntax_kind().as_str() != "wolfram.file.root"
                    || fact.parent().is_some()
                    || source.span().start_byte() != 0
                    || physical_root.file() != source.span().file()
                    || physical_root.end_byte() != source.span().end_byte()
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
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "wolfram.formal_binding_context.signature"
            {
                contexts
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let span = physical(fact.span())?;
                contexts
                    .entry(span)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
        }
        let mut names = HashMap::new();
        if roots != 1 {
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
            if parameter.record.language != "wolfram"
                || parameter.record.kind != EntityKind::Parameter
            {
                continue;
            }
            let key = (owner_id, parameter.record.canonical_name.as_str());
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
            if owner.record.language != "wolfram"
                || owner.record.kind != EntityKind::Function
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || owner.record.provenance != parameter.record.provenance
                || parameter.record.container != Some(parameter.direct_parent)
                || !same_source(owner_source, source)
                || !same_source(parameter_source, source)
                || contexts.get(&owner_source.span()) != Some(&1)
                || signatures.get(&owner_source.span()) != Some(&1)
                || !super::span_contains(owner_source.span(), parameter_source.span())
            {
                continue;
            }
            let Some(declaration) = by_id.get(local).copied().flatten().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "wolfram.parameter.declaration"
            }) else {
                continue;
            };
            let Some(owner_declaration) =
                by_id.get(&owner_local).copied().flatten().filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "wolfram.function.declaration"
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
            if witnesses.get(&(physical(owner_declaration.span())?, "function")) != Some(&1)
                || witnesses.get(&(physical(declaration.span())?, "parameter")) != Some(&1)
                || witnesses.get(&(physical(owner_definition.span())?, "definition")) != Some(&1)
                || witnesses.get(&(physical(definition.span())?, "definition")) != Some(&1)
                || owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "wolfram.name.definition"
                || owner_definition.parent() != Some(owner_local)
                || physical(owner_declaration.span())? != owner_source.span()
                || !super::span_contains(owner_source.span(), physical(owner_definition.span())?)
                || text(owner_definition.span())? != owner.record.canonical_name
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "wolfram.name.definition"
                || definition.parent() != Some(*local)
                || physical(declaration.span())? != parameter_source.span()
                || !super::span_contains(parameter_source.span(), physical(definition.span())?)
                || text(definition.span())? != parameter.record.canonical_name
                || !parameter.record.canonical_name.is_ascii()
            {
                continue;
            }
            names.insert(key, Some(parameter));
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
            || occurrence.syntax_kind != "wolfram.formal_argument.reference"
            || occurrence.role != OccurrenceRole::Read
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.language != "wolfram"
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
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
        {
            return Ok(None);
        }
        let Some(parameter) = self.names.get(&(owner.record.id, text)).copied().flatten() else {
            return Ok(None);
        };
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
