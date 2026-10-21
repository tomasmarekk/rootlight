//! Join a native literal-value certificate to its current written formal.
//! Hashtable/property containers remain the occurrence's source owners;
//! only their exact raw chain may reach the certified function declaration.

use std::collections::{HashMap, hash_map::Entry};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

type Target<'a> = (&'a MaterializedEntity, &'a MaterializedEntity);

pub(super) struct ParameterTargets<'a> {
    facts: HashMap<u64, Option<&'a SyntaxFact>>,
    witnesses: HashMap<(SourceSpan, u8), u8>,
    names: HashMap<(u64, [u8; 32]), Option<Target<'a>>>,
    entities: &'a HashMap<u64, MaterializedEntity>,
    source: &'a SourceRef,
}

fn same_source(selected: &SourceRef, full: &SourceRef) -> bool {
    *selected == super::source_for_span(full, selected.span())
}

fn name_key(name: &str, cancel: &Cancellation) -> Result<[u8; 32], AdapterError> {
    let mut hash = blake3::Hasher::new();
    for chunk in name.as_bytes().chunks(64) {
        cancel.check()?;
        let mut folded = [0_u8; 64];
        for (target, byte) in folded.iter_mut().zip(chunk) {
            *target = byte.to_ascii_lowercase();
        }
        hash.update(&folded[..chunk.len()]);
    }
    Ok(*hash.finalize().as_bytes())
}

fn witness_role(fact: &SyntaxFact) -> Option<u8> {
    match fact.kind() {
        SyntaxFactKind::Declaration => Some(0),
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str().ends_with(".definition")
                || fact.syntax_kind().as_str().ends_with(".definition_part") =>
        {
            Some(1)
        }
        SyntaxFactKind::Scope => Some(2),
        SyntaxFactKind::Root => Some(3),
        SyntaxFactKind::Module => Some(4),
        _ => None,
    }
}

/// Reserve the exact additional RefersTo for the retained native certificate.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "powershell.formal_value.reference"
}

