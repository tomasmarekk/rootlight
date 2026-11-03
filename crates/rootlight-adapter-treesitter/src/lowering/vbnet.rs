//! Native VB.NET declaration identity leaves type, call and dispatch binding explicit.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "vbnet.namespace.declaration")
            | (EntityKind::Class, "vbnet.class.declaration")
            | (EntityKind::Module, "vbnet.named_module.declaration")
            | (EntityKind::Struct, "vbnet.struct.declaration")
            | (EntityKind::Interface, "vbnet.interface.declaration")
            | (EntityKind::Enum, "vbnet.enum.declaration")
            | (EntityKind::Method, "vbnet.method.declaration")
            | (EntityKind::Parameter, "vbnet.parameter.declaration")
    )
}

pub(super) fn gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    Some(match fact.syntax_kind().as_str() {
        "vbnet.invalid_file.signature" => (
            FactDomain::Entities,
            "vbnet-complete-native-structure-unavailable",
        ),
        "vbnet.preprocessing_context.signature" => (
            FactDomain::Entities,
            "vbnet-complete-preprocessor-context-unavailable",
        ),
        "vbnet.type_contract.signature" => (
            FactDomain::Relations,
            "vbnet-type-contract-and-inheritance-bindings-unavailable",
        ),
        "vbnet.method_contract.signature" => (
            FactDomain::Relations,
            "vbnet-method-contract-and-dispatch-bindings-unavailable",
        ),
        "vbnet.parameter_contract.signature" => (
            FactDomain::Relations,
            "vbnet-parameter-type-and-default-bindings-unavailable",
        ),
        "vbnet.statement.signature" => (
            FactDomain::Relations,
            "vbnet-statement-binding-and-call-analysis-unavailable",
        ),
        "vbnet.import.signature" => (
            FactDomain::Relations,
            "vbnet-import-and-project-bindings-unavailable",
        ),
        "vbnet.preprocessor.signature" => (
            FactDomain::Entities,
            "vbnet-preprocessor-evaluation-unavailable",
        ),
        "vbnet.unmodeled_structure.signature" => {
            (FactDomain::Entities, "vbnet-native-construct-not-lowered")
        }
        _ => return None,
    })
}
