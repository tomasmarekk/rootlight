//! Written Julia identities and source gaps stay separate from runtime semantics.
//! Generic name matching cannot establish multiple dispatch or macro bindings.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "julia.module.declaration")
            | (
                EntityKind::Function,
                "julia.function.declaration"
                    | "julia.macro.declaration"
                    | "julia.callable_object.declaration"
                    | "julia.anonymous_function.declaration"
            )
            | (
                EntityKind::Struct,
                "julia.struct.declaration" | "julia.primitive.declaration"
            )
            | (EntityKind::Class, "julia.abstract.declaration")
            | (
                EntityKind::Variable,
                "julia.variable.declaration" | "julia.global_variable.declaration"
            )
            | (
                EntityKind::Parameter,
                "julia.parameter.declaration" | "julia.unnamed_parameter.declaration"
            )
            | (
                EntityKind::TypeParameter,
                "julia.type_parameter.declaration"
            )
            | (EntityKind::Field, "julia.field.declaration")
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "julia.unsupported_owner.signature")
        .then_some("julia-written-declaration-shape-unavailable")
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(reason) = owner_gap(fact) {
        return Some((FactDomain::Entities, reason));
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "julia.file.module") => Some((
            FactDomain::Relations,
            "julia-type-inference-multiple-dispatch-import-and-evaluation-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.quoted_source.signature") => Some((
            FactDomain::Entities,
            "julia-quoted-source-is-data-without-evaluation",
        )),
        (SyntaxFactKind::Signature, "julia.macro_expansion.signature") => Some((
            FactDomain::Relations,
            "julia-macro-expansion-and-generated-declarations-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.local_scope.signature") => Some((
            FactDomain::Relations,
            "julia-local-binding-target-and-lifetime-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.global_scope.signature") => Some((
            FactDomain::Relations,
            "julia-global-binding-target-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.outer_binding.signature") => Some((
            FactDomain::Relations,
            "julia-outer-loop-binding-target-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.binding_scope.signature") => Some((
            FactDomain::Entities,
            "julia-pattern-and-local-scope-bindings-unavailable",
        )),
        (SyntaxFactKind::Signature, "julia.mutation_target.signature") => {
            Some((FactDomain::Relations, "julia-mutation-target-unavailable"))
        }
        (SyntaxFactKind::Signature, "julia.type_binding_scope.signature") => Some((
            FactDomain::Entities,
            "julia-type-expression-binding-scope-unavailable",
        )),
        (SyntaxFactKind::Declaration, "julia.anonymous_function.declaration") => Some((
            FactDomain::Relations,
            "julia-closure-capture-and-call-target-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "julia.written_call.call") => {
            Some("julia-multiple-dispatch-target-unavailable")
        }
        (SyntaxFactKind::Occurrence, "julia.identifier.reference") => {
            Some("julia-binding-target-unavailable")
        }
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "julia.written_call.call") => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Occurrence, "julia.identifier.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Import, "julia.written_import.import") => Some(OccurrenceRole::ImportUse),
        (SyntaxFactKind::Comment, "julia.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "julia.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
