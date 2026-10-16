//! CMake source categories never imply command, variable or configuration binding.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Function,
            "cmake.function.declaration" | "cmake.macro.declaration"
        ) | (
            EntityKind::Parameter,
            "cmake.parameter.declaration" | "cmake.macro_placeholder.declaration"
        ) | (
            EntityKind::Variable,
            "cmake.normal_assignment.declaration"
                | "cmake.cache_assignment.declaration"
                | "cmake.environment_assignment.declaration"
                | "cmake.option.declaration"
        ) | (EntityKind::BuildTarget, "cmake.target.declaration")
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "cmake.unproven_name.signature" => Some("cmake-declaration-name-analysis-unavailable"),
        "cmake.unproven_parameter.signature" => Some("cmake-formal-argument-analysis-unavailable"),
        "cmake.unsupported_owner.signature" => Some("cmake-complete-written-owner-unavailable"),
        "cmake.unsupported_option_arguments.signature" => {
            Some("cmake-option-written-argument-shape-unavailable")
        }
        "cmake.control_flow.signature" => {
            Some("cmake-control-flow-and-variable-analysis-unavailable")
        }
        "cmake.configure_placeholder.signature" => {
            Some("cmake-configure-substitution-and-generated-syntax-unavailable")
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(detail) = owner_gap(fact) {
        return Some((FactDomain::Entities, detail));
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "cmake.file.module") => Some((
            FactDomain::Relations,
            "cmake-variable-command-condition-macro-and-template-evaluation-unavailable",
        )),
        (
            SyntaxFactKind::Declaration,
            "cmake.macro.declaration" | "cmake.macro_placeholder.declaration",
        ) => Some((
            FactDomain::Relations,
            "cmake-macro-expansion-and-placeholder-binding-unavailable",
        )),
        (SyntaxFactKind::Declaration, "cmake.target.declaration") => Some((
            FactDomain::Relations,
            "cmake-effective-target-configuration-unavailable",
        )),
        (SyntaxFactKind::Declaration, "cmake.option.declaration") => Some((
            FactDomain::Relations,
            "cmake-option-initialization-and-effective-cache-state-unavailable",
        )),
        (SyntaxFactKind::Signature, "cmake.unset_binding.signature") => Some((
            FactDomain::Occurrences,
            "cmake-unset-variable-write-unavailable",
        )),
        (SyntaxFactKind::Signature, "cmake.dynamic_evaluation.signature") => Some((
            FactDomain::Relations,
            "cmake-generated-source-and-dynamic-command-evaluation-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "cmake.command.call" | "cmake.template_command.call") => {
            Some("cmake-command-dispatch-target-unavailable")
        }
        (
            SyntaxFactKind::Occurrence,
            "cmake.normal_variable.reference"
            | "cmake.cache_variable.reference"
            | "cmake.environment_variable.reference",
        ) => Some("cmake-variable-evaluation-and-binding-unavailable"),
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "cmake.command.call" | "cmake.template_command.call") => {
            Some(OccurrenceRole::CallSite)
        }
        (
            SyntaxFactKind::Occurrence,
            "cmake.normal_variable.reference"
            | "cmake.cache_variable.reference"
            | "cmake.environment_variable.reference",
        ) => Some(OccurrenceRole::Reference),
        (SyntaxFactKind::Import, "cmake.written_import.import") => Some(OccurrenceRole::ImportUse),
        (SyntaxFactKind::Comment, "cmake.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "cmake.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
