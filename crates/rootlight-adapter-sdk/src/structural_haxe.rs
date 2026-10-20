//! Haxe declaration kinds and names require language-owned native labels.
//! Dynamic names and generated macro declarations have no written identity here.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "haxe.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "haxe.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "haxe.method.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "haxe.variable.declaration") => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !matches!(
            text,
            "new"
                | "class"
                | "function"
                | "var"
                | "final"
                | "macro"
                | "return"
                | "if"
                | "else"
                | "for"
                | "while"
                | "try"
                | "catch"
                | "switch"
                | "case"
                | "default"
                | "import"
                | "package"
                | "using"
                | "true"
                | "false"
                | "null"
                | "this"
                | "super"
        ))
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
            "haxe.name.definition" | "haxe.local_read.reference" | "haxe.unresolved_read.reference"
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
    fn only_exact_native_haxe_labels_classify_existing_kinds() {
        for (label, expected) in [
            ("haxe.class.declaration", Some(EntityKind::Class)),
            ("haxe.method.declaration", Some(EntityKind::Method)),
            ("haxe.variable.declaration", Some(EntityKind::Variable)),
            ("haxe.unsupported.declaration", None),
            ("haxe.name.definition", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected);
        }
    }

    #[test]
    fn authored_ascii_names_keep_case_and_reject_dynamic_or_unproved_names() {
        for name in ["Written", "value", "_value", "value2"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "macro",
            "new",
            "holder.value",
            "$value",
            "λ",
            "value()",
            " value",
        ] {
            assert!(captured_name(name, 128).is_none());
        }
        assert_ne!(captured_name("Value", 5), captured_name("value", 5));
    }
}
