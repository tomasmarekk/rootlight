//! JSONC trailing commas use whitespace in the private native parser input.
//! Queries and evidence still read the complete original text; replacing one
//! ASCII byte preserves offsets, points, UTF-16 maps, and comment captures.

use std::{borrow::Cow, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Range;

use crate::source_view::NativeSourceView;

use super::{CANCELLATION_BYTE_INTERVAL, provider_failure, require_provider_limit};

pub(crate) const JSONC_PARSE_INPUT_POLICY: &str =
    "rootlight.treesitter.jsonc-trailing-comma-input/1";

pub(super) struct JsoncParseInput<'a> {
    bytes: Cow<'a, [u8]>,
}

impl<'a> JsoncParseInput<'a> {
    pub(super) fn new(
        source: &'a NativeSourceView<'_>,
        ranges: &[Range],
        max_owned_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let original = source.as_bytes();
        let mut bytes = Cow::Borrowed(original);
        let mut mask = |index: usize| {
            if matches!(bytes, Cow::Borrowed(_)) {
                admit_capacity(source.owned_bytes(), original.len(), max_owned_bytes)?;
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(original.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                admit_capacity(source.owned_bytes(), owned.capacity(), max_owned_bytes)?;
                for chunk in original.chunks(CANCELLATION_BYTE_INTERVAL) {
                    cancellation.check()?;
                    owned.extend_from_slice(chunk);
                }
                bytes = Cow::Owned(owned);
            }
            let Cow::Owned(owned) = &mut bytes else {
                return Err(provider_failure("jsonc-projection-owner"));
            };
            *owned
                .get_mut(index)
                .ok_or_else(|| provider_failure("jsonc-projection-span"))? = b' ';
            Ok(ControlFlow::Continue(()))
        };
        for range in ranges {
            cancellation.check()?;
            original
                .get(range.start_byte..range.end_byte)
                .ok_or_else(|| provider_failure("jsonc-projection-range"))?;
        }
        walk_commas(original, ranges, cancellation, &mut mask)?;
        cancellation.check()?;
        Ok(Self { bytes })
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn admit_capacity(existing: usize, added: usize, maximum: usize) -> Result<(), AdapterError> {
    require_provider_limit(
        ResourceKind::ReportedMemoryBytes,
        existing
            .checked_add(added)
            .ok_or(SinkError::AccountingOverflow)?,
        maximum,
    )
}

pub(super) fn needs_projection(
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut found = false;
    walk_commas(source, &[], cancellation, |_| {
        found = true;
        Ok(ControlFlow::Break(()))
    })?;
    Ok(found)
}

pub(super) fn invalid_document_span(
    tree: &tree_sitter::Tree,
    cancellation: &Cancellation,
) -> Result<Option<(usize, usize)>, AdapterError> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut values = 0usize;
    for child in root.named_children(&mut cursor) {
        cancellation.check()?;
        if child.kind() != "comment" {
            values += 1;
            if values > 1 {
                break;
            }
        }
    }
    cancellation.check()?;
    // The shared JSON grammar accepts a stream of values. JSONC documents
    // require one value; this contract leaves the existing JSON route intact.
    Ok((values != 1).then(|| (root.start_byte(), root.end_byte())))
}

fn walk_commas(
    source: &[u8],
    ranges: &[Range],
    cancellation: &Cancellation,
    mut visit: impl FnMut(usize) -> Result<ControlFlow<()>, AdapterError>,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    // Native included ranges form one logical lexer input. Keep string,
    // comment and pending-comma state across deindented fragment boundaries.
    let mut input = SelectedBytes::new(source, ranges).peekable();
    let mut next_check = 0usize;
    let mut value_end = false;
    let mut comma = None;
    while let Some((index, byte)) = input.next() {
        check_scan(index, &mut next_check, cancellation)?;
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'/' && input.peek().is_some_and(|(_, byte)| *byte == b'/') {
            input.next();
            while input
                .peek()
                .is_some_and(|(_, byte)| !matches!(byte, b'\r' | b'\n'))
            {
                let Some((index, _)) = input.next() else {
                    break;
                };
                check_scan(index, &mut next_check, cancellation)?;
            }
            continue;
        }
        if byte == b'/' && input.peek().is_some_and(|(_, byte)| *byte == b'*') {
            input.next();
            while let Some((index, byte)) = input.next() {
                check_scan(index, &mut next_check, cancellation)?;
                if byte == b'*' && input.peek().is_some_and(|(_, byte)| *byte == b'/') {
                    input.next();
                    break;
                }
            }
            continue;
        }
        if byte == b'"' {
            comma = None;
            value_end = false;
            while let Some((index, byte)) = input.next() {
                check_scan(index, &mut next_check, cancellation)?;
                if byte == b'\\' {
                    // An escape consumes its next byte even when malformed;
                    // the unchanged native string rule diagnoses its syntax.
                    input.next();
                } else if byte == b'"' {
                    value_end = true;
                    break;
                }
            }
            continue;
        }
        if byte == b',' {
            comma = value_end.then_some(index);
            value_end = false;
        } else {
            if matches!(byte, b']' | b'}')
                && let Some(comma) = comma
                && visit(comma)?.is_break()
            {
                cancellation.check()?;
                return Ok(());
            }
            comma = None;
            // The native grammar still validates keys, values, separators and
            // matching delimiters. Empty or repeated commas are never masked.
            value_end = !matches!(byte, b'[' | b'{' | b':');
        }
    }
    cancellation.check()?;
    Ok(())
}

struct SelectedBytes<'a> {
    source: &'a [u8],
    ranges: &'a [Range],
    range: usize,
    position: usize,
}

impl<'a> SelectedBytes<'a> {
    fn new(source: &'a [u8], ranges: &'a [Range]) -> Self {
        Self {
            source,
            ranges,
            range: 0,
            position: ranges.first().map_or(0, |range| range.start_byte),
        }
    }
}

impl Iterator for SelectedBytes<'_> {
    type Item = (usize, u8);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let end = if self.ranges.is_empty() {
                self.source.len()
            } else {
                self.ranges.get(self.range)?.end_byte
            };
            if self.position >= end {
                if self.ranges.is_empty() {
                    return None;
                }
                self.range = self.range.checked_add(1)?;
                self.position = self.ranges.get(self.range)?.start_byte;
                continue;
            }
            let position = self.position;
            self.position = position.checked_add(1)?;
            return self
                .source
                .get(position)
                .copied()
                .map(|byte| (position, byte));
        }
    }
}

