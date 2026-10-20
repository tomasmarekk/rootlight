//! Written HCL attribute and object keys provide properties without evaluation.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "hcl.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "hcl.property.declaration") => Some(EntityKind::Property),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "hcl.name.definition")
        .then(|| captured_name(text, maximum))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_keys_keep_case_and_hyphens_without_decoding_expressions() {
        for name in [
            "hcloud",
            "source",
            "version",
            "first-name",
            "_private",
            "Mixed2",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "1name", "var.key", "(name)", "\"name\"", "a b", "λ"] {
            assert_eq!(captured_name(name, 128), None, "{name}");
        }
    }

    #[test]
    fn only_the_exact_property_and_definition_contract_is_admitted() {
        use crate::SyntaxKindLabel;
        use rootlight_ids::FileId;
        use rootlight_ir::SourceSpan;
        let fact = |kind, label| {
            SyntaxFact::new(
                1,
                None,
                kind,
                SourceSpan::new(FileId::from_bytes([1; 20]), 0, 6).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        assert_eq!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "hcl.property.declaration"
            )),
            Some(EntityKind::Property)
        );
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Module, "hcl.file.module")),
            Some(EntityKind::Module)
        );
        assert!(
            entity_kind(&fact(
                SyntaxFactKind::Declaration,
                "other.property.declaration"
            ))
            .is_none()
        );
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "hcl.name.definition"),
                "hcloud",
                6
            ),
            Some("hcloud")
        );
        assert!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "hcl.name.reference"),
                "hcloud",
                6
            )
            .is_none()
        );
    }
}
