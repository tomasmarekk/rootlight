//! CUE containment reflects written static fields, including constraint fields.
//! Unification, imported bindings and evaluation remain explicit domain gaps.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::Field && label == "cue.field.declaration"
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "cue.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "cue.unsupported.expression";
    let rows: &[(FactDomain, &'static str)] = if root {
        &[
            (
                FactDomain::Entities,
                "cue-dynamic-labels-unification-and-evaluated-fields-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "cue-imported-binding-and-general-reference-resolution-unavailable",
            ),
            (
                FactDomain::Relations,
                "cue-constraint-evaluation-comprehension-and-import-semantics-unavailable",
            ),
        ]
    } else if unsupported {
        &[(
            FactDomain::Entities,
            "cue-static-label-or-lexical-container-proof-unavailable",
        )]
    } else {
        &[]
    };
    rows.iter().copied()
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "cue.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "cue.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
