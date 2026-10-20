//! Only the Flux bridge's exact native labels establish written identities.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Declaration, "flux.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "flux.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    if text.is_empty()
        || text.len() > maximum
        || text.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    // Lexical acceptance belongs to the native scanner. This gate admits only
    // exact leaves already certified by that bridge, including Unicode names.
    matches!(
        (fact.kind(), fact.syntax_kind().as_str()),
        (
            SyntaxFactKind::Occurrence,
            "flux.name.definition"
                | "flux.parameter_read.reference"
                | "flux.unresolved_read.reference"
        )
    )
    .then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn exact_flux_namespace_does_not_promote_unknown_labels_or_markers() {
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
            ("flux.function.declaration", Some(EntityKind::Function)),
            ("flux.parameter.declaration", Some(EntityKind::Parameter)),
            ("flux.guessed_function.declaration", None),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                expected
            );
        }
        for name in ["value", "_value", "é", "变量"] {
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Occurrence, "flux.name.definition"),
                    name,
                    64
                ),
                Some(name)
            );
        }
        for (kind, label, text) in [
            (
                SyntaxFactKind::Signature,
                "flux.parameter_target.signature",
                "value",
            ),
            (
                SyntaxFactKind::Occurrence,
                "flux.name.definition",
                "two names",
            ),
            (SyntaxFactKind::Occurrence, "flux.other.reference", "value"),
        ] {
            assert_eq!(captured_name_for_fact(&fact(kind, label), text, 64), None);
        }
    }
}
