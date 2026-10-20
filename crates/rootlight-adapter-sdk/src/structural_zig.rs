//! Bare Zig binders preserve written case without evaluating their initializers.
//! Quoted identifiers and inferred container types require separate name proof.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "zig.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "zig.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "zig.variable.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "zig.constant.declaration") => Some(EntityKind::Constant),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && text != "_"
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
        && fact.syntax_kind().as_str() == "zig.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_written_binders_retain_case_and_exclude_discard_and_quoted_names() {
        for name in ["pipe", "read", "write", "Mixed", "_private", "field2"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in ["", "_", "1name", "a.b", "@\"name\"", "a b", "λ"] {
            assert!(captured_name(name, 64).is_none(), "{name}");
        }
        assert_ne!(captured_name("Mixed", 5), captured_name("mixed", 5));
    }

    #[test]
    fn entity_and_definition_admission_requires_the_exact_zig_contract() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 4).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "zig.function.declaration"
            )),
            Some(EntityKind::Function)
        );
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "zig.variable.declaration"
            )),
            Some(EntityKind::Variable)
        );
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "zig.constant.declaration"
            )),
            Some(EntityKind::Constant)
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "other.function.declaration"
            ))
            .is_none()
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Occurrence,
                "zig.function.declaration"
            ))
            .is_none()
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "zig.name.definition"),
                "pipe",
                4
            ),
            Some("pipe")
        );
        assert!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "zig.call.reference"),
                "pipe",
                4
            )
            .is_none()
        );
    }
}
