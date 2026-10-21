//! Written Chapel ownership and explicit boundaries of the finite native profile.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "chapel.module.declaration")
            | (EntityKind::Function, "chapel.function.declaration")
            | (EntityKind::Parameter, "chapel.parameter.declaration")
    )
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "chapel.header.signature"
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match fact.kind() {
        SyntaxFactKind::Comment => Some(OccurrenceRole::Documentation),
        SyntaxFactKind::StringLiteral => Some(OccurrenceRole::StringEvidence),
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "chapel.unresolved_name.reference" =>
        {
            Some(OccurrenceRole::Reference)
        }
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "chapel.formal_initializer.reference" =>
        {
            Some(OccurrenceRole::Read)
        }
        SyntaxFactKind::Occurrence
            if fact.syntax_kind().as_str() == "chapel.typed_local_call.reference" =>
        {
            Some(OccurrenceRole::CallSite)
        }
        _ => None,
    }
}
pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "chapel.typed_local_call.reference") => {
            Some("chapel-typed-local-overload-current-source-proof-unavailable")
        }
        (SyntaxFactKind::Occurrence, "chapel.formal_initializer.reference") => {
            Some("chapel-native-own-formal-initializer-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "chapel.unresolved_name.reference") => {
            Some("chapel-overload-types-and-lexical-binding-unavailable")
        }
        _ => None,
    }
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "chapel.source_file.root") => Some(
            "chapel-written-headers-and-certified-formal-initializers-only-overload-and-runtime-semantics-unavailable",
        ),
        (
            SyntaxFactKind::Signature,
            "chapel.construct_unavailable.expression" | "chapel.unsupported_context.signature",
        ) => {
            Some("chapel-native-unsupported-keyword-control-or-mutation-has-no-binding-certificate")
        }
        _ => None,
    };
    detail.into_iter().flat_map(|detail| {
        [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ]
        .into_iter()
        .map(move |domain| (domain, detail))
    })
}
