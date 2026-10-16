//! HDL physical declarations retain ownership independently of elaborated hardware.
//! Written references never establish preprocessing, hierarchy or library binding.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

fn syntax(label: &str) -> Option<&str> {
    label
        .strip_prefix("verilog.")
        .or_else(|| label.strip_prefix("systemverilog."))
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, syntax(label).unwrap_or_default()),
        (
            EntityKind::Module,
            "module.declaration" | "primitive.declaration" | "program.declaration"
        ) | (EntityKind::Interface, "interface.declaration")
            | (EntityKind::Class, "class.declaration")
            | (
                EntityKind::Namespace,
                "package.declaration"
                    | "generate.declaration"
                    | "modport.declaration"
                    | "clocking.declaration"
            )
            | (
                EntityKind::Function,
                "function.declaration"
                    | "task.declaration"
                    | "checker.declaration"
                    | "property.declaration"
                    | "sequence.declaration"
                    | "let.declaration"
            )
            | (EntityKind::Field, "port.declaration")
            | (EntityKind::Parameter, "argument.declaration")
            | (
                EntityKind::Constant,
                "parameter.declaration" | "enumerator.declaration" | "macro.declaration"
            )
            | (
                EntityKind::Variable,
                "variable.declaration"
                    | "net.declaration"
                    | "instance.declaration"
                    | "genvar.declaration"
            )
            | (EntityKind::TypeAlias, "type.declaration")
            | (EntityKind::TypeParameter, "type_parameter.declaration")
    )
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Signature, "unsupported_owner.signature") => {
            Some("hdl-written-owner-header-unavailable")
        }
        (SyntaxFactKind::Signature, "unsupported_declaration.signature") => {
            Some("hdl-written-declaration-shape-unavailable")
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if let Some(reason) = owner_gap(fact) {
        return Some((FactDomain::Entities, reason));
    }
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Module, "file.module") => Some((
            FactDomain::Relations,
            "hdl-preprocessing-type-library-and-elaborated-hierarchy-binding-unavailable",
        )),
        (SyntaxFactKind::Signature, "preprocessor.signature") => Some((
            FactDomain::Entities,
            "hdl-preprocessor-activation-expansion-and-token-pasting-unavailable",
        )),
        (SyntaxFactKind::Signature, "generate_semantics.signature") => Some((
            FactDomain::Relations,
            "hdl-generate-activation-and-instance-multiplicity-unavailable",
        )),
        (SyntaxFactKind::Signature, "import_semantics.signature") => Some((
            FactDomain::Relations,
            "hdl-include-package-and-configuration-binding-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Occurrence, "written_call.call") => {
            Some("hdl-subroutine-dispatch-target-unavailable")
        }
        (SyntaxFactKind::Occurrence, "system_call.call") => {
            Some("hdl-system-task-function-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "identifier.reference") => {
            Some("hdl-identifier-hierarchy-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "type.reference") => {
            Some("hdl-instance-type-and-library-binding-unavailable")
        }
        (SyntaxFactKind::Occurrence, "macro.reference") => {
            Some("hdl-macro-definition-and-expansion-binding-unavailable")
        }
        _ => None,
    }
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), syntax(fact.syntax_kind().as_str())?) {
        (SyntaxFactKind::Occurrence, "written_call.call" | "system_call.call") => {
            Some(OccurrenceRole::CallSite)
        }
        (
            SyntaxFactKind::Occurrence,
            "identifier.reference" | "type.reference" | "macro.reference",
        ) => Some(OccurrenceRole::Reference),
        (SyntaxFactKind::Comment, "comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "string.string") => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}
