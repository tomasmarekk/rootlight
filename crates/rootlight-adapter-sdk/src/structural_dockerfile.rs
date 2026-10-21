//! Closed Docker labels retain literal stage aliases and ENV keys.
//! Image names, shell text, ENV values and uncertified COPY flags supply no definitions.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Declaration, "dockerfile.stage.declaration") => {
            Some(EntityKind::BuildTarget)
        }
        (SyntaxFactKind::Declaration, "dockerfile.env.declaration") => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && first.is_ascii_lowercase()
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        }))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "dockerfile.env_name.definition"
    {
        if text.is_empty() || text.len() > maximum {
            return None;
        }
        let mut bytes = text.bytes();
        let first = bytes.next()?;
        return ((first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
        .then_some(text);
    }
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "dockerfile.stage_alias.definition" | "dockerfile.copy_from.reference"
        ))
    .then(|| captured_name(text, maximum))
    .flatten()
}
