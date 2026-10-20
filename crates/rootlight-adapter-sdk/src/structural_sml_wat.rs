//! Exact language-owned labels and written identifier spelling only.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    if fact.kind() != SyntaxFactKind::Declaration {
        return None;
    }
    match fact.syntax_kind().as_str() {
        "sml.function.declaration" | "wat.function.declaration" => Some(EntityKind::Function),
        "wat.parameter.declaration" => Some(EntityKind::Parameter),
        "wat.module.declaration" => Some(EntityKind::Module),
        _ => None,
    }
}
pub(super) fn captured_name<'a>(language: &str, text: &'a str, maximum: usize) -> Option<&'a str> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    let bytes = text.as_bytes();
    if language == "webassembly" {
        let (first, rest) = bytes.split_first()?;
        return (*first == b'$'
            && !rest.is_empty()
            && rest
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-./:<=>?@\\^_`|~".contains(b)))
        .then_some(text);
    }
    if language != "standard-ml" {
        return None;
    }
    let (first, rest) = bytes.split_first()?;
    if !first.is_ascii_lowercase()
        || !rest
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'\''))
    {
        return None;
    }
    if matches!(
        text,
        "and"
            | "andalso"
            | "as"
            | "case"
            | "datatype"
            | "do"
            | "else"
            | "end"
            | "exception"
            | "fn"
            | "fun"
            | "handle"
            | "if"
            | "in"
            | "infix"
            | "infixr"
            | "let"
            | "local"
            | "nonfix"
            | "of"
            | "op"
            | "open"
            | "orelse"
            | "raise"
            | "rec"
            | "then"
            | "type"
            | "val"
            | "with"
            | "withtype"
            | "while"
            | "nil"
            | "true"
            | "false"
            | "ref"
            | "o"
            | "before"
            | "div"
            | "mod"
    ) {
        return None;
    }
    Some(text)
}
pub(super) fn captured_name_for_fact<'a>(
    language: &str,
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<&'a str> {
    let prefix = if language == "standard-ml" {
        "sml."
    } else if language == "webassembly" {
        "wat."
    } else {
        return None;
    };
    if !fact.syntax_kind().as_str().starts_with(prefix) {
        return None;
    }
    if fact.kind() == SyntaxFactKind::Declaration {
        entity_kind(fact)?;
        return captured_name(language, text, maximum);
    }
    if fact.kind() != SyntaxFactKind::Occurrence
        || !matches!(
            fact.syntax_kind().as_str(),
            "sml.name.definition"
                | "sml.unresolved_name.reference"
                | "wat.name.definition"
                | "wat.unresolved_name.reference"
        )
    {
        return None;
    }
    captured_name(language, text, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn written_names_preserve_dollars_and_reject_selectors_and_reserved_binders() {
        assert_eq!(
            captured_name("webassembly", "$written-name", 64),
            Some("$written-name")
        );
        assert_eq!(captured_name("webassembly", "local.get", 64), None);
        assert_eq!(
            captured_name("standard-ml", "written'", 64),
            Some("written'")
        );
        for name in [
            "Module.value",
            "SOME",
            "true",
            "ref",
            "op",
            "o",
            "before",
            "div",
        ] {
            assert_eq!(captured_name("standard-ml", name, 64), None);
        }
        assert_eq!(captured_name("standard-ml", "written", 6), None);
    }
    #[test]
    fn exact_labels_keep_named_module_function_and_parameter_kinds_distinct() {
        for (label, expected) in [
            ("wat.module.declaration", Some(EntityKind::Module)),
            ("wat.function.declaration", Some(EntityKind::Function)),
            ("wat.parameter.declaration", Some(EntityKind::Parameter)),
            ("sml.function.declaration", Some(EntityKind::Function)),
            ("sml.parameter.declaration", None),
            ("wat.opcode.declaration", None),
            ("sml.unproved.declaration", None),
            ("python.function.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 1)
                    .unwrap(),
                0,
                crate::SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected);
        }
    }
    #[test]
    fn public_projection_requires_matching_language_role_label_and_byte_budget() {
        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            rootlight_ir::SourceSpan::new(rootlight_ids::FileId::from_bytes([7; 20]), 0, 6)
                .unwrap(),
            0,
            crate::SyntaxKindLabel::new("wat.name.definition").unwrap(),
        );
        assert_eq!(
            crate::structural_captured_name_for_fact("webassembly", &fact, "$value", 6).as_deref(),
            Some("$value")
        );
        assert!(
            crate::structural_captured_name_for_fact("standard-ml", &fact, "$value", 6).is_none()
        );
        assert!(
            crate::structural_captured_name_for_fact("webassembly", &fact, "$value", 5).is_none()
        );
    }
}
