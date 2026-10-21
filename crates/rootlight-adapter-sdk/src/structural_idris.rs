//! Closed written Idris names never infer a type signature or elaborated binder.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "idris.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "idris.function.declaration") => Some(EntityKind::Function),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let (first, rest) = text.as_bytes().split_first()?;
    ((first.is_ascii_alphabetic() || *first == b'_')
        && rest
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\'')))
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
            "idris.function_name.definition"
                | "idris.own_body_global.reference"
                | "idris.unproved_name.reference"
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

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([9; 20]), 0, 3).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn idris_names_require_closed_role_namespace_and_bounded_exact_source() {
        for label in [
            "idris.function_name.definition",
            "idris.own_body_global.reference",
            "idris.unproved_name.reference",
        ] {
            let site = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                super::super::structural_captured_name_for_fact("idris", &site, "var", 3)
                    .as_deref(),
                Some("var")
            );
            assert!(
                super::super::structural_captured_name_for_fact("haskell", &site, "var", 3)
                    .is_none()
            );
            for raw in [" var", "var ", "var\0", "1var", "a.b", "λ"] {
                assert!(captured_name_for_fact(&site, raw, 32).is_none(), "{raw:?}");
            }
            assert!(captured_name_for_fact(&site, "var", 2).is_none());
        }
        assert!(
            captured_name_for_fact(
                &fact(
                    SyntaxFactKind::Declaration,
                    "idris.function_name.definition"
                ),
                "var",
                3
            )
            .is_none()
        );
        assert!(
            captured_name_for_fact(
                &fact(
                    SyntaxFactKind::Occurrence,
                    "idris.function_name.definition.extra"
                ),
                "var",
                3
            )
            .is_none()
        );
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "idris.function.declaration"
            )),
            Some(EntityKind::Function)
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Occurrence,
                "idris.function.declaration"
            ))
            .is_none()
        );
    }
}
