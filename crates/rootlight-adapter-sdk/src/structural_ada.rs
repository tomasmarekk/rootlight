//! Bare Ada identifiers preserve source spelling; lexical lookup folds ASCII case.
//! Extended identifiers and compiler-dependent names require separate proof.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "ada.file.module")
        | (SyntaxFactKind::Declaration, "ada.package.declaration") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "ada.procedure.declaration" | "ada.function.declaration") => {
            Some(EntityKind::Function)
        }
        (SyntaxFactKind::Declaration, "ada.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    (text.len() <= maximum
        && bytes.next()?.is_ascii_alphabetic()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !text.ends_with('_')
        && !text.contains("__"))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "ada.name.definition" | "ada.direct_call.reference"
        ))
    .then(|| captured_name(text, maximum))
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_preserve_written_case_and_reject_unsupported_shapes() {
        for name in ["Raylib", "Draw_Triangle_Strip", "Points", "X1"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in [
            "",
            "_",
            "_First",
            "Last_",
            "Two__Parts",
            "a.b",
            "a b",
            "1A",
            "λ",
        ] {
            assert!(captured_name(name, 64).is_none(), "{name}");
        }
        assert_ne!(captured_name("Mixed", 5), captured_name("mixed", 5));
    }

    #[test]
    fn kind_admission_requires_the_exact_ada_label_and_fact_kind() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 6).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for (label, expected) in [
            ("ada.package.declaration", EntityKind::Module),
            ("ada.procedure.declaration", EntityKind::Function),
            ("ada.function.declaration", EntityKind::Function),
            ("ada.parameter.declaration", EntityKind::Parameter),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(expected)
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)), None);
        }
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "other.procedure.declaration"
            )),
            None
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "ada.name.definition"),
                "Raylib",
                6
            ),
            Some("Raylib")
        );
        assert!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "ada.end.reference"),
                "Raylib",
                6
            )
            .is_none()
        );
    }
}
