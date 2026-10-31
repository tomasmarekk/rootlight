//! Keep native Notation3 source boundaries distinct from RDF logical meaning.
//! Written declaration proofs extend this module without inventing rule entities.

use super::*;

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "n3.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "n3.prefix.declaration") => Some(EntityKind::Namespace),
        (
            SyntaxFactKind::Declaration,
            "n3.universal_variable.declaration" | "n3.existential_variable.declaration",
        ) => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "n3.prefix.declaration")
            | (
                EntityKind::Variable,
                "n3.universal_variable.declaration" | "n3.existential_variable.declaration"
            )
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let missing =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "n3.file.module";
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| {
        missing.then_some((
            domain,
            "n3-formula-rule-triple-quick-variable-and-resource-reference-semantics-unavailable",
        ))
    })
}
