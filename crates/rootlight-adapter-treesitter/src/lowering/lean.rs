//! Written direct Lean headers do not establish elaboration, imports or Calls.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "lean.function.declaration")
            | (EntityKind::Parameter, "lean.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "lean.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "lean.formal_body.reference") => Some(OccurrenceRole::Read),
        (SyntaxFactKind::Occurrence, "lean.unproved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "lean.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "lean.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "lean.formal_body.reference" | "lean.unproved_name.reference"
        ))
    .then_some("lean-source-owned-direct-formal-body-binding-not-certified")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "lean.file.root") => Some(
            "lean-written-explicit-single-formals-and-direct-body-reads-only-elaboration-macros-imports-nested-bindings-and-calls-unavailable",
        ),
        (SyntaxFactKind::Root, "lean.unavailable_file.root") => {
            Some("lean-complete-clean-native-source-context-unavailable")
        }
        (SyntaxFactKind::Signature, "lean.unproved_definition.signature") => {
            Some("lean-written-definition-header-or-classification-not-certified")
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
