//! Lower native TriG graph containers without inventing resource declarations.
//! Complete RDF assertions and term bindings remain explicit file-scoped gaps.

use super::*;

pub(super) fn is_graph(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Module
        && matches!(
            fact.syntax_kind().as_str(),
            "trig.named_graph.module" | "trig.default_graph.module"
        )
}

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "trig.file.module")
        || is_graph(fact)
    {
        return Some(EntityKind::Module);
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Declaration, "trig.prefix.declaration") => Some(EntityKind::Namespace),
        (SyntaxFactKind::Declaration, "trig.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "trig.property.declaration") => Some(EntityKind::Property),
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let missing =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "trig.file.module";
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| {
        missing.then_some((
            domain,
            "trig-rdf-assertions-collections-relative-iri-and-resource-resolution-unavailable",
        ))
    })
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "trig.prefix.declaration")
            | (EntityKind::Class, "trig.class.declaration")
            | (EntityKind::Property, "trig.property.declaration")
    )
}
