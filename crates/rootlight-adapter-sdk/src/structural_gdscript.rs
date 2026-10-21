//! Classify only the written GDScript labels emitted by the native owner census.
//! Missing class_name ownership never promotes an observable function to a method.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "gdscript.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "gdscript.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "gdscript.constructor.declaration") => {
            Some(EntityKind::Constructor)
        }
        (SyntaxFactKind::Declaration, "gdscript.method.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "gdscript.unowned_function.declaration") => {
            Some(EntityKind::Function)
        }
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
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
            "gdscript.name.definition" | "gdscript.unresolved_name.reference"
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
    fn closed_labels_preserve_constructor_method_and_unowned_function_distinctions() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "gdscript.class.declaration",
                Some(EntityKind::Class),
            ),
            (
                SyntaxFactKind::Declaration,
                "gdscript.constructor.declaration",
                Some(EntityKind::Constructor),
            ),
            (
                SyntaxFactKind::Declaration,
                "gdscript.method.declaration",
                Some(EntityKind::Method),
            ),
            (
                SyntaxFactKind::Declaration,
                "gdscript.unowned_function.declaration",
                Some(EntityKind::Function),
            ),
            (
                SyntaxFactKind::Occurrence,
                "gdscript.method.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "gdscript.unsupported.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "gdscript.parameter.declaration",
                None,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([5; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected, "{label}");
        }
    }

    #[test]
    fn written_names_do_not_alias_paths_selectors_literals_or_case() {
        for name in ["GroupCel", "_init", "get_image", "member2"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "a.b",
            "a[0]",
            "1a",
            "a b",
            "\"GroupCel\"",
            "\\u0061",
            "λ",
        ] {
            assert_eq!(captured_name(name, 128), None);
        }
        assert_ne!(captured_name("GroupCel", 8), captured_name("groupcel", 8));
    }
}
