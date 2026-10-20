//! HCL containment describes written properties, without evaluating Terraform.

use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::Property && label == "hcl.property.declaration"
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "hcl.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "hcl.unsupported.expression";
    let rows: &[(FactDomain, &'static str)] = if root {
        &[
            (
                FactDomain::Entities,
                "hcl-quoted-computed-dynamic-and-generated-labels-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "hcl-variable-import-provider-and-general-reference-binding-unavailable",
            ),
            (
                FactDomain::Relations,
                "hcl-expression-template-module-provider-and-runtime-evaluation-unavailable",
            ),
        ]
    } else if unsupported {
        &[(
            FactDomain::Entities,
            "hcl-static-key-or-written-container-proof-unavailable",
        )]
    } else {
        &[]
    };
    rows.iter().copied()
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, "hcl.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "hcl.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}
