//! Protocol Buffers facts describe exact written schema owners and names.
//! Descriptor, import, extension and wire-format validation are separate from
//! these bounded lexical identities and never follow from a successful parse.

use std::borrow::Cow;

use rootlight_ir::EntityKind;

use crate::{SyntaxFact, SyntaxFactKind};

pub(super) fn entity_kind(fact: &SyntaxFact) -> Option<EntityKind> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "protobuf.file.module") => Some(EntityKind::Module),
        (SyntaxFactKind::Declaration, "protobuf.package.declaration") => {
            Some(EntityKind::Namespace)
        }
        (SyntaxFactKind::Declaration, "protobuf.message.declaration") => Some(EntityKind::Struct),
        (SyntaxFactKind::Declaration, "protobuf.enum.declaration") => Some(EntityKind::Enum),
        (SyntaxFactKind::Declaration, "protobuf.enum_value.declaration") => {
            Some(EntityKind::Constant)
        }
        (
            SyntaxFactKind::Declaration,
            "protobuf.field.declaration"
            | "protobuf.map_field.declaration"
            | "protobuf.oneof_field.declaration",
        ) => Some(EntityKind::Field),
        (SyntaxFactKind::Declaration, "protobuf.oneof.declaration") => Some(EntityKind::Union),
        (SyntaxFactKind::Declaration, "protobuf.service.declaration") => {
            Some(EntityKind::Interface)
        }
        (SyntaxFactKind::Declaration, "protobuf.rpc.declaration") => Some(EntityKind::Method),
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
        if tail.starts_with(b"//") {
            offset = offset.checked_add(
                tail.iter()
                    .position(|byte| *byte == b'\n')
                    .unwrap_or(tail.len()),
            )?;
        } else if tail.starts_with(b"/*") {
            let end = tail.get(2..)?.windows(2).position(|pair| pair == b"*/")?;
            offset = offset.checked_add(end.checked_add(4)?)?;
        } else {
            return Some(offset);
        }
    }
}

/// Canonicalizes one written identifier path without case folding or type inference.
///
/// Only grammar trivia between ASCII identifiers and dots may be removed. Both
/// physical input and canonical output must fit the caller's byte ceiling.
pub(super) fn captured_name(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.is_empty() || text.len() > maximum_bytes {
        return None;
    }
    if identifier(text) {
        return Some(Cow::Borrowed(text));
    }
    let bytes = text.as_bytes();
    let mut offset = trivia(bytes, 0)?;
    let mut canonical = String::new();
    canonical.try_reserve_exact(text.len()).ok()?;
    if bytes.get(offset) == Some(&b'.') {
        canonical.push('.');
        offset = trivia(bytes, offset.checked_add(1)?)?;
    }
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
        canonical.push_str(text.get(start..offset)?);
        offset = trivia(bytes, offset)?;
        if offset == bytes.len() {
            return Some(if canonical == text {
                Cow::Borrowed(text)
            } else {
                Cow::Owned(canonical)
            });
        }
        if bytes.get(offset) != Some(&b'.') {
            return None;
        }
        canonical.push('.');
        offset = trivia(bytes, offset.checked_add(1)?)?;
    }
}

pub(super) fn captured_name_for_fact<'a>(
    fact: &SyntaxFact,
    text: &'a str,
    maximum_bytes: usize,
) -> Option<Cow<'a, str>> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "protobuf.name.definition") => {
            (text.len() <= maximum_bytes && identifier(text)).then_some(Cow::Borrowed(text))
        }
        (
            SyntaxFactKind::Occurrence,
            "protobuf.package_name.definition" | "protobuf.type.reference",
        ) => captured_name(text, maximum_bytes),
        (SyntaxFactKind::Import, "protobuf.written_import.import") => (text.len() <= maximum_bytes
            && text.starts_with('"')
            && text.ends_with('"')
            && text.len() >= 2
            && !text.contains(['\0', '\r', '\n']))
        .then_some(Cow::Borrowed(text)),
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
    fn exact_written_labels_do_not_classify_foreign_or_unknown_roles() {
        for (label, expected) in [
            ("protobuf.message.declaration", EntityKind::Struct),
            ("protobuf.enum.declaration", EntityKind::Enum),
            ("protobuf.enum_value.declaration", EntityKind::Constant),
            ("protobuf.field.declaration", EntityKind::Field),
            ("protobuf.map_field.declaration", EntityKind::Field),
            ("protobuf.oneof_field.declaration", EntityKind::Field),
            ("protobuf.oneof.declaration", EntityKind::Union),
            ("protobuf.service.declaration", EntityKind::Interface),
            ("protobuf.rpc.declaration", EntityKind::Method),
            ("protobuf.package.declaration", EntityKind::Namespace),
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
            "protobuf.unknown.declaration",
            "protobuf.message.declaration.extra",
            "proto.message.declaration",
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
    fn paths_preserve_case_absolute_roots_and_byte_limits() {
        for (written, canonical) in [
            ("Record", "Record"),
            ("Outer.Inner", "Outer.Inner"),
            (".api.Outer.Inner", ".api.Outer.Inner"),
            ("api /* note */ . Outer", "api.Outer"),
            ("api.// note\nOuter", "api.Outer"),
        ] {
            assert_eq!(
                captured_name(written, written.len()).as_deref(),
                Some(canonical)
            );
            assert!(captured_name(written, written.len() - 1).is_none());
        }
        assert_ne!(captured_name("Record", 64), captured_name("record", 64));
        for invalid in ["", ".", "A..B", "A.", "9Type", "A/B", "A B", "A/*", "Δ"] {
            assert!(captured_name(invalid, 64).is_none(), "{invalid}");
        }
    }
}
