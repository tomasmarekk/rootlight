//! Exact input Reads use native-certified function scope and same-frame formals.
//! General Modelica class, equation, import, type and runtime binding remain gaps.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "modelica.function.declaration")
            | (
                EntityKind::Parameter,
                "modelica.input_parameter.declaration"
            )
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "modelica.header.signature"
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "modelica.file.module"
    {
        &[
            (
                FactDomain::Entities,
                "modelica-model-class-package-member-output-and-general-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "modelica-qualified-array-type-loop-and-general-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "modelica-import-inheritance-equation-unit-event-type-and-runtime-binding-unavailable",
            ),
        ]
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "modelica.unsupported.expression"
    {
        &[(
            FactDomain::Relations,
            "modelica-source-construct-semantics-unavailable",
        )]
    } else {
        &[]
    };
    rows.iter().copied()
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "modelica.input_read.reference" | "modelica.unresolved_read.reference",
        ) => Some(OccurrenceRole::Read),
        (SyntaxFactKind::Comment, "modelica.comment.comment") => {
            Some(OccurrenceRole::Documentation)
        }
        (SyntaxFactKind::StringLiteral, "modelica.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) struct Inputs<'a> {
    names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>>,
}

impl<'a> Inputs<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut owners = HashSet::new();
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "modelica" && entity.record.kind == EntityKind::Function {
                owners.insert(entity.record.id);
            }
        }
        let mut names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>> =
            HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language != "modelica" || entity.record.kind != EntityKind::Parameter {
                continue;
            }
            let ContainerRef::Entity(owner) = entity.direct_parent else {
                continue;
            };
            if !owners.contains(&owner) {
                continue;
            }
            names
                .entry((owner, entity.record.canonical_name.as_str()))
                .and_modify(|value| *value = None)
                .or_insert(Some(entity));
        }
        Ok(Self { names })
    }

    /// Resolve only the native-certified input of this exact written function.
    /// Allocation is admitted at construction; cancellation and source identity remain mandatory.
    pub(super) fn resolve(
        &self,
        name: &str,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "modelica" && owner.record.kind == EntityKind::Function
        }) else {
            return Ok(None);
        };
        if site.role != OccurrenceRole::Read
            || site.syntax_kind != "modelica.input_read.reference"
            || site.enclosing != Some(owner.record.id)
        {
            return Ok(None);
        }
        let Some(target) = self.names.get(&(owner.record.id, name)).copied().flatten() else {
            return Ok(None);
        };
        let Some(source) = target.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        Ok((source.repository() == site.source.repository()
            && source.generation() == site.source.generation()
            && source.span().file() == site.source.span().file()
            && source.content_hash() == site.source.content_hash()
            && source.span().end_byte() <= site.source.span().start_byte())
        .then_some(target.record.id))
    }
}
