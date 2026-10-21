//! Keep written Wolfram structure separate from evaluation or imported binding.
//! Full-body/target certificates are metadata only, never symbol discriminators.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "wolfram.function.declaration")
            | (EntityKind::Parameter, "wolfram.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "wolfram.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "wolfram.formal_argument.reference") => {
            Some(OccurrenceRole::Read)
        }
        (SyntaxFactKind::Occurrence, "wolfram.unproved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, _) => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, _) => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "wolfram.formal_argument.reference" | "wolfram.unproved_name.reference"
        ))
    .then_some("wolfram-source-owned-pattern-formal-binding-not-certified")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let reason = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "wolfram.file.root") => Some(
            "wolfram-written-delayed-functions-string-pattern-formals-and-finite-module-reads-only-runtime-evaluation-dynamic-scopes-and-import-targets-unavailable",
        ),
        (SyntaxFactKind::Signature, "wolfram.unproved_construct.signature") => Some(
            "wolfram-native-construct-is-retained-without-a-supported-declaration-or-binding-proof",
        ),
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
