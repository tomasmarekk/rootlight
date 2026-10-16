//! Coq source declarations are independent of kernel checking and elaboration.
//! Flat Module/Section sentences retain owner gaps instead of invented scopes.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Constant,
            "coq.definition.declaration"
                | "coq.theorem.declaration"
                | "coq.fixpoint.declaration"
                | "coq.instance.declaration"
                | "coq.declared_instance.declaration"
        ) | (EntityKind::Enum, "coq.inductive.declaration")
            | (EntityKind::Struct, "coq.record.declaration")
            | (EntityKind::Class, "coq.class.declaration")
            | (
                EntityKind::Field,
                "coq.record_field.declaration" | "coq.class_field.declaration"
            )
            | (EntityKind::Constructor, "coq.constructor.declaration")
            | (
                EntityKind::Constructor,
                "coq.record_constructor.declaration"
            )
            | (
                EntityKind::Module,
                "coq.module_header.declaration" | "coq.module_type_header.declaration"
            )
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "coq.file.module") => Some((
            FactDomain::Relations,
            "coq-elaboration-typechecking-proofchecking-and-term-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.module_ownership.signature") => Some((
            FactDomain::Relations,
            "coq-module-section-stack-and-qualified-ownership-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.instance_semantics.signature") => Some((
            FactDomain::Relations,
            "coq-instance-typeclass-registration-and-field-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.record_semantics.signature") => Some((
            FactDomain::Relations,
            "coq-record-class-elaboration-projection-generation-and-field-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.context_binding.signature") => Some((
            FactDomain::Entities,
            "coq-context-and-assumption-binder-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.unmodeled_binder.signature") => Some((
            FactDomain::Entities,
            "coq-term-and-module-binder-entities-and-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.invalid_name.signature") => {
            Some((FactDomain::Entities, "coq-declaration-name-unavailable"))
        }
        (SyntaxFactKind::Signature, "coq.shared_definition.signature") => Some((
            FactDomain::Entities,
            "coq-mutual-definition-body-ownership-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.parser_recovery.signature") => Some((
            FactDomain::Entities,
            "coq-parser-recovery-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.custom_syntax.signature") => Some((
            FactDomain::Entities,
            "coq-custom-notation-or-generic-tactic-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.unmodeled_sentence.signature") => Some((
            FactDomain::Entities,
            "coq-vernacular-construct-structure-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.conditional_sentence.signature") => Some((
            FactDomain::Entities,
            "coq-fail-attribute-or-selector-declaration-effect-unavailable",
        )),
        (SyntaxFactKind::Signature, "coq.proof_checking.signature") => Some((
            FactDomain::Relations,
            "coq-proof-state-and-tactic-semantics-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "coq.written_reference.reference")
        .then_some("coq-name-resolution-and-implicit-argument-elaboration-unavailable")
}

pub(super) fn structural_gap(fact: &SyntaxFact) -> Option<&'static str> {
    source_gap(fact)
        .filter(|(domain, _)| *domain == FactDomain::Entities)
        .map(|(_, detail)| detail)
}

pub(super) fn term_surface_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "coq.file.module")
        .then_some("coq-term-pattern-tactic-and-notation-reference-surface-incomplete")
}

pub(super) fn entity_surface_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "coq.file.module")
        .then_some("coq-local-term-binder-projection-and-generated-declaration-coverage-incomplete")
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "coq.written_reference.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "coq.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "coq.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn declaration_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    // These name anchors describe requirements or written fields. They do not
    // prove an instance implementation or a generated global projection.
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "coq.declared_instance_name.definition"
                | "coq.record_field_name.definition"
                | "coq.class_field_name.definition"
        ))
    .then_some(OccurrenceRole::Declaration)
}
