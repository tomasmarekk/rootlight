//! JSX names preserve complete native tag/attribute spelling and written case.
//! These identities describe markup syntax, never ECMAScript lexical bindings.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn owns(label: &str) -> bool {
    label.starts_with("typescript.jsx_") || label.starts_with("javascript.jsx_")
}

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "typescript.jsx_element.declaration" | "javascript.jsx_element.declaration" => {
            Some(EntityKind::MarkupElement)
        }
        "typescript.jsx_attribute.declaration" | "javascript.jsx_attribute.declaration" => {
            Some(EntityKind::MarkupAttribute)
        }
        _ => None,
    }
}

pub(super) fn captured_name<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    let matching = matches!(
        (language, fact.syntax_kind().as_str()),
        ("typescript", "typescript.jsx_name.definition")
            | ("javascript", "javascript.jsx_name.definition")
    );
    (fact.kind() == SyntaxFactKind::Occurrence
        && matching
        && !text.is_empty()
        && text.len() <= maximum
        && !text.contains('\\')
        && !text.chars().any(|c| c.is_control() || c.is_whitespace()))
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn jsx_written_names_are_closed_and_language_owned() {
        let fact = |kind, label: &str| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for language in ["typescript", "javascript"] {
            for (label, expected) in [
                ("element", EntityKind::MarkupElement),
                ("attribute", EntityKind::MarkupAttribute),
            ] {
                assert_eq!(
                    entity_kind(&fact(
                        SyntaxFactKind::Declaration,
                        &format!("{language}.jsx_{label}.declaration")
                    )),
                    Some(expected)
                );
            }
            let name = fact(
                SyntaxFactKind::Occurrence,
                &format!("{language}.jsx_name.definition"),
            );
            for raw in [
                "div",
                "UI.Card",
                "svg:path",
                "custom-element",
                "data-slot",
                "色",
            ] {
                assert_eq!(captured_name(language, &name, raw, 128), Some(raw));
                assert_eq!(captured_name("html", &name, raw, 128), None);
                assert_eq!(captured_name(language, &name, raw, raw.len() - 1), None);
            }
        }
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "typescript.jsx_unknown.declaration"
            )),
            None
        );
    }
}
