//! Keep native SPARQL source structure distinct from request evaluation.
//! Logical resource and variable bindings remain explicit coverage gaps.

use super::*;

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    rootlight_adapter_sdk::structural_entity_kind(fact)
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "sparql.prefix.declaration")
            | (EntityKind::Variable, "sparql.alias.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let missing = fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "sparql.file.module";
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| {
        missing.then_some((
            domain,
            "sparql-general-variable-binding-triple-pattern-resource-and-evaluation-semantics-unavailable",
        ))
    })
}