fn check_scan(
    index: usize,
    next_check: &mut usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    if index >= *next_check {
        cancellation.check()?;
        *next_check = index.saturating_add(CANCELLATION_BYTE_INTERVAL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(source: &str) -> Vec<u8> {
        let cancellation = Cancellation::new();
        let view = NativeSourceView::new(
            source.as_bytes(),
            "utf-8",
            usize::MAX,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        JsoncParseInput::new(&view, &[], usize::MAX, &cancellation)
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn replaces_only_trailing_commas_outside_strings_and_comments() {
        for (source, expected) in [
            ("[1,]", "[1 ]"),
            ("{\"a\":[null,],}", "{\"a\":[null ] }"),
            ("[1,/* ,]} */\r\n]", "[1 /* ,]} */\r\n]"),
            ("[1,// ,]}\n]", "[1 // ,]}\n]"),
            (r#"["\",]", "plain,}",]"#, r#"["\",]", "plain,}" ]"#),
            ("[,] [1,,] {,} {\"a\":,}", "[,] [1,,] {,} {\"a\":,}"),
            ("[1,/* unterminated ]", "[1,/* unterminated ]"),
            ("[\"unterminated,]", "[\"unterminated,]"),
        ] {
            assert_eq!(projected(source), expected.as_bytes(), "{source}");
        }
    }

    #[test]
    fn odd_escape_offsets_reach_the_next_in_flight_cancellation_checkpoint() {
        let cancellation = Cancellation::new();
        let mut next_check = 0;
        check_scan(1, &mut next_check, &cancellation).unwrap();
        assert_eq!(next_check, 1 + CANCELLATION_BYTE_INTERVAL);
        for offset in (3..next_check).step_by(2) {
            check_scan(offset, &mut next_check, &cancellation).unwrap();
        }
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            check_scan(
                1 + CANCELLATION_BYTE_INTERVAL,
                &mut next_check,
                &cancellation
            )
            .is_err()
        );
    }

    #[test]
    fn selected_ranges_do_not_interpret_quoted_host_prefixes_or_mask_host_commas() {
        let source = "opaque \"\n[1,]\nsuffix,";
        let cancellation = Cancellation::new();
        let view = NativeSourceView::new(
            source.as_bytes(),
            "utf-8",
            usize::MAX,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        let start = source.find("[1,]").unwrap();
        let range = Range {
            start_byte: start,
            end_byte: start + 4,
            start_point: tree_sitter::Point::new(1, 0),
            end_point: tree_sitter::Point::new(1, 4),
        };
        let projected = JsoncParseInput::new(&view, &[range], usize::MAX, &cancellation).unwrap();
        assert_eq!(projected.as_bytes(), b"opaque \"\n[1 ]\nsuffix,");
        assert_eq!(view.as_bytes(), source.as_bytes());
    }

    #[test]
    fn disjoint_ranges_preserve_pending_commas_and_string_and_comment_state() {
        for (source, selected, expected) in [
            (
                "opaque \"\n[1,\nHOST,\n/* first\nHOST,\nsecond */\nHOST,\n]\nsuffix,",
                vec!["[1,\n", "/* first\n", "second */\n", "]\n"],
                "opaque \"\n[1 \nHOST,\n/* first\nHOST,\nsecond */\nHOST,\n]\nsuffix,",
            ),
            (
                "opaque \"\n[\"value,\nHOST\n]\",]\nsuffix,",
                vec!["[\"value,", "]\",]"],
                "opaque \"\n[\"value,\nHOST\n]\" ]\nsuffix,",
            ),
        ] {
            let cancellation = Cancellation::new();
            let view = NativeSourceView::new(
                source.as_bytes(),
                "utf-8",
                usize::MAX,
                usize::MAX,
                &cancellation,
            )
            .unwrap();
            let ranges: Vec<_> = selected
                .iter()
                .map(|selected| {
                    let start = source.find(selected).unwrap();
                    Range {
                        start_byte: start,
                        end_byte: start + selected.len(),
                        start_point: tree_sitter::Point::new(0, 0),
                        end_point: tree_sitter::Point::new(0, 0),
                    }
                })
                .collect();
            let projected =
                JsoncParseInput::new(&view, &ranges, usize::MAX, &cancellation).unwrap();
            assert_eq!(projected.as_bytes(), expected.as_bytes());
            assert_eq!(view.as_bytes(), source.as_bytes());
        }
    }

    #[test]
    fn projection_obeys_owned_memory_and_cancellation() {
        let cancellation = Cancellation::new();
        let view = NativeSourceView::new(b"[1,]", "utf-8", 4, 4, &cancellation).unwrap();
        assert!(JsoncParseInput::new(&view, &[], 3, &cancellation).is_err());
        assert_eq!(
            JsoncParseInput::new(&view, &[], 4, &cancellation)
                .unwrap()
                .as_bytes(),
            b"[1 ]"
        );
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(JsoncParseInput::new(&view, &[], 4, &cancellation).is_err());
    }
}
