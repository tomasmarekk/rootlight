//! Classify written CUE field labels without evaluating their constraints.
//! Physical definition spans exclude optional/required markers and aliases.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "cue.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "cue.field.declaration") => Some(EntityKind::Field),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let bare = text
        .strip_prefix("_#")
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(text);
    if bare.is_empty() {
        return matches!(text, "#" | "_#").then_some(text);
    }
    let mut characters = bare.chars();
    let first = characters.next()?;
    ((first.is_alphabetic() || matches!(first, '_' | '$'))
        && characters.all(|character| {
            character.is_alphabetic()
                || character.is_ascii_digit()
                || matches!(character, '_' | '$')
        }))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "cue.name.definition" => captured_name(text, maximum),
        "cue.string_name.definition" => quoted_name(text, maximum),
        _ => None,
    }
}

pub(super) fn quoted_name(text: &str, maximum: usize) -> Option<&str> {
    (!text.is_empty()
        && text.len() <= maximum
        && text
            .chars()
            .all(|character| !character.is_control() && !matches!(character, '\\' | '"')))
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_labels_keep_definition_prefix_and_written_case() {
        for name in [
            "UserDefined",
            "userDefined",
            "_hidden",
            "#Shape",
            "_#Private",
            "λ",
            "$x",
            "if",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "3name",
            "a-b",
            "a.b",
            "label?",
            "label!",
            "alias=name",
            "(name)",
            "[string]",
            "\"name\"",
            "a\\(b)",
        ] {
            assert_eq!(captured_name(name, 128), None, "{name}");
        }
    }

    #[test]
    fn simple_quoted_labels_preserve_literal_punctuation_without_decoding() {
        assert_eq!(
            quoted_name("snapshot.debian.org", 128),
            Some("snapshot.debian.org")
        );
        assert_eq!(quoted_name("written label", 128), Some("written label"));
        for name in [
            "",
            "escaped\\n",
            "interpolated\\(x)",
            "\"quoted\"",
            "line\nend",
        ] {
            assert_eq!(quoted_name(name, 128), None);
        }
        assert_eq!(quoted_name("written", 6), None);
    }
}
