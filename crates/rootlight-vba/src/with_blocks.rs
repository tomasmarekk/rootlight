//! Reads source-bound With regions for implicit receiver lookup.
//! Lexical boundaries are checked independently of a native syntax tree; this
//! inventory proves nesting only, never the receiver's type or member binding.

use std::ops::Range;

/// A balanced With block retaining offsets into the unchanged input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithBlock {
    header: Range<usize>,
    receiver: Range<usize>,
    body: Range<usize>,
    terminator: Range<usize>,
}

impl WithBlock {
    /// Returns the opening statement, including the With keyword.
    #[must_use]
    pub fn header(&self) -> Range<usize> {
        self.header.clone()
    }

    /// Returns the exact expression evaluated when entering the block.
    #[must_use]
    pub fn receiver(&self) -> Range<usize> {
        self.receiver.clone()
    }

    /// Returns the body, excluding both boundary statements.
    #[must_use]
    pub fn body(&self) -> Range<usize> {
        self.body.clone()
    }

    /// Returns the matching End With statement.
    #[must_use]
    pub fn terminator(&self) -> Range<usize> {
        self.terminator.clone()
    }
}

/// Inventories balanced With blocks in opening-source order.
///
/// Strings, bracketed names, comments, dates and explicit continuations cannot
/// introduce statements. Nested headers belong to the outer body. Conditional
/// branches must preserve the entry stack; incomplete or ambiguous boundaries
/// return None for the entire input rather than exposing a misleading parent.
/// The reader does not validate expressions or infer runtime types.
///
/// Scanning is nonrecursive. Allocation is bounded by `maximum_blocks` and
/// input length; the callback runs per statement and periodically within scans.
/// Exceeding the block limit also returns None, without a partial inventory.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn with_blocks<E>(
    source: &[u8],
    maximum_blocks: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<WithBlock>>, E> {
    let mut blocks: Vec<WithBlock> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut branches: Vec<Option<usize>> = Vec::new();
    let mut cursor = 0;
    while cursor < source.len() {
        check()?;
        let Some((range, next)) = statement(source, cursor, true, check)? else {
            return Ok(None);
        };
        cursor = next;
        let mut start = range.start;
        let end = range.end;
        while source.get(start).is_some_and(u8::is_ascii_digit) && start < end {
            start += 1;
        }
        start += super::logical_whitespace_len(source.get(start..end).unwrap_or_default(), check)?;
        let text = source.get(start..end).unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let (first, rest) = word(text);
        if first.eq_ignore_ascii_case(b"With") {
            let offset = first.len() + super::logical_whitespace_len(rest, check)?;
            if offset == first.len() || offset >= text.len() || blocks.len() >= maximum_blocks {
                return Ok(None);
            }
            stack.push(blocks.len());
            blocks.push(WithBlock {
                header: start..end,
                receiver: start + offset..end,
                body: cursor..cursor,
                terminator: end..end,
            });
        } else if first.eq_ignore_ascii_case(b"End") {
            let rest = rest
                .get(super::logical_whitespace_len(rest, check)?..)
                .unwrap_or_default();
            if rest.eq_ignore_ascii_case(b"With") {
                let Some(open) = stack.pop().and_then(|i| blocks.get_mut(i)) else {
                    return Ok(None);
                };
                open.body.end = start;
                open.terminator = start..end;
            } else if !stack.is_empty()
                && [b"Sub".as_slice(), b"Function", b"Property"]
                    .iter()
                    .any(|word| rest.eq_ignore_ascii_case(word))
            {
                return Ok(None);
            }
        } else if text.first() == Some(&b'#') {
            let directive = text.get(1..).unwrap_or_default().trim_ascii_start();
            let (keyword, tail) = word(directive);
            if keyword.eq_ignore_ascii_case(b"If") {
                branches.push(stack.last().copied());
            } else if keyword.eq_ignore_ascii_case(b"Else")
                || keyword.eq_ignore_ascii_case(b"ElseIf")
            {
                if branches.last().copied() != Some(stack.last().copied()) {
                    return Ok(None);
                }
            } else if keyword.eq_ignore_ascii_case(b"End")
                && tail.trim_ascii().eq_ignore_ascii_case(b"If")
                && branches.pop() != Some(stack.last().copied())
            {
                return Ok(None);
            }
        }
    }
    Ok((stack.is_empty() && branches.is_empty()).then_some(blocks))
}

