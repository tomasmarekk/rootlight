//! Reserve only the Wolfram producer's written delayed-function labels.
//! Spelling validates a complete native name; it supplies no lexical target,
//! runtime context, imported symbol, or computed declaration authority.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "wolfram.function.declaration" => Some(EntityKind::Function),
        "wolfram.parameter.declaration" => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let (first, tail) = text.as_bytes().split_first()?;
    (text.len() <= maximum
        && (*first == b'$' || first.is_ascii_alphabetic())
        && tail
            .iter()
            .all(|byte| *byte == b'$' || byte.is_ascii_alphanumeric()))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence
        || !matches!(
            fact.syntax_kind().as_str(),
            "wolfram.name.definition"
                | "wolfram.formal_argument.reference"
                | "wolfram.unproved_name.reference"
        )
    {
        return None;
    }
    captured_name(text, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn whole_written_names_do_not_admit_context_paths_patterns_or_expressions() {
        for word in ["owned", "$value", "argument2"] {
            assert_eq!(captured_name(word, word.len()), Some(word));
            assert!(captured_name(word, word.len() - 1).is_none());
        }
        for word in [
            "",
            "_",
            "value_String",
            "Private`value",
            "value()",
            "λ",
            "1value",
        ] {
            assert!(captured_name(word, 128).is_none());
        }
    }

    #[test]
    fn exact_reserved_fact_labels_are_required_for_every_name_and_entity() {
        let make = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([7; 20]), 0, 5).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        assert_eq!(
            entity_kind(&make(
                SyntaxFactKind::Declaration,
                "wolfram.function.declaration"
            )),
            Some(EntityKind::Function)
        );
        assert_eq!(
            entity_kind(&make(
                SyntaxFactKind::Declaration,
                "wolfram.parameter.declaration"
            )),
            Some(EntityKind::Parameter)
        );
        assert!(
            entity_kind(&make(
                SyntaxFactKind::Declaration,
                "wolfram.dynamic.declaration"
            ))
            .is_none()
        );
        for label in [
            "wolfram.name.definition",
            "wolfram.formal_argument.reference",
            "wolfram.unproved_name.reference",
        ] {
            assert_eq!(
                captured_name_for_fact(&make(SyntaxFactKind::Occurrence, label), "value", 5),
                Some("value")
            );
        }
        assert!(
            captured_name_for_fact(
                &make(SyntaxFactKind::Occurrence, "wolfram.foreign.reference"),
                "value",
                5
            )
            .is_none()
        );
    }
}
