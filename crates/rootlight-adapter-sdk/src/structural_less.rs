//! Less identities belong only to exact native declaration and leaf labels.
//! Source spelling is retained; escaped and interpolated names remain unavailable.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Declaration, "less.mixin.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "less.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    if !matches!(
        (fact.kind(), fact.syntax_kind().as_str()),
        (SyntaxFactKind::Signature, "less.name.signature")
            | (
                SyntaxFactKind::Occurrence,
                "less.name.definition"
                    | "less.direct_declared_mixin.call"
                    | "less.unresolved_mixin.call"
            )
    ) {
        return None;
    }
    let name = text
        .strip_prefix('.')
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(text);
    let mut chars = name.chars();
    (chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '_' | '-'))
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn exact_less_labels_preserve_authored_mixin_and_parameter_names() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for (label, expected) in [
            ("less.mixin.declaration", Some(EntityKind::Function)),
            ("less.parameter.declaration", Some(EntityKind::Parameter)),
            ("less.unknown.declaration", None),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                expected
            );
        }
        let leaf = fact(SyntaxFactKind::Occurrence, "less.name.definition");
        for text in [".size", "#measure", "width", "height"] {
            assert_eq!(captured_name_for_fact(&leaf, text, 128), Some(text));
            assert_eq!(captured_name_for_fact(&leaf, text, text.len() - 1), None);
        }
        for text in ["@width", ".a\\62", "@{name}", ".a b", ".色", ""] {
            assert_eq!(captured_name_for_fact(&leaf, text, 128), None);
        }
        assert_eq!(
            captured_name_for_fact(
                &fact(
                    SyntaxFactKind::Signature,
                    "less.closed_mixin_namespace.signature"
                ),
                ".size",
                128
            ),
            None
        );
    }
}
