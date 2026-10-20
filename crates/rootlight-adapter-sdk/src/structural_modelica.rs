//! Only exact native Modelica function/input labels establish entity kinds.
//! Bare identifiers preserve case; quoted names require separate canonical proof.

use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "modelica.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "modelica.function.declaration") => {
            Some(EntityKind::Function)
        }
        (SyntaxFactKind::Declaration, "modelica.input_parameter.declaration") => {
            Some(EntityKind::Parameter)
        }
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
            "modelica.name.definition"
                | "modelica.input_read.reference"
                | "modelica.unresolved_read.reference"
                | "modelica.unresolved_reference.reference"
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
    fn exact_bare_names_preserve_case_without_quoted_or_selected_aliases() {
        for name in ["U_sf", "Unom", "Mdot", "_value", "x1"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert!(captured_name(name, name.len() - 1).is_none());
        }
        for name in ["", "'Unom'", "a.b", "a[1]", "1a", "a b", "λ"] {
            assert!(captured_name(name, 64).is_none(), "{name}");
        }
        assert_ne!(captured_name("Unom", 4), captured_name("unom", 4));
    }

    #[test]
    fn only_the_exact_modelica_declaration_label_admits_an_input() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "modelica.input_parameter.declaration",
                Some(EntityKind::Parameter),
            ),
            (
                SyntaxFactKind::Declaration,
                "modelica.function.declaration",
                Some(EntityKind::Function),
            ),
            (
                SyntaxFactKind::Occurrence,
                "modelica.input_parameter.declaration",
                None,
            ),
            (
                SyntaxFactKind::Declaration,
                "modelica.output_parameter.declaration",
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
