//! Joins native Apex lexical-use proofs to their materialized block owners.
//! Source identity and exact callable containment remain mandatory; a marker
//! cannot recover an omitted binding or establish member/dispatch semantics.

use std::hash::{Hash, Hasher};

use super::*;

#[derive(Clone, Copy)]
struct AsciiName<'a>(&'a str);

impl PartialEq for AsciiName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Eq for AsciiName<'_> {}

impl Hash for AsciiName<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for byte in self.0.bytes() {
            byte.to_ascii_lowercase().hash(state);
        }
    }
}

/// Borrowed bindings from this lowering pass, never a cross-file name index.
pub(super) struct FormalBindings<'a> {
    owners: HashMap<SymbolId, &'a EntityRecord>,
    callable_for_entity: HashMap<SymbolId, SymbolId>,
    bindings: HashMap<(SymbolId, AsciiName<'a>), Vec<LexicalBinding<'a>>>,
}

struct LexicalBinding<'a> {
    record: &'a EntityRecord,
    scope: SourceSpan,
    available: u64,
}

#[derive(Clone, Copy, Default)]
struct LexicalContext {
    callable: Option<SymbolId>,
    scope: Option<SourceSpan>,
}

impl<'a> FormalBindings<'a> {
    /// Retains written ASCII parameters and locals with exact block evidence.
    ///
    /// # Errors
    /// Propagates cancellation and checked map allocation failures.
    pub(super) fn new(
        materialized: &'a HashMap<u64, MaterializedEntity>,
        facts: &[SyntaxFact],
        full_source: &SourceRef,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut owners = HashMap::new();
        let mut callable_ids = HashMap::new();
        let mut callable_for_entity = HashMap::new();
        let mut bindings = HashMap::<(SymbolId, AsciiName<'a>), Vec<LexicalBinding<'a>>>::new();
        for (local_id, entity) in materialized {
            cancellation.check()?;
            let record = &entity.record;
            if record.language == "apex"
                && matches!(record.kind, EntityKind::Method | EntityKind::Constructor)
                && record.evidence.source.as_ref().is_some_and(|source| {
                    same_source(source, full_source)
                        && span_contains(full_source.span(), source.span())
                })
            {
                owners
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                owners.insert(record.id, record);
                callable_ids
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                callable_ids.insert(*local_id, record.id);
                callable_for_entity
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                callable_for_entity.insert(record.id, record.id);
            }
        }
        let mut by_id = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            by_id
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if by_id.insert(fact.local_id(), fact).is_some() {
                return Err(provider_failure("apex-lexical-fact-conflict"));
            }
        }
        let mut contexts = HashMap::new();
        let mut bodies = HashMap::<SymbolId, Option<SourceSpan>>::new();
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Scope
                || fact.syntax_kind().as_str() != "apex.body.scope"
            {
                continue;
            }
            let context = lexical_context(
                fact.local_id(),
                &by_id,
                &callable_ids,
                &mut contexts,
                cancellation,
            )?;
            let Some(owner) = context.callable.and_then(|id| owners.get(&id)) else {
                continue;
            };
            let Some(source) = owner.evidence.source.as_ref() else {
                continue;
            };
            if span_contains(source.span(), fact.span())
                && source.span().end_byte() == fact.span().end_byte()
            {
                bodies
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                bodies
                    .entry(owner.id)
                    .and_modify(|body| {
                        if *body != Some(fact.span()) {
                            *body = None;
                        }
                    })
                    .or_insert(Some(fact.span()));
            }
        }
        for (local_id, entity) in materialized {
            cancellation.check()?;
            let record = &entity.record;
            if record.language != "apex"
                || !matches!(record.kind, EntityKind::Parameter | EntityKind::Variable)
                || entity.definition_local_id.is_none()
                || !record.canonical_name.is_ascii()
            {
                continue;
            }
            let Some(ContainerRef::Entity(owner)) = record.container else {
                continue;
            };
            let context = lexical_context(
                *local_id,
                &by_id,
                &callable_ids,
                &mut contexts,
                cancellation,
            )?;
            if context.callable != Some(owner) {
                continue;
            }
            let Some(Some(body)) = bodies.get(&owner).copied() else {
                continue;
            };
            let Some(owner_source) = owners
                .get(&owner)
                .and_then(|owner| owner.evidence.source.as_ref())
            else {
                continue;
            };
            if record.evidence.source.as_ref().is_none_or(|source| {
                !same_source(source, full_source)
                    || !span_contains(owner_source.span(), source.span())
            }) {
                continue;
            }
            let source = record
                .evidence
                .source
                .as_ref()
                .ok_or_else(|| provider_failure("apex-lexical-source"))?;
            let (scope, available) = if record.kind == EntityKind::Parameter {
                (body, body.start_byte())
            } else {
                let Some(scope) = context.scope.filter(|scope| {
                    span_contains(body, *scope) && span_contains(*scope, source.span())
                }) else {
                    continue;
                };
                (scope, source.span().end_byte())
            };
            bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let named = bindings
                .entry((owner, AsciiName(&record.canonical_name)))
                .or_default();
            named
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            named.push(LexicalBinding {
                record,
                scope,
                available,
            });
            callable_for_entity
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            callable_for_entity.insert(record.id, owner);
        }
        Ok(Self {
            owners,
            callable_for_entity,
            bindings,
        })
    }

    /// Returns a target only for a native-certified use in its exact callable.
    ///
    /// # Errors
    /// Propagates cancellation; missing, ambiguous, or foreign evidence declines.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        text: &str,
        enclosing: Option<SymbolId>,
        full_source: &SourceRef,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || !matches!(
                fact.syntax_kind().as_str(),
                "apex.formal_identifier.reference" | "apex.local_identifier.reference"
            )
            || !text.is_ascii()
        {
            return Ok(None);
        }
        let Some(owner) = enclosing
            .and_then(|enclosing| self.callable_for_entity.get(&enclosing))
            .copied()
        else {
            return Ok(None);
        };
        let Some(owner_source) = self
            .owners
            .get(&owner)
            .and_then(|record| record.evidence.source.as_ref())
        else {
            return Ok(None);
        };
        if !span_contains(owner_source.span(), fact.span())
            || !span_contains(full_source.span(), fact.span())
        {
            return Ok(None);
        }
        let Some(named) = self.bindings.get(&(owner, AsciiName(text))) else {
            return Ok(None);
        };
        let mut selected = None;
        for binding in named {
            cancellation.check()?;
            if !span_contains(binding.scope, fact.span()) {
                continue;
            }
            if selected.is_some()
                || binding.available > fact.span().start_byte()
                || binding.record.canonical_name != text
            {
                return Ok(None);
            }
            let kind = if fact.syntax_kind().as_str() == "apex.formal_identifier.reference" {
                EntityKind::Parameter
            } else {
                EntityKind::Variable
            };
            if binding.record.kind != kind {
                return Ok(None);
            }
            selected = Some(binding.record.id);
        }
        Ok(selected)
    }
}

