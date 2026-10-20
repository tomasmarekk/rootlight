//! Written Fish names preserve case and physical spelling without expansion.
//! Named parameters use variable identifiers; computed names require other proof.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "fish.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "fish.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "fish.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum || text.starts_with('-') || text.contains('/') {
        return None;
    }
    text.chars()
        .all(|character| {
            !character.is_whitespace()
                && !character.is_control()
                && !matches!(
                    character,
                    '$' | '*'
                        | '~'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '['
                        | ']'
                        | '<'
                        | '>'
                        | '"'
                        | '\''
                        | '&'
                        | '|'
                        | ';'
                        | '\\'
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
        && fact.syntax_kind().as_str() == "fish.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_names_are_case_sensitive_and_do_not_expand() {
        for name in ["fisher", "Mixed", "mixed", "λ", "name-with-dashes"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "-name", "a/b", "$name", "'name'", "name\\x", "a b"] {
            assert_eq!(captured_name(name, 128), None, "{name}");
        }
    }
}
