//! Written Zig binders provide definitions and lexical physical containment.
//! Type evaluation, imports, receiver binding and calls remain explicit gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "zig.function.declaration")
            | (EntityKind::Variable, "zig.variable.declaration")
            | (EntityKind::Constant, "zig.constant.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "zig.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "zig.unsupported.expression";
    [
        root.then_some((
            FactDomain::Entities,
            "zig-container-parameter-quoted-comptime-and-inferred-declarations-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "zig-nondefinition-variable-type-member-and-builtin-occurrences-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "zig-import-type-comptime-receiver-and-call-analysis-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Entities,
            "zig-static-binder-or-written-owner-proof-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "zig.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "zig.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "zig.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
