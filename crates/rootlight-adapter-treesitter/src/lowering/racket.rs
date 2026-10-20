//! Resolve only unique written Racket call targets in their lexical owner chain.
//! Reader evaluation, expansion, imports and runtime rebinding remain explicit gaps.

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

/// Borrows names only after the caller has completed shared entity mutations.
pub(super) struct CallTargets<'a> {
    names: HashMap<(Option<SymbolId>, &'a str), Option<SymbolId>>,
    parents: HashMap<SymbolId, Option<SymbolId>>,
}

impl<'a> CallTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            names: HashMap::new(),
            parents: HashMap::new(),
        };
        for fact in facts {
            cancellation.check()?;
            // A withheld binding can hide a conflicting lexical target.
            if fact.syntax_kind().as_str() == "racket.binding_unavailable.expression" {
                return Ok(result);
            }
        }
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
            if entity.record.language != "racket" {
                continue;
            }
            let parent = match entity.direct_parent {
                ContainerRef::Entity(parent) => Some(parent),
                _ => None,
            };
            result.parents.insert(entity.record.id, parent);
            let target = (entity.record.kind == EntityKind::Function).then_some(entity.record.id);
            result
                .names
                .entry((parent, entity.record.canonical_name.as_str()))
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
        let mut scope = enclosing.map(|owner| owner.record.id);
        for _ in 0..=self.parents.len() {
            cancellation.check()?;
            if let Some(target) = self.names.get(&(scope, name)) {
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
        Err(provider_failure("racket-owner-cycle"))
    }
}

/// Emit a direct written call only from its resolved source-owned function.
/// Canonical proof parents precede identity derivation; cancellation and failed
/// allocation propagate without publishing a partial relationship.
pub(super) fn call_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.language == "racket" && owner.record.kind == EntityKind::Function
    }) else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "racket.call.reference"
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
        (EntityKind::Function, "racket.function.declaration")
            | (EntityKind::Variable, "racket.variable.declaration")
            | (EntityKind::Parameter, "racket.parameter.declaration")
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "racket.file.module") => Some((
            FactDomain::Relations,
            "racket-reader-macro-import-and-runtime-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "racket.binding_unavailable.expression") => Some((
            FactDomain::Entities,
            "racket-dynamic-or-unmodeled-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "racket.reader_unavailable.expression") => Some((
            FactDomain::Entities,
            "racket-reader-language-or-case-environment-unavailable",
        )),
        (SyntaxFactKind::Signature, "racket.unavailable.expression") => Some((
            FactDomain::Entities,
            "racket-unmodeled-lexical-or-module-construct",
        )),
        (SyntaxFactKind::Signature, "racket.application_unavailable.expression") => Some((
            FactDomain::Relations,
            "racket-application-argument-or-macro-context-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn additional_source_gaps(
    fact: &SyntaxFact,
) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let gaps: &'static [(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "racket.file.module"
    {
        &[
            (
                FactDomain::Entities,
                "racket-generated-struct-macro-and-reader-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "racket-general-variable-parameter-and-expanded-occurrences-unavailable",
            ),
        ]
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "racket.reader_unavailable.expression"
    {
        &[
            (
                FactDomain::Occurrences,
                "racket-reader-occurrence-syntax-unavailable",
            ),
            (
                FactDomain::Relations,
                "racket-reader-relation-syntax-unavailable",
            ),
        ]
    } else {
        &[]
    };
    gaps.iter().copied()
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "racket.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "racket.call.reference") => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Comment, "racket.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "racket.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "racket.call.reference")
        .then_some("racket-written-call-target-unavailable")
}
