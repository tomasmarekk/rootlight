//! Written Pony owners and direct members provide physical containment.
//! Imports, receiver types, reference capabilities and dispatch remain explicit gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Class,
            "pony.class.declaration" | "pony.actor.declaration" | "pony.primitive.declaration"
        ) | (EntityKind::Struct, "pony.struct.declaration")
            | (EntityKind::Trait, "pony.trait.declaration")
            | (EntityKind::Interface, "pony.interface.declaration")
            | (
                EntityKind::Method,
                "pony.method.declaration" | "pony.behavior.declaration"
            )
            | (EntityKind::Constructor, "pony.constructor.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "pony.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "pony.unsupported.expression";
    [
        root.then_some((
            FactDomain::Entities,
            "pony-fields-parameters-aliases-local-and-inferred-declarations-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "pony-nondefinition-variable-type-and-member-occurrences-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "pony-import-type-composition-receiver-capability-and-dispatch-analysis-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Entities,
            "pony-written-name-or-static-owner-proof-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "pony.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "pony.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "pony.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
