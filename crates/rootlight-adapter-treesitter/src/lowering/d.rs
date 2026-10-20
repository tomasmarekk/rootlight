//! D lowering retains only explicit lexical headers, without compiler evaluation.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};
pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "d.module.declaration")
            | (EntityKind::Function, "d.function.declaration")
            | (EntityKind::Parameter, "d.parameter.declaration")
    )
}
pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature && fact.syntax_kind().as_str() == "d.header.signature"
}
pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "d.file.module"
    {
        &[
            (
                FactDomain::Entities,
                "d-aggregate-variable-template-mixin-and-generated-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "d-import-type-and-general-reference-binding-unavailable",
            ),
            (
                FactDomain::Relations,
                "d-template-ctfe-type-compiler-binding-and-resolved-calls-unavailable",
            ),
        ]
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "d.unsupported.expression"
    {
        &[(
            FactDomain::Entities,
            "d-static-header-or-written-owner-proof-unavailable",
        )]
    } else {
        &[]
    };
    rows.iter().copied()
}
pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "d.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "d.string.string") => Some(OccurrenceRole::StringEvidence),
        _ => None,
    }
}
