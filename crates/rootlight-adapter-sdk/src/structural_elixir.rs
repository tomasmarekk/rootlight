//! Written Elixir declaration identities without expansion or runtime binding.
//! Names remain borrowed and retain exact source spelling, including `?` and `!`.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "elixir.file.module")
        | (SyntaxFactKind::Declaration, "elixir.module.declaration") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "elixir.protocol.declaration") => Some(EntityKind::Protocol),
        (SyntaxFactKind::Declaration, "elixir.function.declaration") => Some(EntityKind::Function),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let base = text.strip_suffix(['?', '!']).unwrap_or(text);
    let mut components = base.split('.');
    components
        .all(|component| {
            let mut characters = component.chars();
            characters
                .next()
                .is_some_and(|first| first == '_' || first.is_alphabetic())
                && characters.all(|character| character == '_' || character.is_alphanumeric())
        })
        .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn names_preserve_unicode_case_and_terminal_predicate_or_bang_spelling() {
        for name in [
            "Phoenix.Digester.Gzip",
            "ready?",
            "encode!",
            "_value",
            "café",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "", "?", "Foo..Bar", ".Foo", "Foo.", "bad!name", "x()", "x/y", "def x", "9value",
        ] {
            assert_eq!(captured_name(name, 256), None, "{name}");
        }
    }

    #[test]
    fn only_reviewed_complete_fact_labels_classify_elixir_owners() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Module,
                "elixir.file.module",
                Some(EntityKind::Module),
            ),
            (
                SyntaxFactKind::Declaration,
                "elixir.module.declaration",
                Some(EntityKind::Module),
            ),
            (
                SyntaxFactKind::Declaration,
                "elixir.protocol.declaration",
                Some(EntityKind::Protocol),
            ),
            (
                SyntaxFactKind::Declaration,
                "elixir.function.declaration",
                Some(EntityKind::Function),
            ),
            (
                SyntaxFactKind::Declaration,
                "elixir.function.declaration.extra",
                None,
            ),
            (
                SyntaxFactKind::Signature,
                "elixir.function.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "elixir.macro.declaration",
                None,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected);
        }
    }
}
