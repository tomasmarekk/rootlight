//! Eiffel facts retain written identifiers and reviewed declaration categories.
//! Identifier spelling does not establish inheritance, renaming or call targets.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "eiffel.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "eiffel.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "eiffel.routine.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "eiffel.field.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "eiffel.constant.declaration") => Some(EntityKind::Constant),
        (SyntaxFactKind::Declaration, "eiffel.parameter.declaration") => {
            Some(EntityKind::Parameter)
        }
        (SyntaxFactKind::Declaration, "eiffel.local.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "eiffel.type_parameter.declaration") => {
            Some(EntityKind::TypeParameter)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    (text.len() <= maximum_bytes
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "eiffel.written_operator.call") => {
            // Native tokenization owns the operator alphabet and delimiter boundaries.
            (!text.is_empty()
                && text.len() <= maximum_bytes
                && !text
                    .chars()
                    .any(|character| character.is_control() || character.is_whitespace()))
            .then_some(text)
        }
        (
            SyntaxFactKind::Occurrence,
            "eiffel.name.definition"
            | "eiffel.written_call.call"
            | "eiffel.identifier.reference"
            | "eiffel.type.reference",
        ) => captured_name(text, maximum_bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn eiffel_declaration_categories_require_exact_reviewed_roles() {
        assert_eq!(
            crate::structural_entity_kind(&fact(SyntaxFactKind::Module, "eiffel.file.module")),
            Some(EntityKind::Module)
        );
        for (label, kind) in [
            ("eiffel.class.declaration", EntityKind::Class),
            ("eiffel.routine.declaration", EntityKind::Function),
            ("eiffel.field.declaration", EntityKind::Field),
            ("eiffel.constant.declaration", EntityKind::Constant),
            ("eiffel.parameter.declaration", EntityKind::Parameter),
            ("eiffel.local.declaration", EntityKind::Variable),
            (
                "eiffel.type_parameter.declaration",
                EntityKind::TypeParameter,
            ),
        ] {
            assert_eq!(
                crate::structural_entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            for other in [
                SyntaxFactKind::Module,
                SyntaxFactKind::Occurrence,
                SyntaxFactKind::Import,
            ] {
                assert_eq!(crate::structural_entity_kind(&fact(other, label)), None);
            }
            assert_eq!(
                crate::structural_entity_kind(&fact(
                    SyntaxFactKind::Declaration,
                    &format!("{label}.extra")
                )),
                None
            );
        }
        assert_eq!(
            crate::structural_entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "eiffel.unreviewed_function.declaration"
            )),
            None
        );
    }

    #[test]
    fn eiffel_native_identifiers_preserve_case_and_byte_limits() {
        for name in ["BOX", "Box", "box", "_item", "count_2", "G"] {
            assert_eq!(
                crate::structural_captured_name_for_language("eiffel", name, name.len()).as_deref(),
                Some(name)
            );
            assert!(
                crate::structural_captured_name_for_language("eiffel", name, name.len() - 1)
                    .is_none()
            );
        }
        for name in [
            "",
            "2name",
            "name.member",
            "name ()",
            "\"+\"",
            "+",
            "λ",
            "x\0",
            "x\n",
        ] {
            assert!(
                crate::structural_captured_name_for_language("eiffel", name, usize::MAX).is_none()
            );
        }
    }

    #[test]
    fn eiffel_occurrence_names_cannot_cross_namespace_or_role() {
        for label in [
            "eiffel.name.definition",
            "eiffel.written_call.call",
            "eiffel.identifier.reference",
            "eiffel.type.reference",
        ] {
            let occurrence = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                crate::structural_captured_name_for_fact("eiffel", &occurrence, "Written", 7)
                    .as_deref(),
                Some("Written")
            );
            assert!(
                crate::structural_captured_name_for_fact("rust", &occurrence, "Written", 7)
                    .is_none()
            );
            assert!(
                crate::structural_captured_name_for_fact("eiffel", &occurrence, "Written", 6)
                    .is_none()
            );
            for other in [
                SyntaxFactKind::Declaration,
                SyntaxFactKind::Signature,
                SyntaxFactKind::Import,
            ] {
                assert!(
                    crate::structural_captured_name_for_fact(
                        "eiffel",
                        &fact(other, label),
                        "Written",
                        7
                    )
                    .is_none()
                );
            }
        }
        for label in [
            "eiffel.unknown.reference",
            "eiffel.name.definition.extra",
            "rust.name.definition",
        ] {
            assert!(
                crate::structural_captured_name_for_fact(
                    "eiffel",
                    &fact(SyntaxFactKind::Occurrence, label),
                    "Written",
                    7
                )
                .is_none()
            );
        }
    }

    #[test]
    fn eiffel_written_operator_names_require_the_reviewed_occurrence_role() {
        let occurrence = fact(SyntaxFactKind::Occurrence, "eiffel.written_operator.call");
        for name in ["∋", "⊆", "-*", "#abc"] {
            assert_eq!(
                crate::structural_captured_name_for_fact("eiffel", &occurrence, name, name.len())
                    .as_deref(),
                Some(name)
            );
            assert!(
                crate::structural_captured_name_for_fact(
                    "eiffel",
                    &occurrence,
                    name,
                    name.len() - 1
                )
                .is_none()
            );
            assert!(
                crate::structural_captured_name_for_fact("rust", &occurrence, name, name.len())
                    .is_none()
            );
            assert!(
                crate::structural_captured_name_for_fact(
                    "eiffel",
                    &fact(SyntaxFactKind::Declaration, "eiffel.written_operator.call"),
                    name,
                    name.len()
                )
                .is_none()
            );
        }
        for name in ["", "∋ ", "∋\n", "∋\0"] {
            assert!(
                crate::structural_captured_name_for_fact("eiffel", &occurrence, name, usize::MAX)
                    .is_none()
            );
        }
    }
}
