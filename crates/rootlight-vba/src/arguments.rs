//! Splits authored argument lists without a parser runtime or expression evaluation.
//! Delimiters inside strings, dates, names and nested applications do not create
//! arguments; callers must separately prove expression validity and parameter binding.

use super::{group::quoted_end, logical_whitespace_len, name, parenthesized_expression_end};

/// One argument borrowing its original expression and optional unbracketed name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argument<'a> {
    name: Option<&'a [u8]>,
    name_span: Option<(usize, usize)>,
    expression: &'a [u8],
}

impl<'a> Argument<'a> {
    /// Authored named-argument identifier, without brackets; None means positional.
    #[must_use]
    pub const fn name(self) -> Option<&'a [u8]> {
        self.name
    }

    /// Exact named-label range relative to the input argument list, including brackets.
    ///
    /// Positional arguments return None. Logical whitespace and `:=` are excluded.
    #[must_use]
    pub fn name_span(self) -> Option<std::ops::Range<usize>> {
        self.name_span.map(|(start, end)| start..end)
    }

    /// Expression bytes after leading logical whitespace; empty means omitted.
    ///
    /// Trailing whitespace is preserved. This is not proof of expression validity,
    /// runtime type compatibility, or a callable target.
    #[must_use]
    pub const fn expression(self) -> &'a [u8] {
        self.expression
    }
}

