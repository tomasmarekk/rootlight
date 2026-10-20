//! Join native-certified direct Scheme operators to current top-level procedures.
//! Reader, macro, imported and runtime binding identity remain outside this proof.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord, SourceRef, SourceSpan,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

struct Target<'a> {
    entity: &'a MaterializedEntity,
    definition: SourceSpan,
}

pub(super) struct CallTargets<'a> {
    names: HashMap<(SymbolId, &'a str), Option<Target<'a>>>,
    source: &'a SourceRef,
    procedures: HashSet<SymbolId>,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

impl<'a> CallTargets<'a> {
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        facts: &HashMap<u64, &SyntaxFact>,
        source: &'a SourceRef,
        physical: impl Fn(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut modules = HashSet::new();
        modules
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "scheme"
                && entity.record.kind == EntityKind::Module
                && entity.record.repository == source.repository()
                && entity.record.generation == source.generation()
                && entity
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| same_source(s, source))
            {
                modules.insert(entity.record.id);
            }
        }
        let mut procedures = HashSet::new();
        procedures
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut names = HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local_id, entity) in entities {
            cancel.check()?;
            let ContainerRef::Entity(module) = entity.direct_parent else {
                continue;
            };
            if !modules.contains(&module)
                || entity.record.language != "scheme"
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
            {
                continue;
            }
            let Some(entity_source) = entity
                .record
                .evidence
                .source
                .as_ref()
                .filter(|s| same_source(s, source))
            else {
                continue;
            };
            let definition = entity
                .definition_local_id
                .and_then(|id| facts.get(&id))
                .copied();
            let declaration = facts.get(local_id).copied().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "scheme.procedure.declaration"
            });
            let native_owner = if let Some(declaration) = declaration {
                physical(declaration.span())? == entity_source.span()
            } else {
                false
            };
            if native_owner {
                procedures.insert(entity.record.id);
            }
            let target = if native_owner
                && entity.record.kind == EntityKind::Function
                && entity.record.canonical_name.is_ascii()
                && let Some(definition) = definition.filter(|fact| {
                    fact.kind() == SyntaxFactKind::Occurrence
                        && fact.syntax_kind().as_str() == "scheme.name.definition"
                }) {
                let definition = physical(definition.span())?;
                (definition.file() == source.span().file()
                    && definition.start_byte() >= entity_source.span().start_byte()
                    && definition.end_byte() <= entity_source.span().end_byte())
                .then_some(Target { entity, definition })
            } else {
                None
            };
            names
                .entry((module, entity.record.canonical_name.as_str()))
                .and_modify(|existing| *existing = None)
                .or_insert(target);
        }
        Ok(Self {
            names,
            source,
            procedures,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "scheme" && owner.record.kind == EntityKind::Function
        }) else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let ContainerRef::Entity(module) = owner.direct_parent else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "scheme.direct_file_call.reference"
            || occurrence.syntax_kind != "scheme.direct_file_call.reference"
            || occurrence.role != OccurrenceRole::CallSite
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || !self.procedures.contains(&owner.record.id)
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != physical
            || occurrence.provenance != owner.record.provenance
            || owner.record.repository != self.source.repository()
            || owner.record.generation != self.source.generation()
            || !same_source(owner_source, self.source)
            || !same_source(&occurrence.source, self.source)
            || owner_source.span().start_byte() > physical.start_byte()
            || owner_source.span().end_byte() < physical.end_byte()
            || !text.is_ascii()
            || occurrence.syntactic_text_hash != content_hash(text.as_bytes())
        {
            return Ok(None);
        }
        let Some(Some(target)) = self.names.get(&(module, text)) else {
            return Ok(None);
        };
        let Some(target_source) = target.entity.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !same_source(target_source, self.source)
            || target.entity.record.provenance != occurrence.provenance
            || target.definition.file() != physical.file()
            || target.definition.start_byte() < target_source.span().start_byte()
            || target.definition.end_byte() > target_source.span().end_byte()
        {
            return Ok(None);
        }
        Ok(Some(target.entity.record.id))
    }
}

pub(super) fn call_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    target: SymbolId,
    cancel: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancel.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.language == "scheme" && owner.record.kind == EntityKind::Function
    }) else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "scheme.direct_file_call.reference"
        || occurrence.role != OccurrenceRole::CallSite
        || occurrence.enclosing != Some(owner.record.id)
        || occurrence.target != (OccurrenceTarget::Resolved { symbol: target })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(occurrence, target)?;
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
        FactRef::Entity(target),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    cancel.check()?;
    Ok(Some(relation))
}

/// Accepts complete native proof retained through optional-only query clipping.
pub(super) fn admits_certified_partition(
    output: &rootlight_adapter_sdk::ParseOutput,
    source: &SourceRef,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    cancel.check()?;
    let coverage = output.report().coverage();
    let length = source
        .span()
        .end_byte()
        .checked_sub(source.span().start_byte());
    if source.span().start_byte() != 0
        || length.and_then(|length| usize::try_from(length).ok())
            != Some(coverage.total_source_bytes())
        || coverage.covered_source_bytes() != coverage.total_source_bytes()
    {
        return Ok(false);
    }
    match coverage.status() {
        rootlight_ir::CoverageStatus::Complete => {}
        rootlight_ir::CoverageStatus::Bounded => {
            // Native recovery, traversal/context loss and mixed failures do not
            // inherit a certificate from the optional capture partition.
            if coverage.skipped_regions() != 1 || output.diagnostics().len() != 1 {
                return Ok(false);
            }
            let diagnostic = &output.diagnostics()[0];
            if diagnostic.code().as_str() != "syntax-extraction-limit"
                || !diagnostic.source().is_some_and(|witness|
                    // Sealed syntax artifacts rebind into a fresh generation.
                    // The immutable diagnostic tuple stays source-authenticated.
                    witness.repository() == source.repository()
                        && witness.content_hash() == source.content_hash()
                        && witness.span() == source.span())
            {
                return Ok(false);
            }
        }
        _ => return Ok(false),
    }
    let mut root = false;
    let mut module = false;
    let mut scope = false;
    let mut certified = false;
    for fact in output.facts() {
        cancel.check()?;
        if fact.span().file() != source.span().file()
            || fact.span().end_byte() > source.span().end_byte()
        {
            return Ok(false);
        }
        match (fact.kind(), fact.syntax_kind().as_str()) {
            (SyntaxFactKind::Root, "scheme.file.root") if fact.span() == source.span() => {
                if root {
                    return Ok(false);
                }
                root = true;
            }
            (SyntaxFactKind::Module, "scheme.file.module") if fact.span() == source.span() => {
                if module {
                    return Ok(false);
                }
                module = true;
            }
            (SyntaxFactKind::Scope, "scheme.file.scope") if fact.span() == source.span() => {
                if scope {
                    return Ok(false);
                }
                scope = true;
            }
            (SyntaxFactKind::Occurrence, "scheme.direct_file_call.reference") => {
                certified |=
                    fact.parent().is_some() && fact.span().start_byte() < fact.span().end_byte();
            }
            _ => {}
        }
    }
    cancel.check()?;
    Ok(root && module && scope && certified)
}
