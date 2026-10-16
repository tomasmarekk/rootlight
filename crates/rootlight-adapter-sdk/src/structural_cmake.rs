//! CMake entities describe written declarations, never evaluated build state.
//! Macro placeholders are authored formal arguments, without function scope.

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "cmake.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "cmake.function.declaration") => Some(EntityKind::Function),
        (SyntaxFactKind::Declaration, "cmake.macro.declaration") => Some(EntityKind::Function),
        (
            SyntaxFactKind::Declaration,
            "cmake.parameter.declaration" | "cmake.macro_placeholder.declaration",
        ) => Some(EntityKind::Parameter),
        (
            SyntaxFactKind::Declaration,
            "cmake.normal_assignment.declaration"
            | "cmake.cache_assignment.declaration"
            | "cmake.environment_assignment.declaration"
            | "cmake.option.declaration",
        ) => Some(EntityKind::Variable),
        (SyntaxFactKind::Declaration, "cmake.target.declaration") => Some(EntityKind::BuildTarget),
        _ => None,
    }
}

/// Retains literal spelling without unescaping or evaluating variable contents.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    (!text.is_empty() && text.len() <= maximum_bytes && !text.chars().any(char::is_control))
        .then_some(text)
}

pub(super) fn command_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let mut bytes = text.bytes();
    (text.len() <= maximum_bytes
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(text)
}

fn template_command_name(text: &str, maximum_bytes: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    if text.len() > maximum_bytes
        || !bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(*byte, b'_' | b'@'))
    {
        return None;
    }
    let mut index = 0;
    let mut markers = 0;
    while let Some(byte) = bytes.get(index).copied() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            index += 1;
        } else if byte == b'@' {
            let start = index;
            index += 1;
            while bytes.get(index).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'/' | b'+' | b'-')
            }) {
                index += 1;
            }
            if index == start + 1 || bytes.get(index) != Some(&b'@') {
                return None;
            }
            index += 1;
            markers += 1;
        } else {
            return None;
        }
    }
    (markers != 0).then_some(text)
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<&'a str> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "cmake.command.call") => command_name(text, maximum_bytes),
        (SyntaxFactKind::Occurrence, "cmake.template_command.call") => {
            template_command_name(text, maximum_bytes)
        }
        (
            SyntaxFactKind::Occurrence,
            "cmake.name.definition"
            | "cmake.normal_variable.reference"
            | "cmake.cache_variable.reference"
            | "cmake.environment_variable.reference",
        )
        | (SyntaxFactKind::Import, "cmake.written_import.import") => {
            captured_name(text, maximum_bytes)
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

    fn fact(kind: SyntaxFactKind, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            1,
            None,
            kind,
            SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn declaration_roles_and_names_preserve_written_categories() {
        for (label, kind) in [
            ("cmake.function.declaration", EntityKind::Function),
            ("cmake.macro.declaration", EntityKind::Function),
            ("cmake.parameter.declaration", EntityKind::Parameter),
            ("cmake.macro_placeholder.declaration", EntityKind::Parameter),
            ("cmake.normal_assignment.declaration", EntityKind::Variable),
            ("cmake.cache_assignment.declaration", EntityKind::Variable),
            ("cmake.option.declaration", EntityKind::Variable),
            (
                "cmake.environment_assignment.declaration",
                EntityKind::Variable,
            ),
            ("cmake.target.declaration", EntityKind::BuildTarget),
        ] {
            assert_eq!(
                entity_kind(&fact(SyntaxFactKind::Declaration, label)),
                Some(kind)
            );
            assert_eq!(entity_kind(&fact(SyntaxFactKind::Occurrence, label)), None);
            assert_eq!(
                entity_kind(&fact(
                    SyntaxFactKind::Declaration,
                    &format!("{label}.extra")
                )),
                None
            );
        }
        assert_eq!(
            entity_kind(&fact(SyntaxFactKind::Module, "cmake.file.module")),
            Some(EntityKind::Module)
        );
        for name in [
            "Written",
            "written",
            "literal name",
            "ENV{PATH}",
            "CACHE{option}",
            "name.with-dash",
            "λ",
        ] {
            assert_eq!(captured_name(name, name.len()), Some(name));
            assert_eq!(captured_name(name, name.len() - 1), None);
        }
        for name in ["", "name\nother", "name\0", "\tname"] {
            assert_eq!(captured_name(name, usize::MAX), None);
        }
    }

    #[test]
    fn template_command_names_require_complete_authored_markers_and_exact_roles() {
        let template = fact(SyntaxFactKind::Occurrence, "cmake.template_command.call");
        for name in ["prefix_@NAME@_suffix", "@NAME@", "prefix@A.B/C+-1@@OTHER@"] {
            assert_eq!(
                captured_name_for_fact(&template, name, name.len()),
                Some(name)
            );
            assert_eq!(
                captured_name_for_fact(&template, name, name.len() - 1),
                None
            );
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Occurrence, "cmake.command.call"),
                    name,
                    usize::MAX
                ),
                None
            );
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Declaration, "cmake.template_command.call"),
                    name,
                    usize::MAX
                ),
                None
            );
        }
        for name in [
            "",
            "ordinary",
            "prefix_@@",
            "prefix_@NAME",
            "2prefix_@NAME@",
            "prefix_@NAME@()",
            "prefix_@NAME@ other",
            "prefix_@BAD NAME@",
            "prefix_@NAME@\n",
        ] {
            assert_eq!(
                captured_name_for_fact(&template, name, usize::MAX),
                None,
                "{name:?}"
            );
        }
    }

    #[test]
    fn occurrence_admission_cannot_cross_role_or_language_labels() {
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Occurrence, "cmake.command.call"),
                "MiXeD",
                5
            ),
            Some("MiXeD")
        );
        for name in ["name()", "${dispatch}", "name.member", "2name"] {
            assert_eq!(
                captured_name_for_fact(
                    &fact(SyntaxFactKind::Occurrence, "cmake.command.call"),
                    name,
                    usize::MAX
                ),
                None
            );
        }
        for label in [
            "cmake.unknown.reference",
            "cmake.command.reference",
            "rust.name.definition",
        ] {
            assert_eq!(
                captured_name_for_fact(&fact(SyntaxFactKind::Occurrence, label), "Name", 4),
                None
            );
        }
        assert_eq!(
            captured_name_for_fact(
                &fact(SyntaxFactKind::Signature, "cmake.name.definition"),
                "Name",
                4
            ),
            None
        );
    }
}
