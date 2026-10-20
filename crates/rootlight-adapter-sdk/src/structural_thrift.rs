//! Thrift names retain written source identity without codegen namespace inference.

use std::borrow::Cow;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "thrift.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "thrift.namespace.declaration") => {
            Some(EntityKind::Namespace)
        }
        (SyntaxFactKind::Declaration, "thrift.typedef.declaration") => Some(EntityKind::TypeAlias),
        (SyntaxFactKind::Declaration, "thrift.enum.declaration") => Some(EntityKind::Enum),
        (
            SyntaxFactKind::Declaration,
            "thrift.enum_value.declaration" | "thrift.constant.declaration",
        ) => Some(EntityKind::Constant),
        (
            SyntaxFactKind::Declaration,
            "thrift.struct.declaration" | "thrift.exception.declaration",
        ) => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "thrift.union.declaration") => Some(EntityKind::Union),
        (SyntaxFactKind::Declaration, "thrift.service.declaration") => Some(EntityKind::Interface),
        (SyntaxFactKind::Declaration, "thrift.function.declaration") => Some(EntityKind::Method),
        (SyntaxFactKind::Declaration, "thrift.field.declaration") => Some(EntityKind::Field),
        (
            SyntaxFactKind::Declaration,
            "thrift.parameter.declaration" | "thrift.throws_parameter.declaration",
        ) => Some(EntityKind::Parameter),
        _ => None,
    }
}

fn identifier(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn trivia(bytes: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
            offset = offset.checked_add(1)?;
        }
        let tail = bytes.get(offset..)?;
        if tail.starts_with(b"//") || tail.starts_with(b"#") {
            offset = offset.checked_add(
                tail.iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(tail.len()),
            )?;
        } else if tail.starts_with(b"/*") {
            offset = offset.checked_add(
                tail.get(2..)?
                    .windows(2)
                    .position(|part| part == b"*/")?
                    .checked_add(4)?,
            )?;
        } else {
            return Some(offset);
        }
    }
}

pub(super) fn captured_name(text: &str, maximum: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    if identifier(text) {
        return Some(Cow::Borrowed(text));
    }
    let bytes = text.as_bytes();
    let mut offset = trivia(bytes, 0)?;
    let mut result = String::new();
    result.try_reserve_exact(text.len()).ok()?;
    loop {
        let start = offset;
        let first = *bytes.get(offset)?;
        if !first.is_ascii_alphabetic() && first != b'_' {
            return None;
        }
        offset = offset.checked_add(1)?;
        while bytes
            .get(offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            offset = offset.checked_add(1)?;
        }
        result.push_str(text.get(start..offset)?);
        offset = trivia(bytes, offset)?;
        if offset == bytes.len() {
            return Some(if result == text {
                Cow::Borrowed(text)
            } else {
                Cow::Owned(result)
            });
        }
        if bytes.get(offset) != Some(&b'.') {
            return None;
        }
        result.push('.');
        offset = trivia(bytes, offset.checked_add(1)?)?;
    }
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum: usize,
) -> Option<Cow<'a, str>> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "thrift.name.definition") => {
            (text.len() <= maximum && identifier(text)).then_some(Cow::Borrowed(text))
        }
        (
            SyntaxFactKind::Occurrence,
            "thrift.type.reference"
            | "thrift.service_base.reference"
            | "thrift.uuid_type.reference",
        ) => captured_name(text, maximum),
        // The selector belongs to the written namespace identity. The raw span
        // is codegen metadata, never a prefix for local source type resolution.
        (SyntaxFactKind::Occurrence, "thrift.namespace_name.definition") => {
            (!text.is_empty() && text.len() <= maximum && !text.contains('\0'))
                .then_some(Cow::Borrowed(text))
        }
        (SyntaxFactKind::Import, "thrift.written_include.import") => {
            let quote = text.as_bytes().first().copied();
            (text.len() >= 2
                && text.len() <= maximum
                && matches!(quote, Some(b'\'' | b'"'))
                && text.as_bytes().last().copied() == quote
                && !text.contains(['\0', '\r', '\n']))
            .then_some(Cow::Borrowed(text))
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
    fn only_exact_reviewed_labels_classify_written_declarations() {
        for (label, expected) in [
            ("thrift.typedef.declaration", EntityKind::TypeAlias),
            ("thrift.enum.declaration", EntityKind::Enum),
            ("thrift.enum_value.declaration", EntityKind::Constant),
            ("thrift.struct.declaration", EntityKind::Struct),
            ("thrift.union.declaration", EntityKind::Union),
            ("thrift.exception.declaration", EntityKind::Struct),
            ("thrift.service.declaration", EntityKind::Interface),
            ("thrift.function.declaration", EntityKind::Method),
            ("thrift.field.declaration", EntityKind::Field),
            ("thrift.parameter.declaration", EntityKind::Parameter),
            ("thrift.throws_parameter.declaration", EntityKind::Parameter),
            ("thrift.namespace.declaration", EntityKind::Namespace),
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), Some(expected));
        }
        for label in [
            "thrift.unknown.declaration",
            "thrift.struct.declaration.extra",
            "thrift.type.reference",
            "other.struct.declaration",
        ] {
            let fact = SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Declaration,
                SourceSpan::new(FileId::from_bytes([4; 20]), 0, 1).unwrap(),
                0,
                SyntaxKindLabel::new(label).unwrap(),
            );
            assert_eq!(entity_kind(&fact), None);
        }
    }

    #[test]
    fn qualified_names_keep_case_and_only_remove_grammar_trivia() {
        for (source, expected) in [
            ("Record", "Record"),
            ("common /* x */ . Record", "common.Record"),
            ("common.# x\nRecord", "common.Record"),
        ] {
            assert_eq!(
                captured_name(source, source.len()).as_deref(),
                Some(expected)
            );
            assert!(captured_name(source, source.len() - 1).is_none());
        }
        for invalid in ["", ".Record", "A..B", "A.", "A/B", "A B", "9Name", "Δ"] {
            assert!(captured_name(invalid, 64).is_none());
        }
        assert_ne!(captured_name("Record", 64), captured_name("record", 64));
    }
}
