//! Proves value-consuming assignment positions from original module bytes.
//! Argument passing remains a separate semantic obligation: an argument may
//! denote ByRef storage rather than retrieving an automatic variable's value.

use std::ops::Range;

use crate::{CallChainRead, CallChainRole, logical_whitespace_len, name, trivia};

/// A value read and its complete containing assignment, relative to module input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentRead {
    value: Range<usize>,
    statement: Range<usize>,
}

impl AssignmentRead {
    /// Returns the exact lexical name or complete member expression being read.
    #[must_use]
    pub fn value(&self) -> Range<usize> {
        self.value.clone()
    }

    /// Returns the assignment proving evaluation, excluding trailing comments.
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
    check()?;
    let mut cursor = 0;
    let mut reads = Vec::new();
    while cursor < source.len() {
        check()?;
        let Some((statement, next)) = crate::with_blocks::statement(source, cursor, false, check)?
        else {
            return Ok(None);
        };
        cursor = next;
        let text = source.get(statement.clone()).unwrap_or_default();
        let Some(offset) = value_start(text, check)? else {
            continue;
        };
        let rhs = text.get(offset..).unwrap_or_default();
        let value = match crate::call_chain::value_expression(rhs, maximum_tokens, false, check)? {
            CallChainRead::Parsed(value) => value,
            CallChainRead::Unsupported => continue,
            CallChainRead::TokenLimit => return Ok(None),
        };
        let tail = rhs.get(value.end()..).unwrap_or_default();
        if logical_whitespace_len(tail, check)? != tail.len() {
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
            reads.push(AssignmentRead {
                value: statement.start + offset + span.start..statement.start + offset + span.end,
                statement: statement.clone(),
            });
        }
    }
    Ok(Some(reads))
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
}
