//! Written module variables do not qualify runtime binding or reassignment.
//! Unavailable assignment proofs remain explicit in all three semantic domains.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::FactDomain;

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "python.module_assignment_unavailable.signature"
                | "python.assignment_binding_unavailable.signature"
                | "python.augmented_assignment_binding_unavailable.signature"
                | "python.module_binding_context.signature"
                | "python.module_target.signature"
        )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "python.assignment_binding_unavailable.signature"
    {
        Some(
            "python-assignment-general-binding-and-nonmodule-or-complex-target-identity-unavailable",
        )
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str()
            == "python.augmented_assignment_binding_unavailable.signature"
    {
        Some("python-augmented-write-value-effects-execution-and-read-target-binding-unavailable")
    } else if fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "python.module_binding_context.signature" | "python.module_target.signature"
        )
    {
        Some(
            "python-module-read-proof-is-lexical-only-values-calls-and-dynamic-namespace-semantics-unqualified",
        )
    } else if metadata(fact) {
        Some("python-module-assignment-name-uniqueness-syntax-or-complete-native-proof-unavailable")
    } else if fact.kind() == SyntaxFactKind::Declaration
        && fact.syntax_kind().as_str() == "python.module_variable.declaration"
    {
        Some(
            "python-written-module-variable-runtime-rebinding-aliases-and-general-reference-resolution-unavailable",
        )
    } else if matches!(
        fact.syntax_kind().as_str(),
        "python.module_binding_unavailable.reference" | "python.module_call_unavailable.call"
    ) {
        Some("python-written-module-variable-use-and-call-target-binding-unavailable")
    } else {
        None
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}
