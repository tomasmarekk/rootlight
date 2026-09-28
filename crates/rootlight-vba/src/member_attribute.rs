//! Reads exported procedure dispatch attributes from the declaration header.
//! Executable statements, comments and strings never supply default-member metadata.

use std::ops::Range;

use crate::{logical_whitespace_len, name, parenthesized_expression_end, procedure_header};

/// Source-written dispatch metadata, retaining invalid declarations as barriers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberDispatchAttribute {
    /// No dispatch attribute occurs in the procedure's attribute header.
    Absent,
    /// One signed decimal dispatch ID and its exact attribute-line range.
    Value {
        /// Signed decimal dispatch identifier; zero denotes the default member.
        id: i32,
        /// Complete attribute line relative to the supplied declaration.
        span: Range<usize>,
    },
    /// Malformed or duplicate metadata cannot prove class-wide uniqueness.
    Invalid,
}

/// Reads an exported procedure's `VB_UserMemId` attribute.
///
/// The attribute must follow the complete parameter/header line and precede the
/// body. Its member name must match the procedure. Duplicate dispatch attributes
/// and malformed lines are invalid; IDs other than zero are not default members.
/// The range covers the complete attribute line, relative to `source`.
/// This only proves source metadata; callers must establish class membership,
/// visibility, uniqueness and the argument contract. No allocation occurs.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn member_dispatch_attribute<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<MemberDispatchAttribute, E> {
    use MemberDispatchAttribute::{Absent, Invalid, Value};
    let Some(header) = procedure_header(source, check)? else {
        return Ok(Invalid);
    };
    let mut cursor = header.name_span().end;
    cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
    let Some(end) = parenthesized_expression_end(source.get(cursor..).unwrap_or_default(), check)?
    else {
        return Ok(Invalid);
    };
    cursor += end;
    // Parameter strings and continuations have already been consumed. The
    // remaining header is an optional return type, never another statement.
    while let Some(byte) = source.get(cursor) {
        check()?;
        match byte {
            b'\n' => {
                cursor += 1;
                break;
            }
            b':' | b'\'' => return Ok(Absent),
            b'"' => return Ok(Invalid),
            b' ' | b'\t' => {
                cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
            }
            _ => cursor += 1,
        }
    }
    let mut found = Absent;
    let mut dispatch_seen = false;
    while cursor < source.len() {
        check()?;
        let start = cursor;
        while source
            .get(cursor)
            .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
        {
            if cursor.is_multiple_of(512) {
                check()?;
            }
            cursor += 1;
        }
        let end = cursor;
        if source.get(cursor) == Some(&b'\r') {
            cursor += 1;
        }
        if source.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }
        let line = source.get(start..end).unwrap_or_default().trim_ascii();
        if line.is_empty() {
            continue;
        }
        if !line
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"Attribute"))
            || !line.get(9).is_some_and(u8::is_ascii_whitespace)
        {
            break;
        }
        let mut at = 9;
        at += logical_whitespace_len(line.get(at..).unwrap_or_default(), check)?;
        let Some(owner) = name(line, &mut at, check)? else {
            return Ok(Invalid);
        };
        if !owner.eq_ignore_ascii_case(header.name()) || line.get(at) != Some(&b'.') {
            return Ok(Invalid);
        }
        at += 1;
        let Some(attribute) = name(line, &mut at, check)? else {
            return Ok(Invalid);
        };
        at += logical_whitespace_len(line.get(at..).unwrap_or_default(), check)?;
        if line.get(at) != Some(&b'=') {
            return Ok(Invalid);
        }
        let value = line.get(at + 1..).unwrap_or_default().trim_ascii();
        if attribute.eq_ignore_ascii_case(b"VB_UserMemId") {
            if dispatch_seen {
                return Ok(Invalid);
            }
            dispatch_seen = true;
            let Some(id) = std::str::from_utf8(value)
                .ok()
                .and_then(|text| text.parse::<i32>().ok())
            else {
                return Ok(Invalid);
            };
            found = Value {
                id,
                span: start..end,
            };
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_scans_preserve_bounds_and_propagate_mid_scan_cancellation() {
        let source = format!(
            "Public Function Read() As Long\nAttribute Read.VB_Description = \"{}\"\nAttribute Read.VB_UserMemId = 0\nEnd Function\n",
            "x".repeat(4096)
        );
        let mut calls = 0;
        assert_eq!(
            member_dispatch_attribute(source.as_bytes(), &mut || {
                calls += 1;
                if calls == 12 {
                    Err("work exhausted")
                } else {
                    Ok(())
                }
            }),
            Err("work exhausted")
        );
        for length in 0..=source.len() {
            let input = source.as_bytes().get(..length).unwrap();
            if let MemberDispatchAttribute::Value { span, .. } =
                member_dispatch_attribute(input, &mut || Ok::<(), ()>(())).unwrap()
            {
                assert!(input.get(span).is_some());
            }
        }
        for byte in 0..=u8::MAX {
            let input = [byte; 64];
            assert!(!matches!(
                member_dispatch_attribute(&input, &mut || Ok::<(), ()>(())).unwrap(),
                MemberDispatchAttribute::Value { .. }
            ));
        }
    }

    #[test]
    fn dispatch_attributes_require_the_matching_header_and_unique_zero_id() {
        let prefix = "Public Property Get [Fetch](ByVal key As Long) As Variant\r\n";
        for (attributes, expected) in [
            ("Attribute Fetch.VB_UserMemId = 0\r\n", true),
            ("attribute [fEtCh].VB_UserMemId = 0\n", true),
            (
                "Attribute Fetch.VB_Description = \"help\"\nAttribute Fetch.VB_UserMemId = 0\n",
                true,
            ),
            ("Attribute Fetch.VB_UserMemId = -4\n", false),
            ("Attribute Other.VB_UserMemId = 0\n", false),
            (
                "Attribute Fetch.VB_UserMemId = 0\nAttribute Fetch.VB_UserMemId = 1\n",
                false,
            ),
            ("Attribute Fetch.VB_UserMemId = 0: Stop\n", false),
            ("Attribute Fetch.VB_UserMemId = \"0\"\n", false),
            ("' Attribute Fetch.VB_UserMemId = 0\n", false),
            ("Fetch = \"Attribute Fetch.VB_UserMemId = 0\"\n", false),
            ("Fetch = 1\nAttribute Fetch.VB_UserMemId = 0\n", false),
        ] {
            let source = format!("{prefix}{attributes}End Property\n");
            let result =
                member_dispatch_attribute(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(
                matches!(result, MemberDispatchAttribute::Value { id: 0, .. }),
                expected,
                "{attributes}"
            );
            if let MemberDispatchAttribute::Value { id: 0, span } = result {
                assert!(
                    source
                        .get(span)
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("vb_usermemid = 0")
                );
            }
        }
        assert_eq!(
            member_dispatch_attribute(prefix.as_bytes(), &mut || Err("cancelled")),
            Err("cancelled")
        );
    }
}
