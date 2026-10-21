//! Admit only source-owned CoffeeScript declaration and occurrence labels.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "coffeescript.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "coffeescript.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "coffeescript.method.declaration") => {
            Some(EntityKind::Method)
        }
        (SyntaxFactKind::Declaration, "coffeescript.variable.declaration") => {
            Some(EntityKind::Variable)
        }
        _ => None,
    }
}
pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')))
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
            "coffeescript.name.definition" | "coffeescript.unresolved_name.reference"
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
    fn closed_written_labels_do_not_promote_foreign_roles_or_selector_names() {
        for (label, kind) in [
            ("coffeescript.class.declaration", Some(EntityKind::Class)),
            ("coffeescript.method.declaration", Some(EntityKind::Method)),
            (
                "coffeescript.variable.declaration",
                Some(EntityKind::Variable),
            ),
            ("coffeescript.unsupported.declaration", None),
            ("coffeescript.constructor.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([5; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), kind);
        }
        for name in ["Written", "$value", "_owned"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["a.b", "@field", "a[0]", "'Written'", "\\u0061", "λ", ""] {
            assert_eq!(captured_name(name, 128), None);
        }
    }
}
