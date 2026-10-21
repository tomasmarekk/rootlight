//! Pug identities describe only explicit static source tag and attribute names.
//! Native callers prove complete clean syntax and the exact physical owner chain.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "pug.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "pug.element.declaration") => Some(EntityKind::MarkupElement),
        (SyntaxFactKind::Declaration, "pug.attribute.declaration") => {
            Some(EntityKind::MarkupAttribute)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let (first, tail) = text.as_bytes().split_first()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || *first == b'_')
        && tail
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':')))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "pug.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
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
            SourceSpan::new(FileId::from_bytes([5; 20]), 0, 5).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn closed_written_labels_do_not_promote_foreign_roles_or_reference_names() {
        for (label, kind) in [
            ("pug.element.declaration", Some(EntityKind::MarkupElement)),
            (
                "pug.attribute.declaration",
                Some(EntityKind::MarkupAttribute),
            ),
            ("pug.implicit_or_unavailable_owner.signature", None),
            ("html.element.declaration", None),
        ] {
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Declaration, label)), kind);
        }
        let definition = fact(SyntaxFactKind::Occurrence, "pug.name.definition");
        assert_eq!(
            captured_name_for_fact(&definition, "input", 5),
            Some("input")
        );
        assert_eq!(
            super::super::structural_captured_name_for_fact("html", &definition, "input", 5),
            None
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Declaration, "pug.name.definition"),
                "input",
                5
            ),
            None
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "pug.name.reference"),
                "input",
                5
            ),
            None
        );
    }

    #[test]
    fn static_names_are_exact_and_dynamic_or_quoted_forms_remain_unavailable() {
        for name in ["input", "disabled", "data-value", "xml:lang", "_written"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "div.class",
            "#id",
            "@click",
            "[dynamic]",
            "'text'",
            "#{name}",
            "a b",
            "λ",
        ] {
            assert_eq!(captured_name(name, 128), None);
        }
    }
}