impl<'a> ParameterTargets<'a> {
    /// Retain unique raw witnesses and physical, same-generation formal targets.
    pub(super) fn new<'text>(
        facts: &'a [SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        projected_length: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            facts: HashMap::new(),
            witnesses: HashMap::new(),
            names: HashMap::new(),
            entities,
            source,
        };
        result
            .facts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut counts = HashMap::<(SourceSpan, u8), u8>::new();
        let mut contexts = HashMap::<SourceSpan, Option<u64>>::new();
        counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        contexts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut root_count = 0_usize;
        for fact in facts {
            cancel.check()?;
            result
                .facts
                .entry(fact.local_id())
                .and_modify(|slot| *slot = None)
                .or_insert(Some(fact));
            if let Some(role) = witness_role(fact) {
                counts
                    .entry((fact.span(), role))
                    .and_modify(|count| *count = count.saturating_add(1).min(3))
                    .or_insert(1);
            }
            if fact.kind() == SyntaxFactKind::Root {
                root_count = root_count.saturating_add(1);
                root = Some(fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "powershell.formal_context.signature"
            {
                contexts
                    .entry(physical(fact.span())?)
                    .and_modify(|slot| *slot = None)
                    .or_insert(fact.parent());
            }
        }
        let Some(root) = root.filter(|root| {
            root_count == 1
                && root.syntax_kind().as_str() == "powershell.file.root"
                && root.parent().is_none()
        }) else {
            return Ok(result);
        };
        let end = u64::try_from(projected_length).map_err(|_| SinkError::InvalidIr)?;
        if root.span().start_byte() != 0
            || root.span().end_byte() != end
            || physical(root.span())? != source.span()
            || source.span().start_byte() != 0
        {
            return Ok(result);
        }
        result.witnesses = counts;
        for (local, parameter) in entities {
            cancel.check()?;
            if parameter.record.language != "powershell"
                || parameter.record.kind != EntityKind::Parameter
            {
                continue;
            }
            let Some(declaration) = result.fact(*local) else {
                continue;
            };
            let Some(owner_local) = declaration.parent() else {
                continue;
            };
            let key = (
                owner_local,
                name_key(&parameter.record.canonical_name, cancel)?,
            );
            match result.names.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry.insert(None);
                    continue;
                }
                Entry::Vacant(entry) => {
                    entry.insert(None);
                }
            }
            let Some(owner) = entities.get(&owner_local) else {
                continue;
            };
            let Some(owner_fact) = result.fact(owner_local) else {
                continue;
            };
            let Some(owner_scope) = owner_fact.parent().and_then(|id| result.fact(id)) else {
                continue;
            };
            let Some(file_scope) = owner_scope.parent().and_then(|id| result.fact(id)) else {
                continue;
            };
            let Some(module) = file_scope.parent().and_then(|id| result.fact(id)) else {
                continue;
            };
            let Some(definition) = parameter.definition_local_id.and_then(|id| result.fact(id))
            else {
                continue;
            };
            let Some(owner_definition) = owner.definition_local_id.and_then(|id| result.fact(id))
            else {
                continue;
            };
            let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "powershell.parameter.declaration"
                || owner_fact.kind() != SyntaxFactKind::Declaration
                || owner_fact.syntax_kind().as_str() != "powershell.function.declaration"
                || owner_scope.kind() != SyntaxFactKind::Scope
                || owner_scope.syntax_kind().as_str() != "powershell.function.scope"
                || owner_scope.span() != owner_fact.span()
                || file_scope.kind() != SyntaxFactKind::Scope
                || file_scope.syntax_kind().as_str() != "powershell.file.scope"
                || file_scope.span() != root.span()
                || module.kind() != SyntaxFactKind::Module
                || module.syntax_kind().as_str() != "powershell.file.module"
                || module.span() != root.span()
                || module.parent() != Some(root.local_id())
                || owner.record.language != "powershell"
                || owner.record.kind != EntityKind::Function
                || parameter.direct_parent != ContainerRef::Entity(owner.record.id)
                || parameter.record.container != Some(parameter.direct_parent)
                || parameter.record.provenance != owner.record.provenance
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || !same_source(parameter_source, source)
                || !same_source(owner_source, source)
                || physical(declaration.span())? != parameter_source.span()
                || physical(owner_fact.span())? != owner_source.span()
                || !super::span_contains(owner_source.span(), parameter_source.span())
                || contexts.get(&owner_source.span()) != Some(&Some(owner_local))
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "powershell.identifier.definition"
                || definition.parent() != Some(*local)
                || owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "powershell.identifier.definition"
                || owner_definition.parent() != Some(owner_local)
                || !super::span_contains(declaration.span(), definition.span())
                || !super::span_contains(owner_fact.span(), owner_definition.span())
                || text(definition.span())? != parameter.record.canonical_name
                || text(owner_definition.span())? != owner.record.canonical_name
                || !parameter.record.canonical_name.is_ascii()
                || [
                    declaration,
                    definition,
                    owner_fact,
                    owner_definition,
                    root,
                    module,
                ]
                .into_iter()
                .any(|fact| {
                    witness_role(fact)
                        .is_none_or(|role| result.witnesses.get(&(fact.span(), role)) != Some(&1))
                })
                || result.witnesses.get(&(owner_scope.span(), 2))
                    != Some(&if owner_scope.span() == root.span() {
                        2
                    } else {
                        1
                    })
                || result.witnesses.get(&(file_scope.span(), 2))
                    != Some(&if owner_scope.span() == root.span() {
                        2
                    } else {
                        1
                    })
            {
                continue;
            }
            result.names.insert(key, Some((parameter, owner)));
        }
        Ok(result)
    }

    fn fact(&self, local: u64) -> Option<&'a SyntaxFact> {
        self.facts.get(&local).copied().flatten()
    }

    fn function_owner(
        &self,
        fact: &SyntaxFact,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        if self.fact(fact.local_id()) != Some(fact) {
            return Ok(None);
        }
        let mut current = fact;
        let mut enclosing = occurrence.enclosing;
        let mut expect_property = true;
        let mut completed_pair = false;
        for _ in 0..self.facts.len() {
            cancel.check()?;
            let Some(parent) = current.parent().and_then(|id| self.fact(id)) else {
                return Ok(None);
            };
            let Some(entity) = self.entities.get(&parent.local_id()) else {
                return Ok(None);
            };
            let Some(source) = entity.record.evidence.source.as_ref() else {
                return Ok(None);
            };
            if entity.record.language != "powershell"
                || entity.record.repository != self.source.repository()
                || entity.record.generation != self.source.generation()
                || entity.record.provenance != occurrence.provenance
                || !same_source(source, self.source)
                || enclosing != Some(entity.record.id)
                || !super::span_contains(parent.span(), current.span())
            {
                return Ok(None);
            }
            if parent.kind() == SyntaxFactKind::Declaration
                && parent.syntax_kind().as_str() == "powershell.function.declaration"
                && entity.record.kind == EntityKind::Function
            {
                return Ok((expect_property && completed_pair).then_some(parent.local_id()));
            }
            let label = match (
                parent.kind(),
                parent.syntax_kind().as_str(),
                entity.record.kind,
            ) {
                (
                    SyntaxFactKind::Declaration,
                    "powershell.property.declaration",
                    EntityKind::Property,
                ) if expect_property => {
                    expect_property = false;
                    "powershell.property.scope"
                }
                (
                    SyntaxFactKind::Declaration,
                    "powershell.hashtable.declaration",
                    EntityKind::Namespace,
                ) if !expect_property => {
                    expect_property = true;
                    completed_pair = true;
                    "powershell.hashtable.scope"
                }
                _ => return Ok(None),
            };
            let Some(scope) = parent.parent().and_then(|id| self.fact(id)) else {
                return Ok(None);
            };
            if scope.kind() != SyntaxFactKind::Scope
                || scope.syntax_kind().as_str() != label
                || scope.span() != parent.span()
                || entity.record.container != Some(entity.direct_parent)
                || self.witnesses.get(&(parent.span(), 0)) != Some(&1)
                || self.witnesses.get(&(scope.span(), 2)) != Some(&1)
            {
                return Ok(None);
            }
            let ContainerRef::Entity(next_owner) = entity.direct_parent else {
                return Ok(None);
            };
            enclosing = Some(next_owner);
            current = scope;
        }
        Ok(None)
    }

    /// Resolve only a current certified value while preserving its property owner.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        occurrence: &OccurrenceRecord,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !required(fact)
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.syntax_kind != "powershell.formal_value.reference"
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || !same_source(&occurrence.source, self.source)
            || occurrence.source.span() != physical_span
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
            || !text.is_ascii()
        {
            return Ok(None);
        }
        let Some(owner_local) = self.function_owner(fact, occurrence, cancel)? else {
            return Ok(None);
        };
        let Some((parameter, owner)) = self
            .names
            .get(&(owner_local, name_key(text, cancel)?))
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if parameter_source.span().end_byte() > physical_span.start_byte()
            || !super::span_contains(owner_source.span(), physical_span)
        {
            return Ok(None);
        }
        Ok(Some(parameter.record.id))
    }
}
