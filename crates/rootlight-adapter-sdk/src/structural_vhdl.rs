//! VHDL declaration identity distinguishes basic and extended identifiers.
//! Physical spelling stays in Definition occurrences and display names.

use std::borrow::Cow;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "vhdl.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "vhdl.entity.declaration") => Some(EntityKind::Module),
        (
            SyntaxFactKind::Declaration,
            "vhdl.architecture.declaration" | "vhdl.package.declaration",
        ) => Some(EntityKind::Namespace),
        (SyntaxFactKind::Declaration, "vhdl.component.declaration") => Some(EntityKind::Interface),
        (
            SyntaxFactKind::Declaration,
            "vhdl.port.declaration" | "vhdl.record_field.declaration",
        ) => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "vhdl.generic.declaration" | "vhdl.constant.declaration") => {
            Some(EntityKind::Constant)
        }
        (SyntaxFactKind::Declaration, "vhdl.argument.declaration") => Some(EntityKind::Parameter),
        (
            SyntaxFactKind::Declaration,
            "vhdl.function.declaration" | "vhdl.procedure.declaration",
        ) => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "vhdl.type.declaration" | "vhdl.subtype.declaration") => {
            Some(EntityKind::TypeAlias)
        }
        (
            SyntaxFactKind::Declaration,
            "vhdl.variable.declaration"
            | "vhdl.signal.declaration"
            | "vhdl.file_object.declaration",
        ) => Some(EntityKind::Variable),
        _ => None,
    }
}

pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    if text.starts_with('\\') {
        let body = text.strip_prefix('\\')?.strip_suffix('\\')?;
        if body.is_empty() || body.chars().any(char::is_control) {
            return None;
        }
        let mut chars = body.chars();
        while let Some(character) = chars.next() {
            if character == '\\' && chars.next() != Some('\\') {
                return None;
            }
        }
        // Delimiters keep extended names distinct from folded basic names.
        return Some(Cow::Borrowed(text));
    }
    let bytes = text.as_bytes();
    if !bytes[0].is_ascii_alphabetic()
        || bytes.last() == Some(&b'_')
        || bytes.windows(2).any(|pair| pair == b"__")
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return None;
    }
    if !bytes.iter().any(u8::is_ascii_uppercase) {
        return Some(Cow::Borrowed(text));
    }
    let mut canonical = String::new();
    canonical.try_reserve_exact(text.len()).ok()?;
    canonical.extend(
        bytes
            .iter()
            .map(|byte| char::from(byte.to_ascii_lowercase())),
    );
    Some(Cow::Owned(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SyntaxKindLabel, structural_captured_name_for_fact, structural_entity_kind};
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn declaration_and_name_namespaces_require_exact_roles_and_language() {
        for (kind, label, expected) in [
            (
                SyntaxFactKind::Declaration,
                "vhdl.entity.declaration",
                Some(EntityKind::Module),
            ),
            (
                SyntaxFactKind::Declaration,
                "vhdl.port.declaration",
                Some(EntityKind::Field),
            ),
            (SyntaxFactKind::Occurrence, "vhdl.port.declaration", None),
            (SyntaxFactKind::Declaration, "vhdl.port.reference", None),
            (
                SyntaxFactKind::Declaration,
                "vhdl.port.declaration.extra",
                None,
            ),
        ] {
            assert_eq!(
                structural_entity_kind(&fact(kind, label)),
                expected,
                "{label}"
            );
        }
        let definition = fact(SyntaxFactKind::Occurrence, "vhdl.name.definition");
        assert_eq!(
            structural_captured_name_for_fact("vhdl", &definition, "Clock", 5).as_deref(),
            Some("clock")
        );
        assert!(structural_captured_name_for_fact("verilog", &definition, "Clock", 5).is_none());
        assert!(
            structural_captured_name_for_fact(
                "vhdl",
                &fact(SyntaxFactKind::Declaration, "vhdl.name.definition"),
                "Clock",
                5
            )
            .is_none()
        );
        assert!(
            structural_captured_name_for_fact(
                "vhdl",
                &fact(SyntaxFactKind::Occurrence, "vhdl.name.reference"),
                "Clock",
                5
            )
            .is_none()
        );
    }

    #[test]
    fn basic_names_fold_without_conflating_extended_spelling() {
        for (written, expected) in [
            ("Clock_A", "clock_a"),
            ("clock_a", "clock_a"),
            (r"\Clock_A\", r"\Clock_A\"),
            (r"\clock_a\", r"\clock_a\"),
            (r"\a\\b\", r"\a\\b\"),
            (r"\space name\", r"\space name\"),
        ] {
            assert_eq!(
                captured_name(written, written.len()).as_deref(),
                Some(expected)
            );
            assert!(captured_name(written, written.len() - 1).is_none());
        }
        for written in [
            "", "_a", "a_", "a__b", "9a", "a-b", "a b", "λ", "\\", "\\\\", "\\a\\b\\", "\\a\nb\\",
        ] {
            assert!(captured_name(written, usize::MAX).is_none(), "{written:?}");
        }
    }
}
