//! Written identities and explicit limits for the initial SML/WAT native profiles.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Function,
            "sml.function.declaration" | "wat.function.declaration"
        ) | (EntityKind::Parameter, "wat.parameter.declaration")
            | (EntityKind::Module, "wat.module.declaration")
    )
}
pub(super) fn symbol_signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "sml.header.signature" | "wat.header.signature"
        )
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    if fact.kind() == SyntaxFactKind::Comment {
        return Some(OccurrenceRole::Documentation);
    }
    if fact.kind() == SyntaxFactKind::StringLiteral {
        return Some(OccurrenceRole::StringEvidence);
    }
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "sml.own_function_call.reference" => Some(OccurrenceRole::CallSite),
        "sml.unresolved_name.reference" | "wat.unresolved_name.reference" => {
            Some(OccurrenceRole::Reference)
        }
        _ => None,
    }
}
pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "sml.own_function_call.reference" => {
            Some("sml-native-own-function-recursion-binding-unavailable")
        }
        "sml.unresolved_name.reference" => {
            Some("sml-incoming-basis-pattern-identifier-status-and-binding-unavailable")
        }
        "wat.unresolved_name.reference" => {
            Some("wat-local-function-index-import-and-runtime-binding-unavailable")
        }
        _ => None,
    }
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "sml.source_file.root") => Some(
            "sml-clean-single-clause-function-heads-only-incoming-basis-pattern-binders-and-bindings-unavailable",
        ),
        (SyntaxFactKind::Root, "wat.source_file.root") => Some(
            "wat-written-named-modules-functions-scalar-formals-only-index-import-type-runtime-semantics-unavailable",
        ),
        (
            SyntaxFactKind::Declaration,
            "sml.unsupported.declaration" | "wat.unsupported.declaration",
        ) => Some("native-written-declaration-shape-or-owner-not-certified"),
        (
            SyntaxFactKind::Signature,
            "sml.construct_unavailable.expression" | "wat.construct_unavailable.expression",
        ) => Some("native-construct-retained-without-complete-binding-or-execution-semantics"),
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
