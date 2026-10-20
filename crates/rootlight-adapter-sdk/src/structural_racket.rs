//! Racket facts preserve bounded, case-sensitive written binding spellings.
//! Escaped symbols and reader transformations require separate reader evidence.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "racket.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "racket.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "racket.variable.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "racket.parameter.declaration") => {
            Some(EntityKind::Parameter)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut characters = text.chars();
    let first = characters.next()?;
    if text.len() > maximum || first.is_ascii_digit() || matches!(first, '#' | '.' | '\\' | '|') {
        return None;
    }
    text.chars()
        .all(|character| {
            !character.is_whitespace()
                && !character.is_control()
                && !matches!(
                    character,
                    '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ',' | ';' | '\\' | '|'
                )
        })
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
            "racket.name.definition" | "racket.call.reference"
        ))
    .then(|| captured_name(text, maximum))
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn racket_names_preserve_raw_case_unicode_predicates_and_path_punctuation() {
        for name in [
            "whitespace-base",
            "whitespace?",
            "whitespace/nbsp?",
            "Mixed",
            "λ",
            "+",
            "set!",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "1",
            "#ci",
            "#%app",
            ".",
            "|name|",
            r"escaped\ name",
            "(name)",
            "a b",
            "a\0b",
        ] {
            assert_eq!(captured_name(name, 128), None, "{name:?}");
        }
        assert_ne!(captured_name("Mixed", 64), captured_name("mixed", 64));
    }

    #[test]
    fn racket_fact_labels_are_exact_and_documentary_facts_do_not_become_names() {
        for kind in [
            SyntaxFactKind::Declaration,
            SyntaxFactKind::Occurrence,
            SyntaxFactKind::Comment,
            SyntaxFactKind::StringLiteral,
        ] {
            for label in [
                "racket.name.definition",
                "racket.call.reference",
                "racket.name.reference",
                "scheme.name.definition",
            ] {
                let fact = SyntaxFact::new(
                    1,
                    None,
                    kind,
                    SourceSpan::new(FileId::from_bytes([1; 20]), 0, 1).unwrap(),
                    0,
                    SyntaxKindLabel::new(label).unwrap(),
                );
                let expected = (kind == SyntaxFactKind::Occurrence
                    && matches!(label, "racket.name.definition" | "racket.call.reference"))
                .then_some("written?");
                assert_eq!(captured_name_for_fact(&fact, "written?", 64), expected);
            }
        }
    }
}
