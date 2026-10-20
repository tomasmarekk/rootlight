//! Source-only Q# structure and explicit unsupported semantic domains.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};
pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "qsharp.namespace.declaration")
            | (EntityKind::Function, "qsharp.function.declaration")
            | (EntityKind::Parameter, "qsharp.parameter.declaration")
    )
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "qsharp.header.signature"
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "qsharp.formal_argument.reference") => {
            Some(OccurrenceRole::Read)
        }
        (SyntaxFactKind::Occurrence, "qsharp.unresolved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, _) => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, _) => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}
pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    matches!(
        (fact.kind(), fact.syntax_kind().as_str()),
        (
            SyntaxFactKind::Occurrence,
            "qsharp.formal_argument.reference" | "qsharp.unresolved_name.reference"
        )
    )
    .then_some("qsharp-same-owner-argument-or-external-name-binding-unavailable")
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let reason = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "qsharp.file.root") => Some(
            "qsharp-written-headers-and-finite-native-argument-reads-only-import-targets-specialization-execution-and-quantum-semantics-unavailable",
        ),
        (SyntaxFactKind::Declaration, "qsharp.unsupported.declaration") => {
            Some("qsharp-written-declaration-shape-or-owner-not-certified")
        }
        (SyntaxFactKind::Signature, "qsharp.construct_unavailable.expression") => {
            Some("qsharp-construct-retained-without-binding-or-execution-proof")
        }
        _ => None,
    };
    reason.into_iter().flat_map(|reason| {
        [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ]
        .into_iter()
        .map(move |domain| (domain, reason))
    })
}
