//! Written VHDL declarations support physical identity and containment only.
//! Library, overload, type, elaboration and dialect completeness remain unknown.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "vhdl.entity.declaration")
            | (
                EntityKind::Namespace,
                "vhdl.architecture.declaration" | "vhdl.package.declaration"
            )
            | (EntityKind::Interface, "vhdl.component.declaration")
            | (
                EntityKind::Field,
                "vhdl.port.declaration" | "vhdl.record_field.declaration"
            )
            | (
                EntityKind::Constant,
                "vhdl.generic.declaration" | "vhdl.constant.declaration"
            )
            | (EntityKind::Parameter, "vhdl.argument.declaration")
            | (
                EntityKind::Function,
                "vhdl.function.declaration" | "vhdl.procedure.declaration"
            )
            | (
                EntityKind::TypeAlias,
                "vhdl.type.declaration" | "vhdl.subtype.declaration"
            )
            | (
                EntityKind::Variable,
                "vhdl.variable.declaration"
                    | "vhdl.signal.declaration"
                    | "vhdl.file_object.declaration"
            )
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "vhdl.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "vhdl.unsupported.signature";
    [
        root.then_some((
            FactDomain::Entities,
            "vhdl-additional-declarations-and-dialect-analysis-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "vhdl-nondefinition-occurrence-analysis-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "vhdl-library-type-overload-and-elaboration-binding-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Entities,
            "vhdl-declaration-name-or-owner-proof-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Occurrences,
            "vhdl-declaration-name-or-owner-proof-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Relations,
            "vhdl-declaration-name-or-owner-proof-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}
