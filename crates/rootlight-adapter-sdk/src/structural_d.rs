//! D names are source-written headers; template and compiler binding is unavailable.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;
pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "d.file.module")
        | (SyntaxFactKind::Declaration, "d.module.declaration") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "d.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "d.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}
pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut b = text.bytes();
    let first = b.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_'))
    .then_some(text)
}
pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if fact.kind() != SyntaxFactKind::Occurrence || text.len() > maximum {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "d.name.definition" => captured_name(text, maximum),
        "d.module.name.definition" => text
            .split('.')
            .all(|p| captured_name(p, p.len()).is_some())
            .then_some(text),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn written_names_reject_expressions_and_keep_exact_case() {
        for name in ["index", "showHome", "req", "res", "_value2"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "1bad", "a.b", "a!T", "a()", "a b", "λ"] {
            assert!(captured_name(name, 100).is_none());
        }
    }
}
