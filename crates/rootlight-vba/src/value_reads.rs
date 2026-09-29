//! Proves value-consuming statement positions from original module bytes.
//! Argument passing remains a separate semantic obligation: an argument may
//! denote ByRef storage rather than retrieving an automatic variable's value.

use std::ops::Range;

use crate::call_chain::ExpressionBoundary;
use crate::{CallChainRead, CallChainRole, logical_whitespace_len, name, trivia};

/// A value read and its evaluation witness, relative to unchanged module input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueRead {
    value: Range<usize>,
    statement: Range<usize>,
}

/// An assignment value read returned by [`assignment_value_reads`].
pub type AssignmentRead = ValueRead;

impl ValueRead {
    /// Returns the exact lexical name or complete member expression being read.
    #[must_use]
    pub fn value(&self) -> Range<usize> {
        self.value.clone()
    }

    /// Returns the assignment or control header proving evaluation.
    ///
    /// Trailing comments and the inline body after an If header are excluded.
    #[must_use]
    pub fn statement(&self) -> Range<usize> {
        self.statement.clone()
    }
}

/// Inventories reads in supported scalar assignments without a parser runtime.
///
/// Accepts optional Set/Let prefixes, simple or dotted destinations, and the
/// expression subset of [`crate::value_expression_end`]. Application arguments
/// and callees are excluded: their value/storage semantics need binding evidence.
/// Strings, comments and explicit continuations retain source boundaries.
/// Unsupported statements are skipped, not claimed analyzed. Malformed lexical
/// boundaries or exhausted read/token limits return None without partial output.
/// Scanning is nonrecursive; token and output allocations use the caller's limits.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn assignment_value_reads<E>(
    source: &[u8],
    maximum_tokens: usize,
    maximum_reads: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<AssignmentRead>>, E> {
    inventory(
        source,
        maximum_tokens,
        maximum_reads,
        InventoryKind::Assignments,
        check,
    )
}

