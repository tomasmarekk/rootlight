//! Reads parenthesized source boundaries without constructing a syntax tree.
//! Consumers retain responsibility for expression validity and name resolution;
//! strings and escaped names cannot contribute structural parentheses.

use super::logical_whitespace_len;

/// Returns the exclusive end of one parenthesized VBA expression group.
///
/// Input must start with `(`. Nested parentheses, doubled quotes, bracketed
/// names and space/tab-based logical continuations preserve exact byte offsets.
/// A bare newline, comment, unterminated quoted region or missing closing
/// parenthesis returns None. A string can end at a continuation, as specified
/// by MS-VBAL 3.3.4. This checks boundaries, not expression or name validity;
/// successful scanning alone must never establish a semantic binding.
///
/// Scanning is linear, allocation-free and nonrecursive. The callback runs
/// before scanning and periodically within long tokens and whitespace runs.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
///
/// # Examples
/// ```
/// use rootlight_vba::parenthesized_expression_end;
/// let input = b"(Len(\"(\"), index) = value";
/// let end = parenthesized_expression_end(input, &mut || Ok::<(), std::convert::Infallible>(()))?
///     .expect("the source has one complete group");
/// assert_eq!(&input[..end], b"(Len(\"(\"), index)");
/// # Ok::<(), std::convert::Infallible>(())
/// ```
pub fn parenthesized_expression_end<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    check()?;
    if source.first() != Some(&b'(') {
        return Ok(None);
    }
    let mut cursor = 1;
    let mut depth = 1_usize;
    while let Some(&byte) = source.get(cursor) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        match byte {
            b' ' | b'\t' => {
                cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
            }
            b'"' | b'[' => {
                let Some(end) = quoted_end(source, cursor, byte, check)? else {
                    return Ok(None);
                };
                cursor = end;
            }
            b'(' => {
                // Each level consumes an input byte, so depth cannot exceed len.
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth -= 1;
                cursor += 1;
                if depth == 0 {
                    return Ok(Some(cursor));
                }
            }
            b'\r' | b'\n' | b'\'' => return Ok(None),
            _ => cursor += 1,
        }
    }
    Ok(None)
}

pub(crate) fn quoted_end<E>(
    source: &[u8],
    start: usize,
    opening: u8,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    let closing = if opening == b'[' { b']' } else { b'"' };
    let mut cursor = start + 1;
    let mut checkpoint = cursor;
    while let Some(&byte) = source.get(cursor) {
        // Doubled quotes advance by two bytes and can skip an exact modulus.
        if cursor >= checkpoint {
            check()?;
            checkpoint = cursor.saturating_add(512);
        }
        if byte == closing {
            cursor += 1;
            if opening == b'"' && source.get(cursor) == Some(&b'"') {
                cursor += 1;
            } else {
                return Ok(Some(cursor));
            }
        } else if matches!(byte, b'\r' | b'\n') {
            return Ok(None);
        } else if opening == b'"' && matches!(byte, b' ' | b'\t') {
            // A continuation terminates a STRING token instead of embedding
            // a physical newline in its value (MS-VBAL 3.3.4).
            if super::trivia(source, &mut cursor, check)? {
                return Ok(Some(cursor));
            }
        } else {
            cursor += 1;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_preserve_quotes_names_continuations_and_exact_end() {
        for group in [
            "()",
            "(Inner(0), (1 + 2))",
            "(\"(\", \"x\"\"y\", \"'\", [)])",
            "(index, _\n next)",
            "(index, _\r\n _\r\n next)",
            "(\"continued string _\n , next)",
            "(\"continued string \t_\r\n , next)",
            "(#1/2/2000 10:30#, 1#)",
            "(\"λ𝄞\", [界])",
        ] {
            let input = format!("{group} = value");
            assert_eq!(
                parenthesized_expression_end(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                Some(group.len()),
                "{group:?}"
            );
        }
    }

    #[test]
    fn incomplete_groups_never_consume_another_statement() {
        for input in [
            "",
            " ()",
            "(1",
            "(\"open)",
            "([open)",
            "(1\n)",
            "(1\r\n)",
            "(1 ' )",
            "(\"line\n\")",
            "(name_\n)",
            "([name _\n])",
        ] {
            assert_eq!(
                parenthesized_expression_end(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                None,
                "{input:?}"
            );
        }
    }

    #[test]
    fn group_scans_cancel_inside_long_regions_without_recursion() {
        for input in [
            format!("({})", "x".repeat(8192)),
            format!("(\"{}\")", "x".repeat(8192)),
            format!("(\"x{}\")", "\"\"".repeat(8192)),
            format!("([{}])", "x".repeat(8192)),
            format!("({})", " ".repeat(8192)),
            format!("({})", " _\n".repeat(8192)),
            format!("{}{}", "(".repeat(8192), ")".repeat(8192)),
        ] {
            for limit in [1, 3, 6] {
                let mut calls = 0;
                let result = parenthesized_expression_end(input.as_bytes(), &mut || {
                    calls += 1;
                    if calls == limit {
                        Err("cancelled")
                    } else {
                        Ok(())
                    }
                });
                assert_eq!(result, Err("cancelled"));
                assert_eq!(calls, limit);
            }
            assert_eq!(
                parenthesized_expression_end(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                Some(input.len())
            );
        }
    }

    #[test]
    fn arbitrary_bytes_and_prefixes_preserve_offset_bounds() {
        for byte in 0..=u8::MAX {
            for input in [vec![b'(', byte, b')'], vec![b'(', b'"', byte, b'"', b')']] {
                for length in 0..=input.len() {
                    let prefix = &input[..length];
                    if let Some(end) =
                        parenthesized_expression_end(prefix, &mut || Ok::<(), ()>(())).unwrap()
                    {
                        assert!(end > 0 && end <= prefix.len());
                        assert_eq!(prefix.get(end - 1), Some(&b')'));
                    }
                }
            }
        }
    }
}
