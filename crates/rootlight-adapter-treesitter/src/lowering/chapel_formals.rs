//! Join a native Chapel formal initializer Read certificate to its exact current procedure.
//! Required full-procedure metadata is not part of symbol identity. Missing,
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
    names: HashMap<(SymbolId, &'a str), Option<(&'a MaterializedEntity, u64)>>,
    parents: HashMap<u64, Option<&'a SyntaxFact>>,
    source: &'a SourceRef,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    let mut selected = right.clone();
    selected = super::source_for_span(&selected, left.span());
    *left == selected
}

fn native_witness_role(fact: &SyntaxFact) -> Option<u8> {
    if fact.kind() == SyntaxFactKind::Declaration {
        return Some(0);
    }
    (fact.kind() == SyntaxFactKind::Occurrence
        && (fact.syntax_kind().as_str().ends_with(".definition")
            || fact.syntax_kind().as_str().ends_with(".definition_part")))
    .then_some(1)
}

/// Reserve a possible RefersTo only for a retained, source-contained certificate.
/// Only a clean native procedure context permits this same-procedure source proof.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "chapel.formal_initializer.reference"
}

impl<'a> ParameterTargets<'a> {
    /// Index exact declaration/Definition pairs after current source projection.
    /// All allocations and native-fact joins remain cancellable and fallible.
    pub(super) fn new<'text>(
        facts: &'a [SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        projected_length: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let projected_end = u64::try_from(projected_length).map_err(|_| SinkError::InvalidIr)?;
        let layout = |value: &str| -> Result<bool, AdapterError> {
            for chunk in value
                .strip_prefix('\u{feff}')
                .unwrap_or(value)
                .as_bytes()
                .chunks(64)
            {
                cancel.check()?;
                if !chunk.iter().all(u8::is_ascii_whitespace) {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let mut by_id = HashMap::new();
        let mut contexts = HashMap::<SourceSpan, Option<u64>>::new();
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
        let mut raw_counts = HashMap::<(SourceSpan, u8), usize>::new();
        let mut raw_signatures = HashMap::<SourceSpan, usize>::new();
        raw_signatures
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        raw_counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Signature {
                raw_signatures
                    .entry(physical(fact.span())?)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
            if let Some(role) = native_witness_role(fact) {
                raw_counts
                    .entry((fact.span(), role))
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
                let suffix =
                    SourceSpan::new(fact.span().file(), fact.span().end_byte(), projected_end)
                        .map_err(|_| SinkError::InvalidIr)?;
                let full_projected = SourceSpan::new(fact.span().file(), 0, projected_end)
                    .map_err(|_| SinkError::InvalidIr)?;
                if fact.syntax_kind().as_str() != "chapel.source_file.root"
                    || fact.parent().is_some()
                    || source.span().start_byte() != 0
                    || physical_root.file() != source.span().file()
                    || physical_root.end_byte() > source.span().end_byte()
                    || physical(full_projected)?.end_byte() != source.span().end_byte()
                    || !layout(text(prefix)?)?
                    || !layout(text(suffix)?)?
                {
                    return Ok(Self {
                        names: HashMap::new(),
                        parents: HashMap::new(),
                        source,
                    });
                }
                roots = roots.saturating_add(1);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "chapel.formal_initializer_context.signature"
            {
                contexts
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let span = physical(fact.span())?;
                contexts
                    .entry(span)
                    .and_modify(|owner| *owner = None)
                    .or_insert(fact.parent());
            }
        }
        let mut names = HashMap::new();
        if roots != 1 {
            return Ok(Self {
                names,
                parents: by_id,
                source,
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
            if parameter.record.language != "chapel"
                || parameter.record.kind != EntityKind::Parameter
            {
                continue;
            }
            let key = (owner_id, parameter.record.canonical_name.as_str());
            // Count every same-owner formal declaration before validation, including a
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
            if owner.record.language != "chapel"
                || owner.record.kind != EntityKind::Function
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || owner.record.provenance != parameter.record.provenance
                || parameter.record.container != Some(parameter.direct_parent)
                || !same_source(owner_source, source)
                || !same_source(parameter_source, source)
                || contexts.get(&owner_source.span()) != Some(&Some(owner_local))
                || raw_signatures.get(&owner_source.span()) != Some(&1)
                || !super::span_contains(owner_source.span(), parameter_source.span())
            {
                continue;
            }
            let Some(declaration) = by_id.get(local).copied().flatten().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "chapel.parameter.declaration"
            }) else {
                continue;
            };
            let Some(owner_declaration) =
                by_id.get(&owner_local).copied().flatten().filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "chapel.function.declaration"
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
            let owns_local = declaration.parent() == Some(owner_local);
            if !owns_local
                || [declaration, definition, owner_declaration, owner_definition]
                    .into_iter()
                    .any(|fact| {
                        native_witness_role(fact)
                            .is_none_or(|role| raw_counts.get(&(fact.span(), role)) != Some(&1))
                    })
            {
                continue;
            }
            if owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "chapel.name.definition"
                || owner_definition.parent() != Some(owner_local)
                || physical(owner_declaration.span())? != owner_source.span()
                || !super::span_contains(owner_source.span(), physical(owner_definition.span())?)
                || text(owner_definition.span())? != owner.record.canonical_name
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "chapel.name.definition"
                || definition.parent() != Some(*local)
                || physical(declaration.span())? != parameter_source.span()
                || !super::span_contains(parameter_source.span(), physical(definition.span())?)
                || text(definition.span())? != parameter.record.canonical_name
                || !parameter.record.canonical_name.is_ascii()
            {
                continue;
            }
            names.insert(key, Some((parameter, owner_local)));
        }
        Ok(Self {
            names,
            parents: by_id,
            source,
        })
    }

    fn owned_scope_chain(
        &self,
        fact: &SyntaxFact,
        owner: u64,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(retained) = self.parents.get(&fact.local_id()).copied().flatten() else {
            return Ok(false);
        };
        if retained != fact {
            return Ok(false);
        }
        Ok(retained.parent() == Some(owner))
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
        if !required(fact)
            || occurrence.syntax_kind != "chapel.formal_initializer.reference"
            || occurrence.role != OccurrenceRole::Read
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.language != "chapel"
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
        let Some((parameter, owner_local)) =
            self.names.get(&(owner.record.id, text)).copied().flatten()
        else {
            return Ok(None);
        };
        let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !self.owned_scope_chain(fact, owner_local, cancel)?
            || parameter.record.provenance != occurrence.provenance
            || parameter_source.span().end_byte() > physical_span.start_byte()
        {
            return Ok(None);
        }
        Ok(Some(parameter.record.id))
    }
}