fn lexical_context(
    local_id: u64,
    facts: &HashMap<u64, &SyntaxFact>,
    callables: &HashMap<u64, SymbolId>,
    contexts: &mut HashMap<u64, LexicalContext>,
    cancellation: &Cancellation,
) -> Result<LexicalContext, AdapterError> {
    let mut current = Some(local_id);
    let mut path = Vec::new();
    let mut context = LexicalContext::default();
    while let Some(id) = current {
        cancellation.check()?;
        if let Some(cached) = contexts.get(&id) {
            context = *cached;
            break;
        }
        let fact = facts
            .get(&id)
            .ok_or_else(|| provider_failure("apex-lexical-parent"))?;
        if path.len() >= facts.len() {
            return Err(provider_failure("apex-lexical-parent-cycle"));
        }
        path.try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        path.push(*fact);
        current = fact.parent();
    }
    for fact in path.into_iter().rev() {
        cancellation.check()?;
        if let Some(callable) = callables.get(&fact.local_id()) {
            context = LexicalContext {
                callable: Some(*callable),
                scope: None,
            };
        } else if context.callable.is_some()
            && fact.kind() == SyntaxFactKind::Scope
            && fact.syntax_kind().as_str() == "apex.body.scope"
        {
            context.scope = Some(fact.span());
        }
        contexts
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        contexts.insert(fact.local_id(), context);
    }
    Ok(context)
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}
