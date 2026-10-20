//! Crystal lexical owners and written methods provide physical containment.
//! Requires, types, macro expansion and dispatch remain explicit semantic gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "crystal.module.declaration")
            | (EntityKind::Class, "crystal.class.declaration")
            | (EntityKind::Struct, "crystal.struct.declaration")
            | (EntityKind::Enum, "crystal.enum.declaration")
            | (EntityKind::Method, "crystal.method.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root = fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "crystal.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "crystal.unsupported.expression";
    [
        root.then_some((
            FactDomain::Entities,
            "crystal-parameters-constants-aliases-local-and-inferred-declarations-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "crystal-nondefinition-variable-type-and-member-occurrences-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "crystal-require-type-inference-macro-evaluation-and-dispatch-analysis-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Entities,
            "crystal-written-name-or-static-owner-proof-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "crystal.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "crystal.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "crystal.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
