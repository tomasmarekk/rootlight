//! Elixir written ownership is independent of macro expansion and BEAM binding.

use super::*;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "elixir.module.declaration")
            | (EntityKind::Protocol, "elixir.protocol.declaration")
            | (EntityKind::Function, "elixir.function.declaration")
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "elixir.file.module") => Some((
            FactDomain::Relations,
            "elixir-macro-expansion-import-arity-type-and-runtime-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "elixir.construct_analysis_unavailable.signature") => Some((
            FactDomain::Entities,
            "elixir-unmodeled-dynamic-quoted-or-recovered-construct",
        )),
        _ => None,
    }
}

pub(super) fn additional_source_gaps(
    fact: &SyntaxFact,
) -> impl Iterator<Item = (FactDomain, &'static str)> {
    // Capability gaps belong to the mandatory file record, independently of
    // optional query retention and the number of declarations in the file.
    (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "elixir.file.module")
        .then_some([
            (
                FactDomain::Entities,
                "elixir-parameter-pattern-attribute-generated-and-macro-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "elixir-variable-call-arity-and-macro-occurrence-binding-unavailable",
            ),
        ])
        .into_iter()
        .flatten()
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match fact.syntax_kind().as_str() {
        "elixir.comment.comment" => Some(OccurrenceRole::Documentation),
        "elixir.string.string" => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}
