//! Reserve exact native Lean labels without inferring elaborated names or targets.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "lean.function.declaration" => Some(EntityKind::Function),
        "lean.parameter.declaration" => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let (first, tail) = text.as_bytes().split_first()?;
    (text != "_"
        && text.len() <= maximum
        && (first.is_ascii_alphabetic() || *first == b'_')
        && tail
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'\'')))
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
            "lean.name.definition" | "lean.formal_body.reference" | "lean.unproved_name.reference"
        )
    {
        return None;
    }
    captured_name(text, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_written_names_do_not_accept_qualified_quoted_or_compound_terms() {
        for name in ["owned", "argument2", "value'"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in [
            "",
            "_",
            "1value",
            "Owner.value",
            "«value»",
            "value x",
            "λ",
            "value()",
        ] {
            assert!(captured_name(name, 128).is_none());
        }
    }
}
