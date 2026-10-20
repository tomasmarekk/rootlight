//! Mojo identities require exact native labels; unproved names stay unavailable.
use crate::{SyntaxFact, SyntaxFactKind};
use rootlight_ir::EntityKind;

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "mojo.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "mojo.struct.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "mojo.class.declaration") => Some(EntityKind::Class),
        (SyntaxFactKind::Declaration, "mojo.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "mojo.method.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "mojo.field.declaration") => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "mojo.parameter.declaration") => Some(EntityKind::Parameter),
        _ => None,
    }
}

#[cfg(test)]
mod self_member_protocol_tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    #[test]
    fn owned_self_member_names_require_exact_language_role_and_protocol_labels() {
        let span = SourceSpan::new(FileId::from_bytes([7; 20]), 0, 4).unwrap();
        for label in [
            "mojo.self_field_read.reference",
            "mojo.self_method_call.call",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(
                crate::structural_captured_name_for_fact("mojo", &fact, "data", 4).as_deref(),
                Some("data")
            );
            assert!(crate::structural_captured_name_for_fact("python", &fact, "data", 4).is_none());
            assert!(crate::structural_captured_name_for_fact("mojo", &fact, "data", 3).is_none());
            let wrong = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Signature,
                span,
                0,
                fact.syntax_kind().clone(),
            );
            assert!(crate::structural_captured_name_for_fact("mojo", &wrong, "data", 4).is_none());
        }
        for label in [
            "mojo.self_field_read.call",
            "mojo.self_method_call.reference",
            "mojo.self_field_target.signature",
            "mojo.self_formal_target.signature",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Occurrence,
                span,
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert!(crate::structural_captured_name_for_fact("mojo", &fact, "data", 4).is_none());
        }
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    let first = bytes.next()?;
    (text.len() <= maximum
        && (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !matches!(
            text,
            "class"
                | "struct"
                | "def"
                | "fn"
                | "var"
                | "ref"
                | "comptime"
                | "if"
                | "else"
                | "return"
                | "for"
                | "while"
                | "import"
                | "from"
                | "pass"
        ))
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
            "mojo.name.definition"
                | "mojo.unresolved_name.reference"
                | "mojo.self_field_read.reference"
                | "mojo.self_method_call.call"
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
    fn exact_native_labels_keep_struct_class_method_field_and_formal_kinds_distinct() {
        for (label, expected) in [
            ("mojo.struct.declaration", Some(EntityKind::Struct)),
            ("mojo.class.declaration", Some(EntityKind::Class)),
            ("mojo.method.declaration", Some(EntityKind::Method)),
            ("mojo.function.declaration", Some(EntityKind::Function)),
            ("mojo.field.declaration", Some(EntityKind::Field)),
            ("mojo.parameter.declaration", Some(EntityKind::Parameter)),
            ("mojo.unproved_method.declaration", None),
            ("mojo.unsupported.declaration", None),
            ("python.class.declaration", None),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([7; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), expected, "{label}");
        }
    }

    #[test]
    fn written_ascii_names_keep_case_and_refuse_unproved_or_expanded_text() {
        for name in ["Written", "value", "_value", "self", "__init__"] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in [
            "",
            "struct",
            "fn",
            "var",
            "holder.value",
            "value()",
            "value name",
            "λ",
        ] {
            assert_eq!(captured_name(name, 128), None);
        }
        assert_ne!(captured_name("Value", 5), captured_name("value", 5));
    }

    #[test]
    fn public_name_projection_requires_the_matching_language_role_and_exact_label() {
        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 7).unwrap(),
            0,
            SyntaxKindLabel::new("mojo.name.definition").unwrap(),
        );
        assert_eq!(
            crate::structural_captured_name_for_fact("mojo", &fact, "Written", 7).as_deref(),
            Some("Written")
        );
        assert!(crate::structural_captured_name_for_fact("mojo", &fact, "Written", 6).is_none());
        assert!(crate::structural_captured_name_for_fact("python", &fact, "Written", 7).is_none());
        let wrong = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Occurrence,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 7).unwrap(),
            0,
            SyntaxKindLabel::new("mojo.unproved_method.definition").unwrap(),
        );
        assert!(crate::structural_captured_name_for_fact("mojo", &wrong, "Written", 7).is_none());
    }
}
