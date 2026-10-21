//! Closed written Meson labels from the retained native grammar.
//! Uncertified names remain references and cannot become definitions by spelling.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    (fact.kind() == SyntaxFactKind::Declaration
        && matches!(
            fact.syntax_kind().as_str(),
            "meson.assignment.declaration" | "meson.foreach_variable.declaration"
        ))
    .then_some(EntityKind::Variable)
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric()))
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
            "meson.name.definition"
                | "meson.adjacent_iterable.reference"
                | "meson.unproved_name.reference"
        ))
    .then(|| captured_name(text, maximum))
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SyntaxKindLabel, structural_captured_name_for_fact, structural_entity_kind};
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn closed_labels_require_whole_ordinary_names_and_exact_variable_declarations() {
        let make = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 5).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let declaration = make(SyntaxFactKind::Declaration, "meson.assignment.declaration");
        assert_eq!(
            structural_entity_kind(&declaration),
            Some(EntityKind::Variable)
        );
        for label in [
            "meson.name.definition",
            "meson.adjacent_iterable.reference",
            "meson.unproved_name.reference",
        ] {
            let fact = make(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                structural_captured_name_for_fact("meson", &fact, "items", 5).as_deref(),
                Some("items")
            );
            assert!(structural_captured_name_for_fact("meson", &fact, "items", 4).is_none());
            assert!(structural_captured_name_for_fact("python", &fact, "items", 5).is_none());
            for name in [
                "'items'",
                "item.member",
                "λ",
                "get_option('items')",
                "1name",
            ] {
                assert!(captured_name_for_fact(&fact, name, 128).is_none());
            }
        }
        assert!(
            structural_entity_kind(&make(
                SyntaxFactKind::Declaration,
                "meson.function.declaration"
            ))
            .is_none()
        );
        assert!(
            structural_captured_name_for_fact(
                "meson",
                &make(SyntaxFactKind::Occurrence, "meson.foreign.reference"),
                "items",
                5
            )
            .is_none()
        );
    }
}
