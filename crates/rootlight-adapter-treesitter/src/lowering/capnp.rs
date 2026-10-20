//! Same-file Cap'n Proto type links follow canonical typed lexical ancestry.
//! Borrowed names are indexed after entity mutation; lookup allocates no strings.
//! Imports/aliases/generics disable target inference rather than guessing shadows.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord,
    derive_relation_record_id,
};

use super::{MaterializedEntity, lexical_reference_relation, provider_failure};

/// Add a field edge only for an already resolved, source-owned type use.
/// Other owners and unproven targets produce no relation; the occurrence edge
/// remains separate. Cancellation, allocation and identity errors propagate.
pub(super) fn field_type_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner
        .filter(|owner| owner.record.kind == EntityKind::Field && owner.record.language == "capnp")
    else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "capnp.named_type.reference"
        || occurrence.role != OccurrenceRole::TypeUse
        || occurrence.enclosing != Some(owner.record.id)
        || occurrence.target != (OccurrenceTarget::Resolved { symbol })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::UsesType;
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
    // Normalization must not change semantic fields after the ID is derived.
    relation.evidence.derivation.sort_unstable();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    cancellation.check()?;
    Ok(Some(relation))
}

pub(super) struct TypeTargets<'a> {
    by_id: HashMap<SymbolId, &'a MaterializedEntity>,
    by_name: HashMap<&'a str, Vec<&'a MaterializedEntity>>,
    available: bool,
}

fn type_kind(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Struct | EntityKind::Enum | EntityKind::Interface
    )
}

impl<'a> TypeTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            available: true,
        };
        result
            .by_id
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        result
            .by_name
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "capnp.binding_unavailable.signature" | "capnp.declaration_unavailable.signature"
            ) {
                result.available = false;
            }
        }
        for entity in entities.values() {
            cancellation.check()?;
            result.by_id.insert(entity.record.id, entity);
            if entity.record.language == "capnp" && type_kind(entity.record.kind) {
                let candidates = result
                    .by_name
                    .entry(entity.record.canonical_name.as_str())
                    .or_default();
                candidates
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                // Preserve authored owners even when duplicate symbol IDs coalesce.
                candidates.push(entity);
            }
        }
        Ok(result)
    }

    fn parent(&self, entity: &'a MaterializedEntity) -> Option<&'a MaterializedEntity> {
        match entity.record.container {
            Some(ContainerRef::Entity(parent)) => self.by_id.get(&parent).copied(),
            _ => None,
        }
    }

    fn matches(
        &self,
        entity: &'a MaterializedEntity,
        path: &str,
        scope: Option<SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = Some(entity);
        for segment in path.rsplit('.') {
            cancellation.check()?;
            let Some(owner) = current else {
                return Ok(false);
            };
            if !type_kind(owner.record.kind) || owner.record.canonical_name != segment {
                return Ok(false);
            }
            current = self.parent(owner);
        }
        if current.is_some_and(|owner| owner.record.kind == EntityKind::Module) {
            current = None;
        }
        Ok(current.map(|owner| owner.record.id) == scope)
    }

    pub(super) fn resolve(
        &self,
        path: &str,
        enclosing: Option<SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        if !self.available {
            return Ok(None);
        }
        let Some(leaf) = path.rsplit('.').next() else {
            return Ok(None);
        };
        let Some(candidates) = self.by_name.get(leaf) else {
            return Ok(None);
        };
        let mut context = enclosing.and_then(|id| self.by_id.get(&id).copied());
        let mut visited = 0usize;
        loop {
            cancellation.check()?;
            if context.is_none() || context.is_some_and(|owner| type_kind(owner.record.kind)) {
                let scope = context.map(|owner| owner.record.id);
                let mut target = None;
                let mut count = 0usize;
                for &candidate in candidates {
                    if self.matches(candidate, path, scope, cancellation)? {
                        count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                        target = Some(candidate.record.id);
                    }
                }
                if count > 0 {
                    return Ok(if count == 1 { target } else { None });
                }
            }
            let Some(owner) = context else {
                return Ok(None);
            };
            visited = visited
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            if visited > self.by_id.len() {
                return Ok(None);
            }
            context = self.parent(owner);
        }
    }
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Struct, "capnp.struct.declaration")
            | (EntityKind::Interface, "capnp.interface.declaration")
            | (EntityKind::Enum, "capnp.enum.declaration")
            | (EntityKind::Union, "capnp.union.declaration")
            | (
                EntityKind::Field,
                "capnp.field.declaration" | "capnp.group.declaration"
            )
            | (
                EntityKind::Parameter,
                "capnp.parameter.declaration" | "capnp.return_parameter.declaration"
            )
            | (
                EntityKind::Constant,
                "capnp.const.declaration" | "capnp.enum_value.declaration"
            )
            | (EntityKind::Method, "capnp.method.declaration")
            | (EntityKind::Function, "capnp.annotation.declaration")
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "capnp.file.module") => Some((
            FactDomain::Relations,
            "capnp-wire-default-ordinal-annotation-and-compiler-semantics-unavailable",
        )),
        (SyntaxFactKind::Signature, "capnp.binding_unavailable.signature") => Some((
            FactDomain::Relations,
            "capnp-import-alias-or-generic-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "capnp.type_unavailable.signature") => Some((
            FactDomain::Occurrences,
            "capnp-compound-type-spelling-unavailable",
        )),
        (SyntaxFactKind::Signature, "capnp.declaration_unavailable.signature") => Some((
            FactDomain::Entities,
            "capnp-declaration-name-owner-or-grammar-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    source_gap(fact)
        .filter(|(domain, _)| *domain == FactDomain::Entities)
        .map(|(_, code)| code)
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact
            .syntax_kind()
            .as_str()
            .strip_suffix(".signature")
            .is_some_and(|label| {
                matches!(
                    label,
                    "capnp.struct"
                        | "capnp.enum"
                        | "capnp.interface"
                        | "capnp.field"
                        | "capnp.group"
                        | "capnp.union"
                        | "capnp.enum_value"
                        | "capnp.const"
                        | "capnp.annotation"
                        | "capnp.method"
                        | "capnp.parameter"
                        | "capnp.return_parameter"
                )
            })
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "capnp.named_type.reference" | "capnp.unavailable_type.reference",
        ) => Some(OccurrenceRole::TypeUse),
        (SyntaxFactKind::Occurrence, "capnp.written_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "capnp.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "capnp.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match fact.syntax_kind().as_str() {
        "capnp.named_type.reference" | "capnp.unavailable_type.reference" => {
            Some("capnp-unique-same-file-type-target-unavailable")
        }
        "capnp.written_name.reference" => {
            Some("capnp-constant-annotation-or-alias-target-unavailable")
        }
        _ => None,
    }
}
