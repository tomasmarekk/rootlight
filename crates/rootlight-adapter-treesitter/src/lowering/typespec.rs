//! Lower written TypeSpec declarations without evaluating imports or decorators.
//! Native owner intervals establish Contains only; type and value bindings remain gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "typespec.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "typespec.namespace.declaration") => {
            Some(EntityKind::Namespace)
        }
        (SyntaxFactKind::Declaration, "typespec.model.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "typespec.property.declaration") => {
            Some(EntityKind::Property)
        }
        (SyntaxFactKind::Declaration, "typespec.interface.declaration") => {
            Some(EntityKind::Interface)
        }
        (SyntaxFactKind::Declaration, "typespec.operation.declaration") => {
            Some(EntityKind::Function)
        }
        (SyntaxFactKind::Declaration, "typespec.method.declaration") => Some(EntityKind::Method),
        _ => None,
    }
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "typespec.namespace.declaration")
            | (EntityKind::Struct, "typespec.model.declaration")
            | (EntityKind::Property, "typespec.property.declaration")
            | (EntityKind::Interface, "typespec.interface.declaration")
            | (EntityKind::Function, "typespec.operation.declaration")
            | (EntityKind::Method, "typespec.method.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "typespec.file.module") => Some(
            "typespec-import-type-decorator-reference-resolution-and-other-declarations-unavailable",
        ),
        (SyntaxFactKind::Signature, "typespec.context_unavailable.expression") => Some(
            "typespec-written-context-does-not-qualify-type-value-import-or-decorator-semantics",
        ),
        (SyntaxFactKind::Signature, "typespec.namespace_context_unavailable.expression") => {
            Some("typespec-file-namespace-context-or-complete-native-proof-unavailable")
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
