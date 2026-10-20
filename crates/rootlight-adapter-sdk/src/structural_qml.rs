//! QML kinds come from native object membership, not JavaScript spelling.
//! This initial lane admits plain written names and preserves all other gaps.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "qml.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "qml.id_variable.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "qml.property.declaration") => Some(EntityKind::Property),
        (SyntaxFactKind::Declaration, "qml.method.declaration") => Some(EntityKind::Method),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_' || first == b'$')
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
            "qml.name.definition" | "qml.unique_id_method.call"
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
    fn exact_native_qml_labels_keep_the_authored_id_property_and_method_kinds() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "qml.id_variable.declaration",
                Some(EntityKind::Variable),
            ),
            (
                SyntaxFactKind::Declaration,
                "qml.property.declaration",
                Some(EntityKind::Property),
            ),
            (
                SyntaxFactKind::Declaration,
                "qml.method.declaration",
                Some(EntityKind::Method),
            ),
            (
                SyntaxFactKind::Declaration,
                "qml.unsupported.declaration",
                None,
            ),
            (SyntaxFactKind::Occurrence, "qml.method.declaration", None),
            (
                SyntaxFactKind::Declaration,
                "qml.function.declaration",
                None,
            ),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected, "{label}");
        }
    }

    #[test]
    fn plain_names_preserve_case_and_reject_unproved_escapes_and_qualified_spelling() {
        for name in [
            "root",
            "selectedBarId",
            "normalizeSelectedBar",
            "_value",
            "$value",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "root.method",
            "root['method']",
            "\\u0072oot",
            "λ",
            "Root {}",
        ] {
            assert_eq!(captured_name(name, 128), None);
        }
        assert_ne!(captured_name("root", 4), captured_name("Root", 4));
    }
}
