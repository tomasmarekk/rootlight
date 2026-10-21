//! Keep CoffeeScript structural identities separate from unavailable binding.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};
pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Class, "coffeescript.class.declaration")
            | (EntityKind::Method, "coffeescript.method.declaration")
            | (EntityKind::Variable, "coffeescript.variable.declaration")
    )
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "coffeescript.header.signature"
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "coffeescript.unresolved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "coffeescript.comment.comment") => {
            Some(OccurrenceRole::Documentation)
        }
        (SyntaxFactKind::StringLiteral, "coffeescript.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "coffeescript.unresolved_name.reference")
        .then_some("coffeescript-name-read-write-type-and-call-target-binding-unavailable")
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "coffeescript.source_file.root") => Some(
            "coffeescript-clean-written-class-method-and-unique-local-assignment-owners-only-binding-inheritance-imports-and-runtime-unavailable",
        ),
        (SyntaxFactKind::Declaration, "coffeescript.unsupported.declaration") => {
            Some("coffeescript-written-declaration-or-local-owner-proof-unavailable")
        }
        (SyntaxFactKind::Signature, "coffeescript.construct_unavailable.expression") => {
            Some("coffeescript-source-construct-semantics-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}
