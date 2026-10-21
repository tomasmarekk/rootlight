//! GLSL facts describe written owners and preserve exact ASCII name spelling.
//! Native callers prove captures and generation-bound source evidence;
//! interface blocks do not establish linked shader or runtime bindings.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

/// Classifies only the reviewed physical file and declaration labels.
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "glsl.file.module" {
        return Some(EntityKind::Module);
    }
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "glsl.function.declaration" | "glsl.prototype.declaration" => Some(EntityKind::Function),
        "glsl.variable.declaration"
        | "glsl.variable_fragment.declaration"
        | "glsl.interface_instance.declaration" => Some(EntityKind::Variable),
        "glsl.parameter.declaration" => Some(EntityKind::Parameter),
        "glsl.field.declaration" | "glsl.field_fragment.declaration" => Some(EntityKind::Field),
        "glsl.struct.declaration" | "glsl.interface_block.declaration" => Some(EntityKind::Struct),
        _ => None,
    }
}

/// Borrows a complete bounded ASCII identifier without trimming or case folding.
///
/// Callers independently prove a native identifier capture and its source context.
/// This boundary rejects the exact `discard` keyword, but does not prove broader
/// reserved-word acceptance, shader linkage, binding or generation freshness.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    (bytes.len() <= maximum_bytes
        && text != "discard"
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_'))
    .then_some(text)
}

