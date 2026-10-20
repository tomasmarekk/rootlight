//! Resolve unique same-file written Nim call names through lexical owner chains.
//! Style comparison is separate from source spelling; overload selection stays a gap.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

fn comparison_key(name: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(name.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for (index, character) in name.chars().enumerate() {
        if index == 0 {
            result.push(character);
        } else if character != '_' {
            result.push(character.to_ascii_lowercase());
        }
    }
    Ok(result)
}

/// Index admitted lexical owners, retaining ambiguity and foreign-binding barriers.
pub(super) struct CallTargets {
    names: HashMap<String, HashMap<Option<SymbolId>, Option<SymbolId>>>,
    parents: HashMap<SymbolId, Option<SymbolId>>,
}

impl CallTargets {
    pub(super) fn new(
        entities: &HashMap<u64, MaterializedEntity>,
        _facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            names: HashMap::new(),
            parents: HashMap::new(),
        };
        result
            .names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .parents
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancellation.check()?;
            if entity.record.language != "nim" {
                continue;
            }
            let parent = match entity.direct_parent {
                ContainerRef::Entity(parent) => Some(parent),
                _ => None,
            };
            result.parents.insert(entity.record.id, parent);
            let target = (entity.record.kind == EntityKind::Function).then_some(entity.record.id);
            let scopes = result
                .names
                .entry(comparison_key(&entity.record.canonical_name)?)
                .or_default();
            scopes
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            scopes
                .entry(parent)
                .and_modify(|existing| *existing = None)
                .or_insert(target);
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        name: &str,
        enclosing: Option<&MaterializedEntity>,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        let key = comparison_key(name)?;
        let mut scope = enclosing.map(|owner| owner.record.id);
        for _ in 0..=self.parents.len() {
            cancellation.check()?;
            if let Some(target) = self.names.get(&key).and_then(|scopes| scopes.get(&scope)) {
                return Ok(*target);
            }
            let Some(current) = scope else {
                return Ok(None);
            };
            let Some(parent) = self.parents.get(&current) else {
                return Ok(None);
            };
            scope = *parent;
        }
        Err(provider_failure("nim-owner-cycle"))
    }
}

/// Emit only the resolved source-owned function call, with canonical proof parents.
pub(super) fn call_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.language == "nim" && owner.record.kind == EntityKind::Function
    }) else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "nim.call.reference"
        || occurrence.role != OccurrenceRole::CallSite
        || occurrence.enclosing != Some(owner.record.id)
        || occurrence.target != (OccurrenceTarget::Resolved { symbol })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(symbol),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    cancellation.check()?;
    Ok(Some(relation))
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "nim.function.declaration")
            | (EntityKind::Struct, "nim.object.declaration")
            | (EntityKind::Enum, "nim.enum.declaration")
            | (EntityKind::TypeAlias, "nim.alias.declaration")
            | (EntityKind::Field, "nim.field.declaration")
            | (EntityKind::Parameter, "nim.parameter.declaration")
            | (EntityKind::Variable, "nim.variable.declaration")
    )
}
pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "nim.file.module") => Some((
            FactDomain::Relations,
            "nim-imports-overloads-types-and-compile-time-evaluation-unavailable",
        )),
        (
            SyntaxFactKind::Signature,
            "nim.declaration_unavailable.signature"
            | "nim.binding_unavailable.expression"
            | "nim.context_unavailable.expression",
        ) => Some((
            FactDomain::Entities,
            "nim-dynamic-conditional-or-unmodeled-declaration-unavailable",
        )),
        _ => None,
    }
}
pub(super) fn additional_source_gaps(
    fact: &SyntaxFact,
) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let gaps: &'static [(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "nim.file.module"
    {
        &[
            (
                FactDomain::Entities,
                "nim-macro-template-and-compiler-selected-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "nim-general-reads-types-and-expanded-occurrences-unavailable",
            ),
        ]
    } else {
        &[]
    };
    gaps.iter().copied()
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "nim.written_header.signature"
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "nim.call.reference" | "nim.unresolved_call.reference") => {
            Some(OccurrenceRole::CallSite)
        }
        (SyntaxFactKind::Comment, "nim.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "nim.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "nim.call.reference" | "nim.unresolved_call.reference"
        ))
    .then_some("nim-unique-written-local-call-target-unavailable")
}