fn word(source: &[u8]) -> (&[u8], &[u8]) {
    let end = source
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
        .unwrap_or(source.len());
    source.split_at(end)
}

pub(crate) fn statement<E>(
    source: &[u8],
    start: usize,
    reject_inline_with: bool,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(Range<usize>, usize)>, E> {
    statement_with_boundary(source, start, reject_inline_with, check)
        .map(|statement| statement.map(|(range, next, _)| (range, next)))
}

/// Retains the untrimmed content boundary alongside the legacy statement range.
/// A CR paired with the delimiting LF is excluded; consumed continuation bytes
/// remain available so a closed consumer cannot reinterpret their trimmed tail.
pub(crate) fn statement_with_boundary<E>(
    source: &[u8],
    start: usize,
    reject_inline_with: bool,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(Range<usize>, usize, usize)>, E> {
    let mut cursor = start;
    let mut first = start;
    while source
        .get(first)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r') || b.is_ascii_digit())
    {
        if first.is_multiple_of(512) {
            check()?;
        }
        first += 1;
    }
    let directive = source.get(first) == Some(&b'#');
    let mut previous_word_end = None;
    let mut depth = 0_usize;
    while let Some(&byte) = source.get(cursor) {
        if cursor.is_multiple_of(512) {
            check()?;
        }
        match byte {
            b' ' | b'\t' => {
                cursor +=
                    super::logical_whitespace_len(source.get(cursor..).unwrap_or_default(), check)?
            }
            b'"' | b'[' => {
                let Some(end) = super::group::quoted_end(source, cursor, byte, check)? else {
                    return Ok(None);
                };
                cursor = end;
            }
            b'\'' => {
                if depth != 0 {
                    return Ok(None);
                }
                let end = cursor;
                while source.get(cursor).is_some_and(|b| *b != b'\n') {
                    if cursor.is_multiple_of(512) {
                        check()?;
                    }
                    cursor += 1;
                }
                return Ok(Some((
                    trimmed(source, start, end),
                    cursor + usize::from(cursor < source.len()),
                    end,
                )));
            }
            b'\n' | b':' if byte == b'\n' || source.get(cursor + 1) != Some(&b'=') => {
                if depth != 0 {
                    return Ok(None);
                }
                let content_end =
                    if byte == b'\n' && source.get(cursor.wrapping_sub(1)) == Some(&b'\r') {
                        cursor - 1
                    } else {
                        cursor
                    };
                return Ok(Some((
                    trimmed(source, start, cursor),
                    cursor + 1,
                    content_end,
                )));
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                let Some(next) = depth.checked_sub(1) else {
                    return Ok(None);
                };
                depth = next;
                cursor += 1;
            }
            // A decimal trailing point can precede a FLOAT Hash suffix. This
            // boundary reader leaves complete numeric validity to its consumers.
            b'#' if !directive
                && !source
                    .get(cursor.wrapping_sub(1))
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b')')
                && !(source.get(cursor.wrapping_sub(1)) == Some(&b'.')
                    && source
                        .get(cursor.wrapping_sub(2))
                        .is_some_and(u8::is_ascii_digit)) =>
            {
                cursor += 1;
                while source
                    .get(cursor)
                    .is_some_and(|b| !matches!(b, b'#' | b'\r' | b'\n'))
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
            }
            b if b.is_ascii_alphabetic() => {
                let begin = cursor;
                while source
                    .get(cursor)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    if cursor.is_multiple_of(512) {
                        check()?;
                    }
                    cursor += 1;
                }
                let token = source.get(begin..cursor).unwrap_or_default();
                if token.eq_ignore_ascii_case(b"Rem")
                    && source
                        .get(start..begin)
                        .unwrap_or_default()
                        .iter()
                        .all(|b| b.is_ascii_whitespace() || b.is_ascii_digit())
                {
                    let end = begin;
                    while source.get(cursor).is_some_and(|b| *b != b'\n') {
                        if cursor.is_multiple_of(512) {
                            check()?;
                        }
                        cursor += 1;
                    }
                    return Ok(Some((
                        trimmed(source, start, end),
                        cursor + usize::from(cursor < source.len()),
                        end,
                    )));
                }
                // An inline control statement must not close an outer block
                // after hiding an unrecognized With opener in its body.
                if reject_inline_with
                    && token.eq_ignore_ascii_case(b"With")
                    && begin != first
                    && source.get(begin.wrapping_sub(1)) != Some(&b'.')
                    && !source
                        .get(first..previous_word_end.unwrap_or(first))
                        .unwrap_or_default()
                        .eq_ignore_ascii_case(b"End")
                    && !source
                        .get(first..begin)
                        .unwrap_or_default()
                        .iter()
                        .all(|b| b.is_ascii_whitespace() || b.is_ascii_digit())
                {
                    return Ok(None);
                }
                previous_word_end = Some(cursor);
            }
            _ => cursor += 1,
        }
    }
    Ok((depth == 0).then(|| (trimmed(source, start, cursor), cursor, cursor)))
}

