//! Closed Ring labels distinguish functions, class methods and ordinary formals.
//! Names preserve source case; the native certificate alone admits ASCII case folding.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "ring.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "ring.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "ring.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "ring.method.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "ring.parameter.declaration") => Some(EntityKind::Parameter),
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
            "ring.name.definition"
                | "ring.parameter_read.reference"
                | "ring.unresolved_read.reference"
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
    fn exact_names_keep_written_case_and_reject_selected_or_non_ascii_aliases() {
        for name in ["HTMLFunctions", "HTMLSpecialChars", "cStr", "_value", "x1"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "a.b", "a[1]", "1a", "a b", "雪", "\\u0061"] {
            assert_eq!(captured_name(name, 64), None);
        }
        assert_ne!(captured_name("cStr", 4), captured_name("CSTR", 4));
    }

    #[test]
    fn only_exact_ring_declaration_kinds_admit_ordinary_parameters() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "ring.parameter.declaration",
                Some(EntityKind::Parameter),
            ),
            (
                SyntaxFactKind::Declaration,
                "ring.method.declaration",
                Some(EntityKind::Method),
            ),
            (
                SyntaxFactKind::Declaration,
                "ring.function.declaration",
                Some(EntityKind::Function),
            ),
            (
                SyntaxFactKind::Occurrence,
                "ring.function.declaration",
                None,
            ),
            (
                SyntaxFactKind::Occurrence,
                "ring.parameter.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "ring.typed_parameter.declaration",
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
            assert_eq!(entity_kind(&fact), expected);
        }
    }
}
