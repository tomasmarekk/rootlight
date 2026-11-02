//! Native interface structure does not establish token-sequence semantics.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Interface, "abap.interface.declaration")
            | (EntityKind::Method, "abap.method.declaration")
    )
}

pub(super) fn gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    Some(match fact.syntax_kind().as_str() {
        "abap.invalid_file.signature" => (
            FactDomain::Entities,
            "abap-complete-native-structure-unavailable",
        ),
        "abap.method_contract.signature" => (
            FactDomain::Relations,
            "abap-method-parameter-and-contract-bindings-unavailable",
        ),
        "abap.unmodeled_statement.signature" => (
            FactDomain::Entities,
            "abap-statement-structure-and-bindings-unavailable",
        ),
        "abap.unmodeled_structure.signature" => {
            (FactDomain::Entities, "abap-native-construct-not-lowered")
        }
        _ => return None,
    })
}