fn trimmed(source: &[u8], start: usize, end: usize) -> Range<usize> {
    let text = source.get(start..end).unwrap_or_default();
    let left = text.len() - text.trim_ascii_start().len();
    start + left..start + text.trim_ascii_end().len().max(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_boundaries_preserve_continuation_bytes_without_changing_legacy_ranges() {
        for (source, raw, trimmed) in [
            ("  Target _\n\nnext", "  Target _\n", "Target _"),
            ("  Target _\r\n\r\nnext", "  Target _\r\n", "Target _"),
            (
                "  Target _\n\t ' comment\nnext",
                "  Target _\n\t ",
                "Target _",
            ),
            ("  Target\t \r\nnext", "  Target\t ", "Target"),
            ("  Target\t :next", "  Target\t ", "Target"),
            ("  Target _\n", "  Target _\n", "Target _"),
        ] {
            let (range, next, content_end) =
                statement_with_boundary(source.as_bytes(), 0, false, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .unwrap();
            assert_eq!(&source[..content_end], raw);
            assert_eq!(&source[range.clone()], trimmed);
            assert_eq!(
                statement(source.as_bytes(), 0, false, &mut || Ok::<(), ()>(())).unwrap(),
                Some((range, next))
            );
        }
    }

    #[test]
    fn float_trailing_point_hash_suffix_preserves_exact_statement_boundaries() {
        for literal in ["1.#", "5.#"] {
            for delimiter in [":", "\n", "\r\n", ""] {
                let text = format!("  sink={literal} \t");
                let source = format!(
                    "prefix\n{text}{delimiter}{}",
                    if delimiter.is_empty() { "" } else { "next" }
                );
                let (range, next) =
                    statement(source.as_bytes(), 7, false, &mut || Ok::<(), ()>(()))
                        .unwrap()
                        .unwrap();
                assert_eq!(range, 9..9 + "sink=".len() + literal.len());
                assert_eq!(&source[range], format!("sink={literal}"));
                assert_eq!(next, 7 + text.len() + delimiter.len());
                if !delimiter.is_empty() {
                    assert_eq!(
                        statement(source.as_bytes(), next, false, &mut || Ok::<(), ()>(()))
                            .unwrap(),
                        Some((next..source.len(), source.len()))
                    );
                }
            }
        }
        for text in [
            "Const Seed=1.#",
            "Call Target(1.#,5.#)",
            "Dim grid(-1.# To+5.#)",
            "sink= _\r\n 1.# + _\n 5.#",
        ] {
            let source = format!("{text}\nnext");
            assert_eq!(
                statement(source.as_bytes(), 0, false, &mut || Ok::<(), ()>(())).unwrap(),
                Some((0..text.len(), text.len() + 1))
            );
        }
    }

    #[test]
    fn float_hash_boundaries_preserve_dates_quotes_directives_and_comments() {
        for text in [
            "x=#1/2/2000 10:30#",
            "x=\"1.#: With hidden\"",
            "x=[1.#: With hidden]",
            "#If Enabled Then",
        ] {
            let source = format!("{text}:next");
            assert_eq!(
                statement(source.as_bytes(), 0, false, &mut || Ok::<(), ()>(())).unwrap(),
                Some((0..text.len(), text.len() + 1))
            );
        }
        for source in [
            "x=.#unterminated\n",
            "x=value.#unterminated\n",
            "x=1. #unterminated\n",
        ] {
            assert!(
                statement(source.as_bytes(), 0, false, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none()
            );
        }
        let input = "With item\nRem 1.#: With hidden\nx=1.#: y=5.#\nx=\"End With:1.#\": x=[With]\nx=#1/2/2000 10:30#\n#If Enabled Then\nx= _\r\n 1.# + _\n 5.# ' End With: With hidden\n#Else\nx=5.#\n#End If\nEnd With\n";
        let blocks = with_blocks(input.as_bytes(), 1, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].header(), 0.."With item".len());
        assert_eq!(&input[blocks[0].receiver()], "item");
        assert_eq!(
            blocks[0].body(),
            "With item\n".len()..input.rfind("End With").unwrap()
        );
        assert_eq!(&input[blocks[0].terminator()], "End With");
    }

    #[test]
    fn float_hash_statement_scans_propagate_cancellation_at_every_checkpoint() {
        let input = format!(
            "With item\nsink={}.#: sink= _\r\n 5.# ' {}\nEnd With\n",
            "9".repeat(2048),
            "1.#: With hidden ".repeat(128)
        );
        let mut checkpoints = 0;
        assert_eq!(
            with_blocks(input.as_bytes(), 1, &mut || {
                checkpoints += 1;
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap()
            .len(),
            1
        );
        assert!(checkpoints > 8);
        for stop in 1..=checkpoints {
            let mut calls = 0;
            assert_eq!(
                with_blocks(input.as_bytes(), 1, &mut || {
                    calls += 1;
                    if calls == stop { Err("stop") } else { Ok(()) }
                }),
                Err("stop")
            );
            assert_eq!(calls, stop);
        }
    }

    #[test]
    fn nested_headers_remain_in_the_outer_body() {
        let input = b"Sub Run()\nWith factory.Build( _\r\n value:=1) ' With ignored\nWith .Child\nx = .Value\nEnd With\nEnd With\nEnd Sub\n";
        let blocks = with_blocks(input, 2, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            &input[blocks[0].receiver()],
            b"factory.Build( _\r\n value:=1)"
        );
        assert_eq!(&input[blocks[1].receiver()], b".Child");
        assert!(blocks[0].body().contains(&blocks[1].receiver().start));
        assert!(!blocks[1].body().contains(&blocks[1].receiver().start));
        assert_eq!(&input[blocks[1].terminator()], b"End With");
    }

    #[test]
    fn comments_dates_strings_labels_and_branches_do_not_change_nesting() {
        let input = b"10 With item\nRem End With: With fake\nx = \"End With: With\": x = #1/2/2000 10:30#\n#If Enabled Then\nlabel: With .Child\nx = [With]\nEnd With\n#Else\nx = .Value\n#End If\n20 End With\n";
        assert_eq!(
            with_blocks(input, 3, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn incomplete_and_cross_branch_blocks_never_expose_an_outer_receiver() {
        for input in [
            "With a\nx=.Value",
            "End With",
            "With a\nEnd Sub\nEnd With",
            "With a\nIf flag Then With b\nEnd With\nEnd With",
            "With a\n#If FLAG Then\nEnd With\nWith b\n#End If\nEnd With",
            "#If FLAG Then\nWith a\n#Else\nEnd With\n#End If",
            "With a\nx=\"End With\nEnd With",
        ] {
            assert!(
                with_blocks(input.as_bytes(), 20, &mut || Ok::<(), ()>(()))
                    .unwrap()
                    .is_none(),
                "{input}"
            );
        }
        assert!(
            with_blocks(b"With a\nEnd With", 0, &mut || Ok::<(), ()>(()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn arbitrary_bytes_and_long_comments_are_cancellable() {
        let bytes: Vec<u8> = (0..=255).collect();
        let _ = with_blocks(&bytes, 4, &mut || Ok::<(), ()>(()));
        let comment = format!("'{}", "x".repeat(8192));
        let mut calls = 0;
        assert_eq!(
            with_blocks(comment.as_bytes(), 4, &mut || {
                calls += 1;
                if calls > 3 { Err("stop") } else { Ok(()) }
            }),
            Err("stop")
        );
    }
}
