//! Nim captures preserve written leaf names independently of style equivalence.
//! Native owner checks exclude export markers, generated names and wrappers.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "nim.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "nim.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "nim.object.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "nim.enum.declaration") => Some(EntityKind::Enum),
        (SyntaxFactKind::Declaration, "nim.alias.declaration") => Some(EntityKind::TypeAlias),
        (SyntaxFactKind::Declaration, "nim.field.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "nim.parameter.declaration") => Some(EntityKind::Parameter),
        (SyntaxFactKind::Declaration, "nim.variable.declaration") => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut characters = text.chars();
    let first = characters.next()?;
    (text.len() <= maximum
        && (first.is_alphabetic() || first == '_')
        && text != "_"
        && !text.ends_with('_')
        && !text.contains("__")
        && characters.all(|character| character.is_alphanumeric() || character == '_'))
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
            "nim.name.definition" | "nim.call.reference" | "nim.unresolved_call.reference"
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
    fn written_leaf_names_preserve_case_and_exclude_export_or_quoted_wrappers() {
        for name in ["Node", "findNextText", "find_next_text", "café", "_name"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in [
            "", "_", "Node*", "`+`", "a__b", "end_", "1name", "a.b", "a b",
        ] {
            assert!(captured_name(name, 128).is_none(), "{name}");
        }
        assert_ne!(captured_name("Node", 4), captured_name("node", 4));
    }

    #[test]
    fn exact_nim_labels_are_required_for_entity_and_occurrence_admission() {
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
            entity_kind(&fact(SyntaxFactKind::Declaration, "nim.object.declaration")),
            Some(EntityKind::Struct)
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "nim.object.declaration.extra"
            ))
            .is_none()
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "nim.unresolved_call.reference"),
                "Node",
                4,
            ),
            Some("Node")
        );
        assert!(entity_kind(&fact(SyntaxFactKind::Signature, "nim.object.declaration")).is_none());
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "nim.call.reference"),
                "Node",
                4
            ),
            Some("Node")
        );
        assert!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "nim.call.reference.extra"),
                "Node",
                4
            )
            .is_none()
        );
    }
}
