//! Written V identities retain exact source spelling and unresolved bindings.
//! Compile-time execution and inferred receiver/type relationships are not facts.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "v.module.declaration")
            | (EntityKind::Struct, "v.struct.declaration")
            | (EntityKind::Union, "v.union.declaration")
            | (EntityKind::Interface, "v.interface.declaration")
            | (EntityKind::Enum, "v.enum.declaration")
            | (EntityKind::TypeAlias, "v.type.declaration")
            | (EntityKind::Function, "v.function.declaration")
            | (EntityKind::Method, "v.method.declaration")
            | (EntityKind::Field, "v.field.declaration")
            | (
                EntityKind::Constant,
                "v.constant.declaration" | "v.enumerator.declaration"
            )
            | (EntityKind::Variable, "v.variable.declaration")
            | (
                EntityKind::Parameter,
                "v.parameter.declaration" | "v.receiver.declaration"
            )
            | (EntityKind::TypeParameter, "v.type_parameter.declaration")
            | (EntityKind::Import, "v.import.declaration")
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Signature, "v.unsupported_owner.signature") => {
            Some("v-written-owner-header-unavailable")
        }
        (SyntaxFactKind::Signature, "v.unsupported_construct.signature") => {
            Some("v-written-construct-analysis-unavailable")
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(reason) = owner_gap(fact) {
        return Some((FactDomain::Entities, reason));
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "v.file.module") => Some((
            FactDomain::Relations,
            "v-type-receiver-interface-foreign-and-compile-time-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "v.compile_time.signature") => Some((
            FactDomain::Entities,
            "v-compile-time-activation-and-expansion-unavailable",
        )),
        (SyntaxFactKind::Signature, "v.import_semantics.signature") => Some((
            FactDomain::Relations,
            "v-import-module-and-selective-binding-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "v.written_call.call") => {
            Some("v-call-receiver-and-dispatch-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "v.identifier.reference") => {
            Some("v-identifier-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "v.type.reference") => Some("v-type-binding-unavailable"),
        (SyntaxFactKind::Import, "v.written_import.import") => {
            Some("v-import-target-binding-unavailable")
        }
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "v.written_call.call") => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Occurrence, "v.identifier.reference" | "v.type.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Import, "v.written_import.import") => Some(OccurrenceRole::ImportUse),
        (SyntaxFactKind::Comment, "v.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "v.string.string") => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}
