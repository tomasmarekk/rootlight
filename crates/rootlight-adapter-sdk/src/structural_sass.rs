//! Sass identities require exact native labels in their own language namespace.
//! Written selectors and dollar-inclusive variables preserve source spelling;
//! lookup equivalence is established separately by language-owned bindings.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Declaration, "sass.variable.declaration") => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "sass.style_rule.declaration") => Some(EntityKind::StyleRule),
        (SyntaxFactKind::Declaration, "sass.property.declaration") => Some(EntityKind::Property),
        _ => None,
    }
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if text.is_empty() || text.len() > maximum || text.chars().any(char::is_control) {
        return None;
    }
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "sass.selector.definition")
        | (SyntaxFactKind::Signature, "sass.selector.signature") => {
            (!text.contains('\0')).then_some(text)
        }
        (
            SyntaxFactKind::Occurrence,
            "sass.name.definition"
            | "sass.plain_read.reference"
            | "sass.unresolved_read.reference"
            | "sass.assignment.write",
        )
        | (SyntaxFactKind::Signature, "sass.name.signature") => {
            let name = text.strip_prefix('$').unwrap_or(text);
            let mut characters = name.chars();
            (characters
                .next()
                .is_some_and(|value| value.is_alphabetic() || matches!(value, '_' | '-'))
                && characters.all(|value| value.is_alphanumeric() || matches!(value, '_' | '-')))
            .then_some(text)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn sass_namespace_keeps_exact_kinds_and_written_names() {
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([7; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        for (label, expected) in [
            ("sass.variable.declaration", Some(EntityKind::Variable)),
            ("sass.property.declaration", Some(EntityKind::Property)),
            ("sass.style_rule.declaration", Some(EntityKind::StyleRule)),
            ("sass.mixin.declaration", None),
            ("sass.unknown.declaration", None),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                expected
            );
        }
        for name in ["$nav-content-width", "$nav_content_width", "$Width", "$色"] {
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Occurrence, "sass.name.definition"),
                    name,
                    128
                ),
                Some(name)
            );
        }
        for name in ["$a.b", "$a\\62", "$", "a b", "\nname"] {
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Occurrence, "sass.name.definition"),
                    name,
                    128
                ),
                None
            );
        }
    }
}
