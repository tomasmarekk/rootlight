//! HLSL facts classify written declarations and preserve exact ASCII name spelling.
//! Native callers prove complete captures, physical owners and source provenance;
//! buffer labels and annotations do not establish entities or shader bindings.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies only exact reviewed physical file and declaration labels.
///
/// Constant-buffer labels and contexts are not declarations, types or scopes.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "hlsl.file.module" {
        return Some(EntityKind::Module);
    }
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "hlsl.function.declaration" | "hlsl.prototype.declaration" => Some(EntityKind::Function),
        "hlsl.variable.declaration"
        | "hlsl.variable_fragment.declaration"
        | "hlsl.buffer_member.declaration"
        | "hlsl.buffer_member_fragment.declaration" => Some(EntityKind::Variable),
        "hlsl.parameter.declaration" => Some(EntityKind::Parameter),
        "hlsl.field.declaration" | "hlsl.field_fragment.declaration" => Some(EntityKind::Field),
        "hlsl.struct.declaration" => Some(EntityKind::Struct),
        _ => None,
    }
}

/// Borrows a complete bounded ASCII identifier without trimming or case folding.
///
/// The caller independently proves a native name capture and its physical context.
/// This lexical boundary does not prove reserved-word acceptance, shader binding,
/// declaration ownership or generation freshness.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    (bytes.len() <= maximum_bytes
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_'))
    .then_some(text)
}

