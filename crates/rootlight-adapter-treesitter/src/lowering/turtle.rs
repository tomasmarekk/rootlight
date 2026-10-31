//! Lower native written Turtle aliases with source-bound namespace identities.
//! Generic RDF assertions, inference and resource coalescence remain explicit gaps.

use super::*;
use crate::turtle_terms::{expanded, prefix_binding};

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "turtle.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "turtle.prefix.declaration") => Some(EntityKind::Namespace),
        (SyntaxFactKind::Declaration, "turtle.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "turtle.property.declaration") => Some(EntityKind::Property),
        _ => None,
    }
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "turtle.prefix.declaration")
            | (EntityKind::Class, "turtle.class.declaration")
            | (EntityKind::Property, "turtle.property.declaration")
    )
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    source: &str,
    drafts: &mut HashMap<u64, EntityDraft>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut events = Vec::new();
    for fact in facts {
        cancellation.check()?;
        let signature = fact.kind() == SyntaxFactKind::Signature
            && fact.syntax_kind().as_str() == "turtle.prefix_binding.signature";
        if signature || kind(fact).is_some_and(|kind| kind != EntityKind::Module) {
            events
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            events.push((fact.span().start_byte(), !signature, fact));
        }
    }
    events
        .sort_unstable_by_key(|(start, declaration, fact)| (*start, *declaration, fact.local_id()));
    let mut bindings = HashMap::<&str, Option<&str>>::new();
    let mut ordinals = HashMap::<[u8; 32], u64>::new();
    for (_, declaration, fact) in events {
        cancellation.check()?;
        let start =
            usize::try_from(fact.span().start_byte()).map_err(|_| SinkError::AccountingOverflow)?;
        let end =
            usize::try_from(fact.span().end_byte()).map_err(|_| SinkError::AccountingOverflow)?;
        let text = source
            .get(start..end)
            .ok_or_else(|| provider_failure("turtle-source-span"))?;
        if !declaration {
            if let Some((name, iri)) = prefix_binding(text) {
                bindings
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                bindings.insert(name, iri);
            } else {
                bindings.clear();
            }
            continue;
        }
        let Some(draft) = drafts.get_mut(&fact.local_id()) else {
            continue;
        };
        let (prefix, local) = if draft.kind == EntityKind::Namespace {
            let (name, Some(iri)) =
                prefix_binding(text).ok_or_else(|| provider_failure("turtle-prefix-binding"))?
            else {
                return Err(provider_failure("turtle-prefix-context"));
            };
            if draft.name != name {
                return Err(provider_failure("turtle-prefix-name"));
            }
            (iri, "")
        } else {
            expanded(&draft.name, &bindings)
                .ok_or_else(|| provider_failure("turtle-resource-binding"))?
        };
        let length = prefix
            .len()
            .checked_add(local.len())
            .ok_or(SinkError::AccountingOverflow)?;
        *strings = strings
            .checked_sub(draft.qualified_length)
            .ok_or(SinkError::AccountingOverflow)?;
        account_string(strings, length, limits)?;
        let mut qualified = String::new();
        qualified
            .try_reserve_exact(length)
            .map_err(|_| SinkError::AllocationFailed)?;
        qualified.push_str(prefix);
        qualified.push_str(local);
        let mut digest = blake3::Hasher::new();
        digest.update(b"rootlight.turtle.written-declaration/1\0");
        digest.update(fact.syntax_kind().as_str().as_bytes());
        digest.update(&[0]);
        digest.update(draft.name.as_bytes());
        digest.update(&[0]);
        digest.update(qualified.as_bytes());
        let base = *digest.finalize().as_bytes();
        ordinals
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        let ordinal = ordinals.entry(base).or_default();
        digest.update(&ordinal.to_be_bytes());
        *ordinal = ordinal
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        draft.scope_identity = None;
        draft.scope_collision_guard = None;
        draft.data_identity = Some(*digest.finalize().as_bytes());
        draft.data_qualified_name = Some(qualified);
        draft.qualified_length = length;
    }
    Ok(())
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let missing = fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "turtle.file.module";
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| {
        missing.then_some((
            domain,
            "turtle-rdf-assertions-collections-relative-iri-and-resource-resolution-unavailable",
        ))
    })
}