/// Inventories assignment RHS reads and If/ElseIf, While and Do/Loop conditions.
///
/// Uses the expression subset and limits of [`assignment_value_reads`], scanning
/// each statement once. If conditions require an unquoted top-level Then, which
/// can precede an inline body. Do/Loop conditions require While or Until.
/// Only the header's expression is analyzed; this does not prove block nesting,
/// reachability, iteration counts or runtime state. Application arguments and
/// callees remain excluded until their passing semantics are independently bound.
/// Unsupported statements are skipped. Malformed lexical boundaries or exhausted
/// token/read limits return None atomically; scanning is nonrecursive and bounded.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn statement_value_reads<E>(
    source: &[u8],
    maximum_tokens: usize,
    maximum_reads: usize,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<ValueRead>>, E> {
    inventory(
        source,
        maximum_tokens,
        maximum_reads,
        InventoryKind::Statements,
        check,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InventoryKind {
    Assignments,
    Statements,
}

fn inventory<E>(
    source: &[u8],
    maximum_tokens: usize,
    maximum_reads: usize,
    kind: InventoryKind,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<Vec<ValueRead>>, E> {
    check()?;
    let mut cursor = 0;
    let mut reads = Vec::new();
    while cursor < source.len() {
        check()?;
        let Some((mut statement, next)) =
            crate::with_blocks::statement(source, cursor, false, check)?
        else {
            return Ok(None);
        };
        cursor = next;
        let text = source.get(statement.clone()).unwrap_or_default();
        let context = if kind == InventoryKind::Statements {
            condition_start(text, check)?
        } else {
            None
        };
        let context = match context {
            Some(context) => Some(context),
            None => value_start(text, check)?.map(|offset| (offset, ExpressionBoundary::Statement)),
        };
        let Some((offset, boundary)) = context else {
            continue;
        };
        let rhs = text.get(offset..).unwrap_or_default();
        let value = match crate::call_chain::bounded_value_expression(
            rhs,
            maximum_tokens,
            false,
            boundary,
            check,
        )? {
            CallChainRead::Parsed(value) => value,
            CallChainRead::Unsupported => continue,
            CallChainRead::TokenLimit => return Ok(None),
        };
        let tail = rhs.get(value.end()..).unwrap_or_default();
        if boundary == ExpressionBoundary::Then {
            let mut end = logical_whitespace_len(tail, check)?;
            let keyword_start = end;
            if !name(tail, &mut end, check)?.is_some_and(|word| word.eq_ignore_ascii_case(b"Then"))
                || tail.get(keyword_start) == Some(&b'[')
            {
                continue;
            }
            statement.end = statement.start + offset + value.end() + end;
        } else if logical_whitespace_len(tail, check)? != tail.len() {
            continue;
        }
        let mut applications: Vec<_> = value
            .parts()
            .iter()
            .filter(|part| part.role() == CallChainRole::Application)
            .map(|part| part.span())
            .collect();
        applications.sort_unstable_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
        let mut parts: Vec<_> = value.parts().iter().collect();
        parts.sort_unstable_by_key(|part| (part.span().start, part.span().end));
        let mut applications = applications.into_iter().peekable();
        let mut application_end = 0;
        for part in parts {
            check()?;
            let span = part.span();
            while applications
                .peek()
                .is_some_and(|app| app.start <= span.start)
            {
                check()?;
                if let Some(app) = applications.next() {
                    application_end = application_end.max(app.end);
                }
            }
            if span.end <= application_end
                || !matches!(
                    part.role(),
                    CallChainRole::ValueIdentifier
                        | CallChainRole::Member
                        | CallChainRole::SelfMember
                )
            {
                continue;
            }
            if reads.len() >= maximum_reads {
                return Ok(None);
            }
            // Both offsets originate within this statement; summing cannot
            // exceed the already validated module slice's end.
            reads.push(ValueRead {
                value: statement.start + offset + span.start..statement.start + offset + span.end,
                statement: statement.clone(),
            });
        }
    }
    Ok(Some(reads))
}

fn condition_start<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(usize, ExpressionBoundary)>, E> {
    if source.first() == Some(&b'[') {
        return Ok(None);
    }
    let mut cursor = 0;
    let Some(first) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    let boundary = if first.eq_ignore_ascii_case(b"If") || first.eq_ignore_ascii_case(b"ElseIf") {
        ExpressionBoundary::Then
    } else if first.eq_ignore_ascii_case(b"While") {
        ExpressionBoundary::Statement
    } else if first.eq_ignore_ascii_case(b"Do") || first.eq_ignore_ascii_case(b"Loop") {
        trivia(source, &mut cursor, check)?;
        if source.get(cursor) == Some(&b'[')
            || !name(source, &mut cursor, check)?.is_some_and(|word| {
                word.eq_ignore_ascii_case(b"While") || word.eq_ignore_ascii_case(b"Until")
            })
        {
            return Ok(None);
        }
        ExpressionBoundary::Statement
    } else {
        return Ok(None);
    };
    trivia(source, &mut cursor, check)?;
    Ok(Some((cursor, boundary)))
}

fn value_start<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    let mut cursor = 0;
    let Some(first) = name(source, &mut cursor, check)? else {
        return Ok(None);
    };
    if source.first() != Some(&b'[')
        && (first.eq_ignore_ascii_case(b"Set") || first.eq_ignore_ascii_case(b"Let"))
    {
        let end = cursor;
        trivia(source, &mut cursor, check)?;
        if end == cursor || name(source, &mut cursor, check)?.is_none() {
            return Ok(None);
        }
    }
    loop {
        let end = cursor;
        let continued = trivia(source, &mut cursor, check)?;
        match source.get(cursor) {
            Some(b'.') if cursor == end || continued => {
                cursor += 1;
                trivia(source, &mut cursor, check)?;
                if name(source, &mut cursor, check)?.is_none() {
                    return Ok(None);
                }
            }
            Some(b'=') => {
                cursor += 1;
                trivia(source, &mut cursor, check)?;
                return Ok(Some(cursor));
            }
            _ => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> Vec<String> {
        assignment_value_reads(source.as_bytes(), 256, 64, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap()
            .iter()
            .map(|read| source[read.value()].to_owned())
            .collect()
    }

    #[test]
    fn assignments_expose_only_consumed_values_not_destinations_or_arguments() {
        for (source, expected) in [
            ("Set target = item", vec!["item"]),
            ("Let target = item Is Nothing", vec!["item"]),
            ("target = (item)", vec!["item"]),
            (
                "Set holder.Target = holder.Child",
                vec!["holder", "holder.Child"],
            ),
            ("Set target = Invoke(item)", vec![]),
            ("target = item Is Invoke(other)", vec!["item"]),
            (
                "target = Invoke(Inner(item), other) + outside",
                vec!["outside"],
            ),
            ("Set target = .Child", vec![".Child"]),
            (
                "Set target = Invoke(item).Child",
                vec!["Invoke(item).Child"],
            ),
            ("Invoke item", vec![]),
            ("Invoke target = item", vec![]),
            ("Set target(1) = item", vec![]),
            ("Dim item As New Worker", vec![]),
            ("Set target = New Worker", vec![]),
            ("Set target = AddressOf Worker", vec![]),
            ("Set target = item +", vec![]),
        ] {
            assert_eq!(read(source), expected, "{source}");
        }
    }

    #[test]
    fn comments_strings_and_continuations_preserve_exact_ranges() {
        let source = "' Set fake = shadow\nRem Set fake = shadow\ntext = \"Set fake = shadow\"\nSet [target] = _\r\n [item] ' trailing\nSet holder _\n .Target = holder _\n .Child: flag = item Is Nothing\n";
        assert_eq!(
            read(source),
            ["[item]", "holder", "holder _\n .Child", "item"]
        );
        for read in assignment_value_reads(source.as_bytes(), 256, 64, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap()
        {
            assert!(read.statement().start <= read.value().start);
            assert!(read.statement().end >= read.value().end);
        }
    }

    #[test]
    fn limits_and_cancellation_never_return_partial_evidence() {
        assert_eq!(
            assignment_value_reads(b"a = b: c = d", 64, 1, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(
            assignment_value_reads(b"a = b + c", 1, 64, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(
            assignment_value_reads(b"a = b\nc = (", 64, 64, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(
            assignment_value_reads(b"a = b", 64, 64, &mut || Err("cancelled")),
            Err("cancelled")
        );
        let source = format!("a = ({})", "b + ".repeat(2048));
        let mut steps = 0;
        assert_eq!(
            assignment_value_reads(source.as_bytes(), 10000, 10000, &mut || {
                steps += 1;
                if steps == 5 { Err("cancelled") } else { Ok(()) }
            }),
            Err("cancelled")
        );
    }

    #[test]
    fn arbitrary_bytes_never_escape_source_or_output_bounds() {
        for byte in 0..=255 {
            let source = [b'a', b' ', b'=', b' ', byte, b'\n'];
            if let Some(reads) =
                assignment_value_reads(&source, 16, 4, &mut || Ok::<(), ()>(())).unwrap()
            {
                assert!(reads.len() <= 4);
                for read in reads {
                    assert!(source.get(read.value()).is_some());
                    assert!(source.get(read.statement()).is_some());
                }
            }
        }
    }

    #[test]
    fn control_headers_expose_values_without_consuming_inline_bodies() {
        for (source, expected) in [
            ("If item Is Nothing Then", vec!["item"]),
            ("ElseIf Not (item Is Nothing) Then", vec!["item"]),
            ("If item Is Nothing Then Set other = ignored", vec!["item"]),
            ("If(item Is Nothing)Then Exit Sub", vec!["item"]),
            ("While item Is Nothing", vec!["item"]),
            ("Do While item Is Nothing", vec!["item"]),
            ("Do Until item Is Nothing", vec!["item"]),
            ("Loop While item Is Nothing", vec!["item"]),
            ("Loop Until item Is Nothing", vec!["item"]),
            ("If Invoke(item) Is Nothing Then", vec![]),
            ("If Invoke(item, \"Then\") Is Nothing Then", vec![]),
            ("If Invoke([Then], item) Then", vec![]),
            ("If item Is Invoke(other) Then", vec!["item"]),
            (
                "If holder.Then Is Nothing Then",
                vec!["holder", "holder.Then"],
            ),
            ("If [Then] Is Nothing Then", vec!["[Then]"]),
            ("If text = \"Then\" Then", vec!["text"]),
            ("If stamp = #1/2/2000 10:30# Then", vec!["stamp"]),
            ("If item Is Nothing", vec![]),
            ("If item Is Nothing [Then]", vec![]),
            ("If item Is Nothing ThenLater", vec![]),
            ("If holder.Then Is Nothing", vec![]),
            ("If item + Then", vec![]),
            ("If TypeOf item Is Worker Then", vec![]),
            ("#If item Then", vec![]),
            ("Do item", vec![]),
            ("Loop item", vec![]),
            ("Do [While] item", vec![]),
            ("With item", vec![]),
            ("While item extra", vec![]),
        ] {
            let reads = statement_value_reads(source.as_bytes(), 128, 64, &mut || Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(
                reads
                    .iter()
                    .map(|read| &source[read.value()])
                    .collect::<Vec<_>>(),
                expected,
                "{source}"
            );
            for read in reads {
                assert!(read.statement().start <= read.value().start);
                assert!(read.statement().end >= read.value().end);
                assert!(!source[read.statement()].contains("ignored"));
            }
        }
    }

    #[test]
    fn mixed_statements_preserve_offsets_and_assignment_only_contract() {
        let source = "' If fake Then\nRem While fake\nIf _\r\n item Is Nothing _\r\n Then ' comment\nSet target = other\nElseIf [Then] Is Nothing Then\nEnd If\nDo _\n Until third Is Nothing: Loop\n";
        let reads = statement_value_reads(source.as_bytes(), 128, 64, &mut || Ok::<(), ()>(()))
            .unwrap()
            .unwrap();
        assert_eq!(
            reads
                .iter()
                .map(|read| &source[read.value()])
                .collect::<Vec<_>>(),
            ["item", "other", "[Then]", "third"]
        );
        assert_eq!(
            &source[reads[0].statement()],
            "If _\r\n item Is Nothing _\r\n Then"
        );
        assert_eq!(read(source), ["other"]);
    }

    #[test]
    fn control_reader_limits_and_cancellation_are_atomic() {
        assert_eq!(
            statement_value_reads(b"x = a\nIf b Then", 64, 1, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(
            statement_value_reads(b"If a Is Nothing Then", 1, 64, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        assert_eq!(
            statement_value_reads(b"If (a Then", 64, 64, &mut || Ok::<(), ()>(())),
            Ok(None)
        );
        let source = format!("If {} Then", "(a) + ".repeat(2048));
        let mut checks = 0;
        assert_eq!(
            statement_value_reads(source.as_bytes(), 10000, 10000, &mut || {
                checks += 1;
                if checks == 5 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            }),
            Err("cancelled")
        );
        for byte in 0..=255 {
            let source = [b'I', b'f', b' ', byte, b' ', b'T', b'h', b'e', b'n'];
            for end in 0..=source.len() {
                let source = &source[..end];
                if let Some(reads) =
                    statement_value_reads(source, 16, 4, &mut || Ok::<(), ()>(())).unwrap()
                {
                    assert!(reads.len() <= 4);
                    for read in reads {
                        assert!(source.get(read.value()).is_some());
                        assert!(source.get(read.statement()).is_some());
                    }
                }
            }
        }
    }
}
