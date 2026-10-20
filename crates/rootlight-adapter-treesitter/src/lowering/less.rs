//! Join only native-certified, closed-file Less mixin declaration calls.
//! Physical witnesses and exact owners survive decoding; no generic name binding
//! may promote unresolved Less calls or claim evaluated expansion semantics.

#[cfg(test)]
mod tests;

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};
use std::collections::{HashMap, HashSet};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "less.mixin.declaration")
            | (EntityKind::Parameter, "less.parameter.declaration")
    )
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "less.name.signature" | "less.mixin.header.signature"
        )
}
// The bridge stages a closed mandatory fact roster; artifact partitioning must
// retain its context, call and recovery witnesses without adding them to identities.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    matches!(
        (fact.kind(), fact.syntax_kind().as_str()),
        (SyntaxFactKind::Root, "less.source_file")
            | (
                SyntaxFactKind::Signature,
                "less.clean_parse.signature"
                    | "less.partial_parse.signature"
                    | "less.closed_mixin_namespace.signature"
                    | "less.name.signature"
                    | "less.mixin.header.signature"
                    | "less.unsupported.construct"
            )
            | (
                SyntaxFactKind::Declaration,
                "less.mixin.declaration" | "less.parameter.declaration"
            )
            | (
                SyntaxFactKind::Occurrence,
                "less.name.definition"
                    | "less.direct_declared_mixin.call"
                    | "less.unresolved_mixin.call"
            )
            | (SyntaxFactKind::Scope, "less.block.scope")
            | (SyntaxFactKind::ErrorRecovery, "less.rejected_syntax")
    )
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "less.direct_declared_mixin.call" | "less.unresolved_mixin.call",
        ) => Some(OccurrenceRole::CallSite),
        _ => None,
    }
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "less.source_file") => &[
            (
                FactDomain::Entities,
                "less-general-stylesheet-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "less-general-values-and-variable-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "less-import-dynamic-scope-expansion-and-runtime-bindings-unavailable",
            ),
        ],
        (SyntaxFactKind::Signature, "less.unsupported.construct") => &[
            (
                FactDomain::Entities,
                "less-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "less-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "less-source-construct-semantics-unavailable",
            ),
        ],
        (SyntaxFactKind::ErrorRecovery, "less.rejected_syntax") => &[
            (
                FactDomain::Entities,
                "less-native-parse-recovery-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "less-native-parse-recovery-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "less-native-parse-recovery-bindings-unavailable",
            ),
        ],
        _ => &[],
    };
    rows.iter().copied()
}

fn same_frame(entity: &MaterializedEntity, source: &SourceRef) -> bool {
    entity.record.language == "less"
        && entity.record.kind == EntityKind::Function
        && entity.record.repository == source.repository()
        && entity.record.generation == source.generation()
        && entity.direct_parent == ContainerRef::File(source.span().file())
        && entity.record.container == Some(entity.direct_parent)
        && entity.record.evidence.source.as_ref().is_some_and(|e| {
            e.repository() == source.repository()
                && e.generation() == source.generation()
                && e.content_hash() == source.content_hash()
                && e.span().file() == source.span().file()
        })
}
fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && outer.end_byte() >= inner.end_byte()
}

struct CallWitness {
    owner: SymbolId,
    span: SourceSpan,
    native: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct CallTargets<'a> {
    names: HashMap<&'a str, Option<&'a MaterializedEntity>>,
    calls: HashMap<u64, CallWitness>,
    full: SourceRef,
}

