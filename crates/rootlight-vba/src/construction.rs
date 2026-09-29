//! Reads the authored class path of an explicit New expression.
//! Source ranges survive native statement recovery; lookup and lifecycle effects
//! remain separate proof obligations for the project resolver.

use std::ops::Range;

use crate::{logical_whitespace_len, name, trivia};

/// Returns the exact class-name range following an initial VBA `New` keyword.
///
/// Accepts simple and dotted ASCII names, brackets and explicit continuations.
/// The range excludes `New` and trailing trivia. Input may continue at a comma,
/// closing parenthesis, operator, statement boundary or apostrophe comment. Incomplete
/// paths, argument lists and unsupported suffixes return None, not a valid prefix.
/// This allocation-free reader neither resolves a class nor proves initialization.
/// Every scan observes the caller's cancellation/work callback.
///
/// # Errors
/// Propagates the callback's error unchanged.
pub fn constructed_type_range<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Range<usize>>, E> {
    check()?;
    if !source
        .get(..3)
        .is_some_and(|word| word.eq_ignore_ascii_case(b"New"))
    {
        return Ok(None);
    }
    let suffix = source.get(3..).unwrap_or_default();
    let whitespace = logical_whitespace_len(suffix, check)?;
    if whitespace == 0 && suffix.first() != Some(&b'[') {
        return Ok(None);
    }
    let start = 3 + whitespace;
    let mut cursor = start;
    if name(source, &mut cursor, check)?.is_none() {
        return Ok(None);
    }
    loop {
        let end = cursor;
        let continued = trivia(source, &mut cursor, check)?;
        match source.get(cursor) {
            // An adjacent ampersand belongs to a bare identifier's type
            // character, not to the following concatenation expression.
            Some(b'&') if cursor == end && source.get(end - 1) != Some(&b']') => {
                return Ok(None);
            }
            Some(b'.') => {
                if cursor != end && !continued {
                    return Ok(None);
                }
                cursor += 1;
                trivia(source, &mut cursor, check)?;
                if name(source, &mut cursor, check)?.is_none() {
                    return Ok(None);
                }
            }
            None
            | Some(
                b',' | b')' | b'\r' | b'\n' | b':' | b'\'' | b';' | b'+' | b'-' | b'*' | b'/'
                | b'\\' | b'^' | b'&' | b'=' | b'<' | b'>',
            ) => {
                return Ok(Some(start..end));
            }
            _ if cursor != end => {
                let tail = source.get(cursor..).unwrap_or_default();
                let mut next = 0;
                let boundary = name(tail, &mut next, check)?.is_some_and(|word| {
                    tail.first() != Some(&b'[')
                        && [
                            b"Is".as_slice(),
                            b"And",
                            b"Or",
                            b"Xor",
                            b"Eqv",
                            b"Imp",
                            b"Mod",
                            b"Like",
                            b"Then",
                            b"Else",
                            b"To",
                            b"Step",
                        ]
                        .iter()
                        .any(|keyword| word.eq_ignore_ascii_case(keyword))
                });
                return Ok(boundary.then_some(start..end));
            }
            _ => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    #[test]
    fn construction_paths_preserve_spelling_and_boundaries() {
        for (source, expected) in [
            ("New Target", "Target"),
            ("New[Target]", "[Target]"),
            ("nEw Primary.Target\nnext", "Primary.Target"),
            ("New [Primary].[Target], next", "[Primary].[Target]"),
            (
                "New _\r\n Primary _\r\n .Target)\n",
                "Primary _\r\n .Target",
            ),
            ("New Target ' note", "Target"),
            ("New Target : next", "Target"),
            ("New Primary.Target.Extra", "Primary.Target.Extra"),
            ("New Target Is Nothing Then", "Target"),
            ("New Target + value", "Target"),
            ("New Target & value", "Target"),
        ] {
            let range = constructed_type_range(source.as_bytes(), &mut || Ok::<_, Infallible>(()))
                .unwrap()
                .unwrap();
            assert_eq!(&source[range], expected);
        }
        for source in [
            "New",
            "NewTarget",
            "New\nTarget",
            "New .Target",
            "New Primary.",
            "New Primary .Target",
            "New Primary!Target",
            "New Target()",
            "New Target(1)",
            "New Target$",
            "New Target&",
            "New Target%",
            "New [Target",
            "New Target Other",
            "New Target [Is]",
            "New Target _\n .",
        ] {
            assert_eq!(
                constructed_type_range(source.as_bytes(), &mut || Ok::<_, Infallible>(())).unwrap(),
                None,
                "{source}"
            );
        }
    }

    #[test]
    fn construction_scans_propagate_work_cancellation() {
        for source in [
            format!("New {}Target", " ".repeat(8192)),
            format!("New {}", "a".repeat(8192)),
        ] {
            let mut calls = 0;
            let result = constructed_type_range(source.as_bytes(), &mut || {
                calls += 1;
                if calls == 6 { Err("cancelled") } else { Ok(()) }
            });
            assert_eq!(result, Err("cancelled"));
            assert_eq!(calls, 6);
        }
        for byte in 0..=u8::MAX {
            for length in 0..48 {
                let mut source = b"New ".to_vec();
                source.extend(std::iter::repeat_n(byte, length));
                let range =
                    constructed_type_range(&source, &mut || Ok::<_, Infallible>(())).unwrap();
                if let Some(range) = range {
                    assert!(range.start >= 4 && range.start < range.end);
                    assert!(range.end <= source.len());
                }
            }
        }
    }
}