/// Borrows raw names only for exact definition and unresolved source-use labels.
///
/// Name admission does not resolve a call or member target. The caller proves
/// capture context and source association; buffer labels are not name occurrences.
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "hlsl.name.definition"
        | "hlsl.identifier.reference"
        | "hlsl.member.reference"
        | "hlsl.unproven_call.reference" => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;
    use crate::{
        SyntaxKindLabel, structural_captured_name_for_fact, structural_captured_name_for_language,
        structural_entity_kind, structural_entity_kind_from_source,
    };

    const FACT_KINDS: [SyntaxFactKind; 11] = [
        SyntaxFactKind::Root,
        SyntaxFactKind::Module,
        SyntaxFactKind::Declaration,
        SyntaxFactKind::Signature,
        SyntaxFactKind::Import,
        SyntaxFactKind::Scope,
        SyntaxFactKind::Occurrence,
        SyntaxFactKind::Comment,
        SyntaxFactKind::StringLiteral,
        SyntaxFactKind::EmbeddedRegion,
        SyntaxFactKind::ErrorRecovery,
    ];

    const DECLARATIONS: [(&str, EntityKind); 10] = [
        ("hlsl.function.declaration", EntityKind::Function),
        ("hlsl.prototype.declaration", EntityKind::Function),
        ("hlsl.variable.declaration", EntityKind::Variable),
        ("hlsl.variable_fragment.declaration", EntityKind::Variable),
        ("hlsl.buffer_member.declaration", EntityKind::Variable),
        (
            "hlsl.buffer_member_fragment.declaration",
            EntityKind::Variable,
        ),
        ("hlsl.parameter.declaration", EntityKind::Parameter),
        ("hlsl.field.declaration", EntityKind::Field),
        ("hlsl.field_fragment.declaration", EntityKind::Field),
        ("hlsl.struct.declaration", EntityKind::Struct),
    ];

    const OCCURRENCES: [&str; 4] = [
        "hlsl.name.definition",
        "hlsl.identifier.reference",
        "hlsl.member.reference",
        "hlsl.unproven_call.reference",
    ];

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([6; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn exact_hlsl_owner_labels_have_written_physical_kinds() {
        for (label, expected) in DECLARATIONS {
            let declaration = fact(SyntaxFactKind::Declaration, label);
            assert_eq!(entity_kind(&declaration), Some(expected), "{label}");
            assert_eq!(
                structural_entity_kind(&declaration),
                Some(expected),
                "{label}"
            );
            assert_eq!(
                structural_entity_kind_from_source(&declaration, "written source"),
                Some(expected),
                "{label}"
            );
        }
        let module = fact(SyntaxFactKind::Module, "hlsl.file.module");
        assert_eq!(entity_kind(&module), Some(EntityKind::Module));
        assert_eq!(structural_entity_kind(&module), Some(EntityKind::Module));
    }

    #[test]
    fn hlsl_owners_require_exact_declaration_and_module_roles() {
        for kind in FACT_KINDS {
            for (label, expected) in DECLARATIONS {
                let expected = (kind == SyntaxFactKind::Declaration).then_some(expected);
                let owner = fact(kind, label);
                assert_eq!(entity_kind(&owner), expected, "{kind:?}: {label}");
                assert_eq!(
                    structural_entity_kind(&owner),
                    expected,
                    "{kind:?}: {label}"
                );
            }
            let module = fact(kind, "hlsl.file.module");
            let expected = (kind == SyntaxFactKind::Module).then_some(EntityKind::Module);
            assert_eq!(entity_kind(&module), expected, "{kind:?}");
            assert_eq!(structural_entity_kind(&module), expected, "{kind:?}");
        }
    }

    #[test]
    fn unknown_hlsl_owner_labels_never_use_substring_classification() {
        for label in [
            "hlsl.unknown.declaration",
            "hlsl.function.declaration.extra",
            "hlsl.function.definition",
            "hlsl.extra_function.declaration",
            "hlsl.unknown_struct.declaration",
            "hlsl.class.declaration",
            "hlsl.interface.declaration",
            "hlsl.name.definition",
            "hlsl.file.declaration",
        ] {
            for kind in FACT_KINDS {
                let owner = fact(kind, label);
                assert_eq!(entity_kind(&owner), None, "{kind:?}: {label}");
                assert_eq!(structural_entity_kind(&owner), None, "{kind:?}: {label}");
            }
        }
        for label in [
            "hlsl.unknown.module",
            "hlsl.struct.module",
            "hlsl.file.module.extra",
        ] {
            assert_eq!(
                structural_entity_kind(&fact(SyntaxFactKind::Module, label)),
                None,
                "{label}"
            );
        }
        for label in [
            "glsl.function.declaration",
            "cpp.function.declaration",
            "hlsl_extra.function.declaration",
            "HLSL.function.declaration",
        ] {
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Declaration, label)), None);
        }
    }

    #[test]
    fn complete_hlsl_names_preserve_borrowed_ascii_spelling_and_limits() {
        for source in [
            "_",
            "a",
            "Z",
            "_Written9",
            "MixedCase",
            "mixedcase",
            "hlsl_Value",
        ] {
            let accepted = captured_name(source, source.len()).unwrap();
            assert_eq!(accepted, source);
            assert!(std::ptr::eq(accepted.as_ptr(), source.as_ptr()));
            assert_eq!(captured_name(source, usize::MAX), Some(source));
            assert_eq!(captured_name(source, source.len() - 1), None);
            assert_eq!(captured_name(source, 0), None);
            let Some(Cow::Borrowed(public)) =
                structural_captured_name_for_language("hlsl", source, source.len())
            else {
                panic!("complete HLSL spelling must remain borrowed: {source}");
            };
            assert!(std::ptr::eq(public.as_ptr(), source.as_ptr()));
        }
        assert_ne!(captured_name("Mixed", 5), captured_name("mixed", 5));
    }

    #[test]
    fn hlsl_name_admission_never_trims_decodes_or_extracts_substrings() {
        for source in [
            "",
            "9name",
            " name",
            "name ",
            "two names",
            "name\t",
            "name\n",
            "name\0tail",
            "name.field",
            "name[0]",
            "name()",
            "name-name",
            "name/name",
            "\\name",
            "name::member",
            "register(t0)",
            ":SV_Target",
            "λ",
            "éname",
            "nameé",
        ] {
            assert_eq!(captured_name(source, usize::MAX), None, "{source:?}");
            assert!(structural_captured_name_for_language("hlsl", source, usize::MAX).is_none());
        }
        assert_eq!(captured_name("", 0), None);
    }

    #[test]
    fn exact_hlsl_occurrences_borrow_only_the_complete_name() {
        for label in OCCURRENCES {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            let source = "Written9";
            let accepted = captured_name_for_fact(&occurrence, source, source.len()).unwrap();
            assert!(std::ptr::eq(accepted.as_ptr(), source.as_ptr()));
            let Some(Cow::Borrowed(public)) =
                structural_captured_name_for_fact("hlsl", &occurrence, source, source.len())
            else {
                panic!("exact HLSL occurrence must retain borrowed spelling: {label}");
            };
            assert_eq!(public, source);
            assert!(std::ptr::eq(public.as_ptr(), source.as_ptr()));
            assert_eq!(captured_name_for_fact(&occurrence, source, 0), None);
            assert_eq!(
                captured_name_for_fact(&occurrence, source, source.len() - 1),
                None
            );
            for invalid in ["Written9()", " Written9", "Written9 ", "λ", ""] {
                assert!(
                    structural_captured_name_for_fact("hlsl", &occurrence, invalid, 64).is_none(),
                    "{label}: {invalid:?}"
                );
            }
        }
    }

    #[test]
    fn hlsl_occurrences_require_exact_role_label_and_language() {
        for label in OCCURRENCES {
            for kind in FACT_KINDS {
                if kind != SyntaxFactKind::Occurrence {
                    let wrong_role = fact(kind, label);
                    assert_eq!(captured_name_for_fact(&wrong_role, "Written", 64), None);
                    assert!(
                        structural_captured_name_for_fact("hlsl", &wrong_role, "Written", 64)
                            .is_none(),
                        "{kind:?}: {label}"
                    );
                }
            }
            for foreign in ["c", "cpp", "glsl", "sfapex", "rust", "HLSL"] {
                assert!(
                    structural_captured_name_for_fact(
                        foreign,
                        &fact(SyntaxFactKind::Occurrence, label),
                        "Written",
                        64
                    )
                    .is_none(),
                    "{foreign}: {label}"
                );
            }
        }
        for label in [
            "hlsl.unknown.reference",
            "hlsl.name.reference",
            "hlsl.identifier.definition",
            "hlsl.member.reference.extra",
            "hlsl.unproven_call.call",
            "hlsl.name.definition.extra",
            "hlsl.function.declaration",
            "glsl.name.definition",
            "cpp.name.definition",
            "hlsl_extra.name.definition",
            "HLSL.name.definition",
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(captured_name_for_fact(&occurrence, "Written", 64), None);
            assert!(
                structural_captured_name_for_fact("hlsl", &occurrence, "Written", 64).is_none(),
                "{label}"
            );
        }
    }

    #[test]
    fn buffer_labels_and_contexts_never_become_entities_or_name_occurrences() {
        for label in [
            "hlsl.cbuffer.declaration",
            "hlsl.buffer.declaration",
            "hlsl.buffer_label.definition",
            "hlsl.buffer_context.expression",
            "hlsl.buffer_context.scope",
            "hlsl.buffer.struct.declaration",
            "hlsl.buffer.function.declaration",
            "hlsl.buffer_name.reference",
            "hlsl.buffer.module",
        ] {
            for kind in FACT_KINDS {
                let context = fact(kind, label);
                assert_eq!(structural_entity_kind(&context), None, "{kind:?}: {label}");
                assert_eq!(
                    structural_entity_kind_from_source(&context, "cbuffer Written {}"),
                    None,
                    "{kind:?}: {label}"
                );
                assert!(
                    structural_captured_name_for_fact("hlsl", &context, "Written", 64).is_none(),
                    "{kind:?}: {label}"
                );
                assert!(
                    structural_captured_name_for_fact("rust", &context, "Written", 64).is_none(),
                    "{kind:?}: {label}"
                );
            }
        }
    }
}