/// Reads a complete VBA argument list, excluding its enclosing parentheses.
///
/// Returns None for unclosed regions, bare newlines, comments, invalid named-order
/// boundaries, trailing omitted arguments, or more than `maximum_arguments`.
/// Empty logical whitespace represents zero arguments; empty comma-delimited slots
/// represent omitted positional arguments. Names are ASCII and may be bracketed.
/// Nested groups, doubled string quotes, date literals and logical continuations
/// preserve source offsets. Expressions and ByVal legality are caller obligations.
/// Allocation is bounded by the argument limit and input length; scanning is
/// nonrecursive, with periodic cancellation/work callbacks throughout byte scans.
///
/// # Errors
/// Returns the caller's error unchanged if the callback fails.
pub fn argument_list<'a, E>(
    source: &'a [u8],
    maximum_arguments: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<Argument<'a>>>, E> {
    check()?;
    let mut result = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    let mut named = false;
    loop {
        check()?;
        let boundary = match source.get(cursor) {
            None | Some(b',') => true,
            Some(b' ' | b'\t') => {
                cursor += logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?;
                continue;
            }
            Some(b'(') => {
                let Some(end) =
                    parenthesized_expression_end(source.get(cursor..).unwrap_or_default(), check)?
                else {
                    return Ok(None);
                };
                cursor += end;
                continue;
            }
            Some(&opening @ (b'"' | b'[')) => {
                let Some(end) = quoted_end(source, cursor, opening, check)? else {
                    return Ok(None);
                };
                cursor = end;
                continue;
            }
            Some(b'#')
                if cursor == 0
                    || source
                        .get(cursor - 1)
                        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_') =>
            {
                cursor += 1;
                while source
                    .get(cursor)
                    .is_some_and(|byte| !matches!(byte, b'#' | b'\r' | b'\n'))
                {
                    if cursor.is_multiple_of(512) {
                        check()?;
                    }
                    cursor += 1;
                }
                if source.get(cursor) != Some(&b'#') {
                    return Ok(None);
                }
                cursor += 1;
                continue;
            }
            Some(b')' | b'\r' | b'\n' | b'\'') => return Ok(None),
            Some(b':') if source.get(cursor + 1) != Some(&b'=') => return Ok(None),
            Some(_) => false,
        };
        if !boundary {
            cursor += 1;
            continue;
        }
        let raw = source.get(start..cursor).unwrap_or_default();
        let leading = logical_whitespace_len(raw, check)?;
        let raw = raw.get(leading..).unwrap_or_default();
        if raw.is_empty() && source.get(cursor).is_none() {
            return Ok(result.is_empty().then_some(result));
        }
        let mut end = 0;
        let candidate = name(raw, &mut end, check)?;
        let name_end = end;
        end += logical_whitespace_len(raw.get(end..).unwrap_or_default(), check)?;
        let (name, name_span, expression) = if let Some(name) = candidate
            && let Some(expression) = raw.get(end..).and_then(|tail| tail.strip_prefix(b":="))
        {
            named = true;
            let expression_leading = logical_whitespace_len(expression, check)?;
            let expression = expression.get(expression_leading..).unwrap_or_default();
            if expression.is_empty() {
                return Ok(None);
            }
            (
                Some(name),
                Some((start + leading, start + leading + name_end)),
                expression,
            )
        } else {
            if named {
                return Ok(None);
            }
            (None, None, raw)
        };
        if result.len() == maximum_arguments {
            return Ok(None);
        }
        result.push(Argument {
            name,
            name_span,
            expression,
        });
        if source.get(cursor).is_none() {
            return Ok(Some(result));
        }
        cursor += 1;
        start = cursor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_preserve_nested_regions_omissions_names_and_boundaries() {
        let source = b", Inner(1, 2), \"a,\"\"b\", #1/2/2000 10:30#, [last] := _\r\n 4";
        let args = argument_list(source, 5, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(args.len(), 5);
        assert_eq!(args[0].expression(), b"");
        assert_eq!(args[1].expression(), b"Inner(1, 2)");
        assert_eq!(args[2].expression(), b"\"a,\"\"b\"");
        assert_eq!(args[3].expression(), b"#1/2/2000 10:30#");
        assert_eq!(args[4].name(), Some(b"last".as_slice()));
        assert_eq!(&source[args[4].name_span().unwrap()], b"[last]");
        assert!(
            args[..4]
                .iter()
                .all(|argument| argument.name_span().is_none())
        );
        assert_eq!(args[4].expression(), b"4");
        assert!(
            argument_list(source, 4, &mut || Ok::<(), ()>(()))
                .unwrap()
                .is_none()
        );
        assert!(
            argument_list(b" _\n ", 0, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn incomplete_or_reordered_argument_lists_do_not_establish_boundaries() {
        for source in [
            "1,",
            "value:=",
            "value:=1, 2",
            "(1",
            "1)",
            "\"open",
            "[open",
            "#open",
            "1\n,2",
            "1 ' comment",
            "1: Run",
            "value:=1,,other:=2",
        ] {
            assert!(
                argument_list(source.as_bytes(), 20, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn named_label_spans_preserve_brackets_continuations_and_byte_offsets() {
        let source = b"\"\xc3\xa9\", inner([value]:=1), _\r\n [value] _\r\n := 2, extra:=\"x\"";
        let arguments = argument_list(source, 4, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(arguments.len(), 4);
        assert!(arguments[0].name_span().is_none());
        assert!(arguments[1].name_span().is_none());
        for (argument, expected) in [
            (arguments[2], b"[value]".as_slice()),
            (arguments[3], b"extra".as_slice()),
        ] {
            assert_eq!(&source[argument.name_span().unwrap()], expected);
        }
        for end in 0..=source.len() {
            if let Some(arguments) =
                argument_list(&source[..end], 4, &mut || Ok::<(), ()>(())).unwrap()
            {
                for argument in arguments {
                    if let Some(span) = argument.name_span() {
                        assert!(span.start < span.end && span.end <= end);
                        let authored = &source[span];
                        let name = if authored.starts_with(b"[") {
                            &authored[1..authored.len() - 1]
                        } else {
                            authored
                        };
                        assert_eq!(argument.name(), Some(name));
                    }
                }
            }
        }
    }

    #[test]
    fn argument_scans_propagate_cancellation_and_accept_arbitrary_bytes_without_panics() {
        for source in [
            vec![b' '; 4096],
            b"\"a\"\"b\"".repeat(1024),
            b"(1,2)".repeat(1024),
        ] {
            let mut remaining = 3;
            assert_eq!(
                argument_list(&source, 32, &mut || {
                    remaining -= 1;
                    if remaining == 0 { Err(7) } else { Ok(()) }
                }),
                Err(7)
            );
        }
        for byte in 0..=255_u8 {
            let _ = argument_list(&[byte; 12], 4, &mut || Ok::<(), ()>(())).unwrap();
        }
    }
}
