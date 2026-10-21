//! Preserve source-owned GDScript declarations and explicit semantic limitations.
//! No call, imported type, local Read, or runtime target is inferred by this lane.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Class, "gdscript.class.declaration")
            | (EntityKind::Constructor, "gdscript.constructor.declaration")
            | (EntityKind::Method, "gdscript.method.declaration")
            | (
                EntityKind::Function,
                "gdscript.unowned_function.declaration"
            )
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "gdscript.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "gdscript.unresolved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "gdscript.comment.comment") => {
            Some(OccurrenceRole::Documentation)
        }
        (SyntaxFactKind::StringLiteral, "gdscript.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "gdscript.unresolved_name.reference")
        .then_some("gdscript-name-binding-type-and-call-target-unavailable")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "gdscript.source_file.root") => &[
            (
                FactDomain::Entities,
                "gdscript-parameters-properties-enums-nested-classes-and-imported-types-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "gdscript-read-write-type-and-dynamic-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "gdscript-calls-inheritance-import-and-runtime-binding-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "gdscript.unowned_function.declaration") => &[
            (
                FactDomain::Entities,
                "gdscript-explicit-file-class-name-owner-unavailable",
            ),
            (
                FactDomain::Relations,
                "gdscript-explicit-file-class-name-owner-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "gdscript.unsupported.declaration") => &[
            (
                FactDomain::Entities,
                "gdscript-written-declaration-shape-or-unique-owner-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "gdscript-written-definition-shape-or-unique-owner-unavailable",
            ),
            (FactDomain::Relations, "gdscript-written-owner-unavailable"),
        ],
        (SyntaxFactKind::Signature, "gdscript.construct_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "gdscript-source-construct-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "gdscript-source-construct-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "gdscript-source-construct-semantics-unavailable",
            ),
        ],
        _ => &[],
    };
    rows.iter().copied()
}
