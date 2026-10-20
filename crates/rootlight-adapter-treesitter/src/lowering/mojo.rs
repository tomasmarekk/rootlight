//! Project written Mojo identities without inventing field or method bindings.
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::{EntityKind, FactDomain, OccurrenceRole};

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Struct, "mojo.struct.declaration")
            | (EntityKind::Class, "mojo.class.declaration")
            | (EntityKind::Method, "mojo.method.declaration")
            | (EntityKind::Function, "mojo.function.declaration")
            | (EntityKind::Field, "mojo.field.declaration")
            | (EntityKind::Parameter, "mojo.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "mojo.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "mojo.self_field_read.reference") => {
            Some(OccurrenceRole::Read)
        }
        (SyntaxFactKind::Occurrence, "mojo.self_method_call.call") => {
            Some(OccurrenceRole::CallSite)
        }
        (SyntaxFactKind::Occurrence, "mojo.unresolved_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "mojo.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "mojo.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "mojo.file.module") => &[
            (
                FactDomain::Entities,
                "mojo-only-ascii-clean-written-types-methods-functions-fields-and-ordinary-formals-supported",
            ),
            (
                FactDomain::Occurrences,
                "mojo-field-reads-writes-and-general-binding-unavailable",
            ),
            (
                FactDomain::Relations,
                "mojo-method-dispatch-import-types-generic-evaluation-and-runtime-semantics-unavailable",
            ),
        ],
        (SyntaxFactKind::Declaration, "mojo.unsupported.declaration") => &[
            (
                FactDomain::Entities,
                "mojo-native-declaration-or-owner-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "mojo-native-definition-unavailable",
            ),
            (FactDomain::Relations, "mojo-native-owner-unavailable"),
        ],
        (SyntaxFactKind::Signature, "mojo.construct_unavailable.expression") => &[
            (
                FactDomain::Entities,
                "mojo-recovered-or-generated-declarations-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "mojo-recovered-or-expanded-occurrences-unavailable",
            ),
            (
                FactDomain::Relations,
                "mojo-construct-binding-and-runtime-semantics-unavailable",
            ),
        ],
        _ => &[],
    };
    rows.iter().copied()
}
