//! Sed entities are written statement labels from a declared static file view.
//! Native callers prove syntax; this closed classifier never trims a label.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "sed.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "sed.statement_label.declaration") => {
            Some(EntityKind::StatementLabel)
        }
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
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
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
            "sed.label_name.definition"
                | "sed.written_branch.reference"
                | "sed.unproved_branch.reference"
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
            SourceSpan::new(FileId::from_bytes([5; 20]), 0, 4).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn sed_written_names_require_exact_role_namespace_and_untrimmed_bounded_spelling() {
        for label in [
            "sed.label_name.definition",
            "sed.written_branch.reference",
            "sed.unproved_branch.reference",
        ] {
            let name = fact(SyntaxFactKind::Occurrence, label);
            assert_eq!(
                super::super::structural_captured_name_for_fact("sed", &name, "none", 4).as_deref(),
                Some("none")
            );
            assert!(
                super::super::structural_captured_name_for_fact("bash", &name, "none", 4).is_none()
            );
            assert!(captured_name_for_fact(&name, "none", 3).is_none());
            for raw in [" none", "none ", "none\r", "none\0", "n\\one", "1none", "λ"] {
                assert!(captured_name_for_fact(&name, raw, 64).is_none(), "{raw:?}");
            }
        }
        assert!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Declaration, "sed.label_name.definition"),
                "none",
                4
            )
            .is_none()
        );
        assert!(
            captured_name_for_fact(
                &fact(
                    SyntaxFactKind::Occurrence,
                    "sed.label_name.definition.extra"
                ),
                "none",
                4
            )
            .is_none()
        );
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "sed.statement_label.declaration"
            )),
            Some(EntityKind::StatementLabel)
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Occurrence,
                "sed.statement_label.declaration"
            ))
            .is_none()
        );
    }
}