/// Admits only reviewed GLSL occurrence labels and returns their raw borrowed name.
///
/// A retained name does not resolve a call or member target. The caller owns
/// declaration context, source span validity and generation-bound provenance.
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "glsl.name.definition"
        | "glsl.identifier.reference"
        | "glsl.member.reference"
        | "glsl.unproven_call.reference"
        | "glsl.formal_value.reference"
        | "glsl.direct_file_function.reference" => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;
    use crate::SyntaxKindLabel;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([5; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn exact_glsl_labels_have_written_physical_kinds() {
        for (label, expected) in [
            ("glsl.function.declaration", EntityKind::Function),
            ("glsl.prototype.declaration", EntityKind::Function),
            ("glsl.variable.declaration", EntityKind::Variable),
            ("glsl.variable_fragment.declaration", EntityKind::Variable),
            ("glsl.interface_instance.declaration", EntityKind::Variable),
            ("glsl.parameter.declaration", EntityKind::Parameter),
            ("glsl.field.declaration", EntityKind::Field),
            ("glsl.field_fragment.declaration", EntityKind::Field),
            ("glsl.struct.declaration", EntityKind::Struct),
            ("glsl.interface_block.declaration", EntityKind::Struct),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(expected),
                "{label}"
            );
        }
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Module, "glsl.file.module")),
            Some(EntityKind::Module)
        );
    }

    #[test]
    fn glsl_owner_labels_require_their_exact_fact_roles() {
        for kind in [
            SyntaxFactKind::Root,
            SyntaxFactKind::Module,
            SyntaxFactKind::Signature,
            SyntaxFactKind::Import,
            SyntaxFactKind::Scope,
            SyntaxFactKind::Occurrence,
            SyntaxFactKind::Comment,
            SyntaxFactKind::StringLiteral,
            SyntaxFactKind::EmbeddedRegion,
            SyntaxFactKind::ErrorRecovery,
        ] {
            assert_eq!(
                entity_kind(&fact(kind, "glsl.function.declaration")),
                None,
                "{kind:?}"
            );
            if kind != SyntaxFactKind::Module {
                assert_eq!(
                    entity_kind(&fact(kind, "glsl.file.module")),
                    None,
                    "{kind:?}"
                );
            }
        }
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Declaration, "glsl.file.module")),
            None
        );
    }

    #[test]
    fn unknown_glsl_labels_and_foreign_namespaces_never_fall_through() {
        for label in [
            "glsl.unknown.declaration",
            "glsl.function.declaration.extra",
            "glsl.function.definition",
            "glsl.interface_block.scope",
            "glsl.struct.module",
            "glsl.name.definition",
            "glsl_extra.function.declaration",
            "GLSL.function.declaration",
            "rust.function.declaration",
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                None,
                "{label}"
            );
        }
        for label in [
            "glsl.unknown.module",
            "glsl.interface_block.module",
            "glsl.file.module.extra",
            "glsl.file.declaration",
            "unknown.file.module",
            "glsl_extra.file.module",
            "GLSL.file.module",
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Module, label)),
                None,
                "{label}"
            );
        }
    }

    #[test]
    fn glsl_names_preserve_complete_borrowed_ascii_spelling_and_byte_limits() {
        for source in [
            "_",
            "a",
            "Z",
            "_Written9",
            "MixedCase",
            "mixedcase",
            "gl_Value",
        ] {
            let accepted = captured_name(source, source.len()).unwrap();
            assert_eq!(accepted, source);
            assert!(std::ptr::eq(accepted.as_ptr(), source.as_ptr()));
            assert_eq!(captured_name(source, usize::MAX), Some(source));
            assert_eq!(captured_name(source, source.len() - 1), None);
            assert_eq!(captured_name(source, 0), None);
        }
        assert_ne!(captured_name("Mixed", 5), captured_name("mixed", 5));
    }

    #[test]
    fn malformed_names_do_not_trim_decode_or_extract_substrings() {
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
            "λ",
            "éname",
            "nameé",
        ] {
            assert_eq!(captured_name(source, usize::MAX), None, "{source:?}");
        }
        assert_eq!(captured_name("", 0), None);
    }

    #[test]
    fn discard_keyword_is_rejected_without_case_folding_other_spellings() {
        assert_eq!(captured_name("discard", 7), None);
        for source in ["Discard", "DISCARD", "discarded", "_discard"] {
            assert_eq!(captured_name(source, source.len()), Some(source));
        }
    }

    #[test]
    fn reviewed_glsl_occurrences_borrow_only_their_complete_name() {
        for label in [
            "glsl.name.definition",
            "glsl.identifier.reference",
            "glsl.member.reference",
            "glsl.unproven_call.reference",
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            let source = "Written9";
            let accepted = captured_name_for_fact(&occurrence, source, source.len()).unwrap();
            assert_eq!(accepted, source);
            assert!(std::ptr::eq(accepted.as_ptr(), source.as_ptr()));
            assert_eq!(
                captured_name_for_fact(&occurrence, source, usize::MAX),
                Some(source)
            );
            assert_eq!(captured_name_for_fact(&occurrence, source, 0), None);
            assert_eq!(
                captured_name_for_fact(&occurrence, source, source.len() - 1),
                None,
                "{label}"
            );
            for invalid in ["discard", "Written9()", " Written9", "λ", ""] {
                assert_eq!(
                    captured_name_for_fact(&occurrence, invalid, 64),
                    None,
                    "{label}: {invalid:?}"
                );
            }
        }
    }

    #[test]
    fn glsl_name_admission_requires_exact_occurrence_role_and_label() {
        for kind in [
            SyntaxFactKind::Root,
            SyntaxFactKind::Module,
            SyntaxFactKind::Declaration,
            SyntaxFactKind::Signature,
            SyntaxFactKind::Import,
            SyntaxFactKind::Scope,
            SyntaxFactKind::Comment,
            SyntaxFactKind::StringLiteral,
            SyntaxFactKind::EmbeddedRegion,
            SyntaxFactKind::ErrorRecovery,
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(kind, "glsl.name.definition"), "Written", 64),
                None,
                "{kind:?}"
            );
        }
        for label in [
            "glsl.unknown.reference",
            "glsl.name.reference",
            "glsl.identifier.definition",
            "glsl.member.reference.extra",
            "glsl.unproven_call.call",
            "glsl.name.definition.extra",
            "glsl.function.declaration",
            "rust.name.definition",
            "glsl_extra.name.definition",
            "GLSL.name.definition",
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "Written", 64),
                None,
                "{label}"
            );
        }
    }

    #[test]
    fn public_glsl_dispatch_preserves_namespace_and_borrowed_names() {
        use std::borrow::Cow;

        use crate::{
            structural_captured_name_for_fact, structural_captured_name_for_language,
            structural_entity_kind,
        };

        let name = "Written";
        for label in [
            "glsl.name.definition",
            "glsl.identifier.reference",
            "glsl.member.reference",
            "glsl.unproven_call.reference",
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                structural_captured_name_for_fact("glsl", &occurrence, name, name.len()),
                Some(Cow::Borrowed(name))
            );
            for foreign in ["c", "cpp", "rust", "GLSL"] {
                assert!(
                    structural_captured_name_for_fact(foreign, &occurrence, name, 64).is_none()
                );
            }
        }
        for label in [
            "glsl.unknown.declaration",
            "glsl.function.declaration.extra",
        ] {
            assert!(structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)).is_none());
        }
        assert!(
            structural_captured_name_for_fact(
                "glsl",
                &fact(SyntaxFactKind::Occurrence, "rust.name.definition"),
                name,
                64
            )
            .is_none()
        );
        assert_eq!(
            structural_captured_name_for_language("glsl", name, name.len()),
            Some(Cow::Borrowed(name))
        );
        assert!(structural_captured_name_for_language("glsl", " Written ", 64).is_none());
    }
}

#[cfg(test)]
mod written_binding_label_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn native_binding_labels_admit_only_exact_occurrences_and_borrow_whole_names() {
        for label in [
            "glsl.formal_value.reference",
            "glsl.direct_file_function.reference",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 5).unwrap(),
                1,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(captured_name_for_fact(&fact, "value", 5), Some("value"));
            assert_eq!(captured_name_for_fact(&fact, "value", 4), None);
            let foreign = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                fact.span(),
                1,
                SyntaxKindLabel::new(&format!("{label}.extra")).unwrap(),
            );
            assert_eq!(captured_name_for_fact(&foreign, "value", 5), None);
            let wrong_role = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Signature,
                fact.span(),
                1,
                fact.syntax_kind().clone(),
            );
            assert_eq!(captured_name_for_fact(&wrong_role, "value", 5), None);
        }
    }
}
