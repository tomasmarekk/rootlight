//! Join native C variable target windows to exact current declaration witnesses.
//! Coordinate certificates select targets; spelling alone cannot supply one.
//! Duplicate raw witnesses remain ambiguous even if materialization coalesces them.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "c.written_variable.reference"
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

pub(super) struct VariableTargets<'a> {
    variables: HashMap<u64, Option<&'a MaterializedEntity>>,
    owners: HashMap<SymbolId, Option<&'a MaterializedEntity>>,
    targets: HashMap<u64, Option<u64>>,
    source: &'a SourceRef,
}

impl<'a> VariableTargets<'a> {
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut output = Self {
            variables: HashMap::new(),
            owners: HashMap::new(),
            targets: HashMap::new(),
            source,
        };
        let mut by_id = HashMap::new();
        let mut raw_counts = HashMap::new();
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        raw_counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut roots = 0_usize;
        let mut contexts = 0_usize;
        let mut valid_root = true;
        for fact in facts {
            cancel.check()?;
            by_id
                .entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            raw_counts
                .entry((fact.kind(), fact.syntax_kind().as_str(), fact.span()))
                .and_modify(|count| *count = 2_usize)
                .or_insert(1);
            let span = physical(fact.span())?;
            if fact.kind() == SyntaxFactKind::Root {
                roots = roots.saturating_add(1);
                valid_root &= fact.syntax_kind().as_str() == "c.file.root"
                    && fact.parent().is_none()
                    && span == source.span();
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "c.variable_file_context.signature"
            {
                contexts = contexts.saturating_add(1);
                valid_root &= span == source.span();
            }
        }
        if roots != 1
            || contexts != 1
            || !valid_root
            || source.span().start_byte() != 0
            || by_id.values().any(Option::is_none)
        {
            return Ok(output);
        }
        output
            .variables
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        output
            .owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "c"
                || !matches!(
                    entity.record.kind,
                    EntityKind::Variable | EntityKind::Function
                )
            {
                continue;
            }
            let Some(entity_source) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(declaration) = by_id.get(local).copied().flatten() else {
                continue;
            };
            let Some(definition) = entity
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let (decl_label, def_label) = if entity.record.kind == EntityKind::Variable {
                ("c.variable.declaration", "c.variable_name.definition")
            } else {
                ("c.function.declaration", "c.identifier.definition")
            };
            let valid = declaration.kind() == SyntaxFactKind::Declaration
                && declaration.syntax_kind().as_str() == decl_label
                && definition.kind() == SyntaxFactKind::Occurrence
                && definition.syntax_kind().as_str() == def_label
                && raw_counts.get(&(declaration.kind(), decl_label, declaration.span()))
                    == Some(&1)
                && raw_counts.get(&(definition.kind(), def_label, definition.span())) == Some(&1)
                && definition.parent() == Some(*local)
                && physical(declaration.span())? == entity_source.span()
                && super::span_contains(entity_source.span(), physical(definition.span())?)
                && text(definition.span())? == entity.record.canonical_name
                && entity.record.canonical_name.is_ascii()
                && same_source(entity_source, source)
                && entity_source.span().file() == source.span().file()
                && super::span_contains(source.span(), entity_source.span())
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity.record.container == Some(entity.direct_parent);
            if entity.record.kind == EntityKind::Variable {
                output
                    .variables
                    .entry(entity_source.span().start_byte())
                    .and_modify(|entry| *entry = None)
                    .or_insert(valid.then_some(entity));
            } else {
                output
                    .owners
                    .entry(entity.record.id)
                    .and_modify(|entry| *entry = None)
                    .or_insert(valid.then_some(entity));
            }
        }
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "c.variable_target.signature"
            {
                let span = physical(fact.span())?;
                output
                    .targets
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let unique = by_id.get(&fact.local_id()).copied().flatten().is_some()
                    && raw_counts.get(&(fact.kind(), fact.syntax_kind().as_str(), fact.span()))
                        == Some(&1)
                    && span.file() == source.span().file()
                    && super::span_contains(source.span(), span);
                output
                    .targets
                    .entry(span.end_byte())
                    .and_modify(|entry| *entry = None)
                    .or_insert(unique.then_some(span.start_byte()));
            }
        }
        Ok(output)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        span: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        written: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner else { return Ok(None) };
        if !required(fact)
            || occurrence.role != OccurrenceRole::Read
            || occurrence.syntax_kind != "c.written_variable.reference"
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || self
                .owners
                .get(&owner.record.id)
                .copied()
                .flatten()
                .is_none()
            || occurrence.enclosing != Some(owner.record.id)
            || occurrence.provenance != owner.record.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != span
            || span.file() != self.source.span().file()
            || !same_source(&occurrence.source, self.source)
            || occurrence.syntactic_text_hash != content_hash(written.as_bytes())
        {
            return Ok(None);
        }
        let Some(start) = self.targets.get(&span.end_byte()).copied().flatten() else {
            return Ok(None);
        };
        let Some(variable) = self.variables.get(&start).copied().flatten() else {
            return Ok(None);
        };
        let Some(variable_source) = variable.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if variable.record.canonical_name != written
            || variable.record.provenance != occurrence.provenance
            || variable_source.span().end_byte() > span.start_byte()
            || !super::span_contains(owner_source.span(), span)
            || !matches!(variable.direct_parent, ContainerRef::File(file) if file == self.source.span().file())
                && variable.direct_parent != ContainerRef::Entity(owner.record.id)
        {
            return Ok(None);
        }
        Ok(Some(variable.record.id))
    }
}
