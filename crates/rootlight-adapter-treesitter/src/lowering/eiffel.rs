//! Eiffel written declarations and occurrences retain unresolved semantic targets.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Class, "eiffel.class.declaration")
            | (EntityKind::Function, "eiffel.routine.declaration")
            | (EntityKind::Field, "eiffel.field.declaration")
            | (EntityKind::Constant, "eiffel.constant.declaration")
            | (EntityKind::Parameter, "eiffel.parameter.declaration")
            | (EntityKind::Variable, "eiffel.local.declaration")
            | (
                EntityKind::TypeParameter,
                "eiffel.type_parameter.declaration"
            )
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "eiffel.unsupported_owner.signature" => {
            Some("eiffel-written-declaration-shape-unavailable")
        }
        "eiffel.shared_feature.signature" => {
            Some("eiffel-shared-feature-body-ownership-unavailable")
        }
        "eiffel.binding_semantics.signature" => {
            Some("eiffel-agent-iteration-object-test-and-adaptation-binding-unavailable")
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(gap) = owner_gap(fact) {
        return Some((FactDomain::Entities, gap));
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "eiffel.file.module") => Some((
            FactDomain::Relations,
            "eiffel-inheritance-redefinition-case-insensitive-binding-and-dispatch-unavailable",
        )),
        (SyntaxFactKind::Signature, "eiffel.unproven_multiline_string.signature") => Some((
            FactDomain::Occurrences,
            "eiffel-tagged-and-braced-verbatim-classification-unavailable",
        )),
        (SyntaxFactKind::Signature, "eiffel.separate_binding_semantics.signature") => Some((
            FactDomain::Relations,
            "eiffel-separate-region-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "eiffel.generic_binding_semantics.signature") => Some((
            FactDomain::Relations,
            "eiffel-generic-constraint-and-representation-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "eiffel.operator_binding_semantics.signature") => Some((
            FactDomain::Relations,
            "eiffel-operator-alias-binding-and-dispatch-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "eiffel.written_call.call") => {
            Some("eiffel-feature-dispatch-target-unavailable")
        }
        (SyntaxFactKind::Occurrence, "eiffel.written_operator.call") => {
            Some("eiffel-operator-dispatch-target-unavailable")
        }
        (SyntaxFactKind::Occurrence, "eiffel.identifier.reference") => {
            Some("eiffel-variable-or-argumentless-feature-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "eiffel.type.reference") => {
            Some("eiffel-type-and-inheritance-target-unavailable")
        }
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "eiffel.written_call.call" | "eiffel.written_operator.call",
        ) => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Occurrence, "eiffel.identifier.reference" | "eiffel.type.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "eiffel.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "eiffel.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
