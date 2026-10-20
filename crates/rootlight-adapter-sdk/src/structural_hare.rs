//! Exact Hare labels preserve written types, fields, functions and formals.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "hare.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "hare.struct.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "hare.field.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "hare.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "hare.parameter.declaration") => Some(EntityKind::Parameter),
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
            "hare.name.definition"
                | "hare.direct_file_function.reference"
                | "hare.unresolved_call.reference"
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
    fn written_names_preserve_case_and_reject_qualified_or_unproved_forms() {
        for name in [
            "equation",
            "test",
            "read_input",
            "main",
            "_value",
            "CamelCase",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "fmt::print", "obj.field", "a[0]", "1name", "雪"] {
            assert_eq!(captured_name(name, 128), None);
        }
    }

    #[test]
    fn only_closed_declaration_labels_admit_entity_kinds() {
        for (label, expected) in [
            ("hare.struct.declaration", Some(EntityKind::Struct)),
            ("hare.field.declaration", Some(EntityKind::Field)),
            ("hare.function.declaration", Some(EntityKind::Function)),
            ("hare.parameter.declaration", Some(EntityKind::Parameter)),
            ("hare.unsupported.declaration", None),
            ("hare.function_like.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([7; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected);
        }
    }
}