impl<'a> CallTargets<'a> {
    pub(super) fn new<'source>(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        full: &SourceRef,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            names: HashMap::new(),
            calls: HashMap::new(),
            full: full.clone(),
        };
        result
            .names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut by_id = HashMap::new();
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut witnesses = HashMap::new();
        witnesses
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut owners = HashSet::new();
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if by_id.insert(fact.local_id(), fact).is_some() {
                return Err(SinkError::SourceMismatch.into());
            }
            if fact.kind() == SyntaxFactKind::Signature
                && let Some(parent) = fact.parent()
            {
                witnesses
                    .entry((parent, fact.syntax_kind().as_str()))
                    .and_modify(|slot| *slot = None)
                    .or_insert(Some(fact));
            }
        }
        let mut roots = facts.iter().filter(|f| {
            f.kind() == SyntaxFactKind::Root && f.syntax_kind().as_str() == "less.source_file"
        });
        let Some(root) = roots.next() else {
            return Ok(result);
        };
        if roots.next().is_some() {
            return Ok(result);
        }
        if physical(root.span())? != full.span() {
            return Ok(result);
        }
        for label in [
            "less.clean_parse.signature",
            "less.closed_mixin_namespace.signature",
        ] {
            let Some(witness) = witnesses.get(&(root.local_id(), label)).copied().flatten() else {
                return Ok(result);
            };
            if physical(witness.span())? != full.span() {
                return Ok(result);
            }
        }
        for fact in facts {
            cancel.check()?;
            if fact.kind() != SyntaxFactKind::Declaration
                || fact.syntax_kind().as_str() != "less.mixin.declaration"
                || fact.parent() != Some(root.local_id())
            {
                continue;
            }
            let Some(entity) = entities
                .get(&fact.local_id())
                .filter(|e| same_frame(e, full))
            else {
                continue;
            };
            let Some(definition) = entity
                .definition_local_id
                .and_then(|id| by_id.get(&id))
                .copied()
                .filter(|f| {
                    f.kind() == SyntaxFactKind::Occurrence
                        && f.syntax_kind().as_str() == "less.name.definition"
                        && f.parent() == Some(fact.local_id())
                })
            else {
                continue;
            };
            let name = physical(definition.span())?;
            if entity.record.evidence.source.as_ref().map(SourceRef::span)
                != Some(physical(fact.span())?)
                || !contains(physical(fact.span())?, name)
                || text(definition.span())? != entity.record.canonical_name
            {
                continue;
            }
            let Some(witness) = witnesses
                .get(&(fact.local_id(), "less.name.signature"))
                .copied()
                .flatten()
            else {
                continue;
            };
            if physical(witness.span())? != name {
                continue;
            }
            let Some(header) = witnesses
                .get(&(fact.local_id(), "less.mixin.header.signature"))
                .copied()
                .flatten()
            else {
                continue;
            };
            if !contains(physical(header.span())?, name) {
                continue;
            }
            result
                .names
                .entry(entity.record.canonical_name.as_str())
                .and_modify(|slot| *slot = None)
                .or_insert(Some(entity));
            owners.insert(entity.record.id);
        }
        for fact in facts {
            cancel.check()?;
            if fact.kind() != SyntaxFactKind::Occurrence
                || fact.syntax_kind().as_str() != "less.direct_declared_mixin.call"
            {
                continue;
            }
            let Some(scope) = fact
                .parent()
                .and_then(|id| by_id.get(&id))
                .copied()
                .filter(|f| {
                    f.kind() == SyntaxFactKind::Scope
                        && f.syntax_kind().as_str() == "less.block.scope"
                })
            else {
                continue;
            };
            let Some(owner) = scope
                .parent()
                .and_then(|id| entities.get(&id))
                .filter(|e| same_frame(e, full))
            else {
                continue;
            };
            if !owners.contains(&owner.record.id) {
                continue;
            }
            let span = physical(fact.span())?;
            if !contains(physical(scope.span())?, span)
                || !owner
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| contains(s.span(), span))
            {
                continue;
            }
            result
                .calls
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.calls.insert(
                fact.local_id(),
                CallWitness {
                    owner: owner.record.id,
                    span,
                    native: fact.span(),
                    spelling: content_hash(text(fact.span())?.as_bytes()),
                    provenance: owner.record.provenance,
                },
            );
        }
        Ok(result)
    }
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        site: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner.filter(|e| same_frame(e, &self.full)) else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "less.direct_declared_mixin.call"
            || site.role != OccurrenceRole::CallSite
            || site.syntax_kind != "less.direct_declared_mixin.call"
            || site.enclosing != Some(owner.record.id)
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || site.repository != self.full.repository()
            || site.generation != self.full.generation()
            || site.file != self.full.span().file()
            || site.source.repository() != self.full.repository()
            || site.source.generation() != self.full.generation()
            || site.source.content_hash() != self.full.content_hash()
            || !self.calls.get(&fact.local_id()).is_some_and(|witness| {
                witness.owner == owner.record.id
                    && witness.span == site.source.span()
                    && witness.native == fact.span()
                    && witness.spelling == site.syntactic_text_hash
                    && witness.spelling == content_hash(name.as_bytes())
                    && witness.provenance == site.provenance
            })
        {
            return Ok(None);
        }
        Ok(self
            .names
            .get(name)
            .copied()
            .flatten()
            .map(|target| target.record.id))
    }
}

pub(super) fn call_relation(
    site: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let Some(owner) = owner.filter(|e| same_frame(e, &site.source)) else {
        return Ok(None);
    };
    if site.role != OccurrenceRole::CallSite
        || site.syntax_kind != "less.direct_declared_mixin.call"
        || site.enclosing != Some(owner.record.id)
        || site.target != (OccurrenceTarget::Resolved { symbol: target })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(site, target)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::Calls;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(site.id),
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("less-declared-call-identity"))?;
    Ok(Some(relation))
}
