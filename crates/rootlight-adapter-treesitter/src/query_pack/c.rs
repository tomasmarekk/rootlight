//! Retain authored C-family macro headers and replacement-list lexical evidence.
//!
//! The reviewed C and CPP grammars keep replacements in opaque preproc_arg nodes.
//! This lexical scan never expands macros or assigns evaluated bindings.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

/// Selects the separately reviewed literal rules of the two native consumers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreprocessorDialect {
    C,
    Cpp,
}

impl PreprocessorDialect {
    fn syntax(self, c: &'static str, cpp: &'static str) -> &'static str {
        match self {
            Self::C => c,
            Self::Cpp => cpp,
        }
    }
}

/// Selects optional lexical evidence or mandatory lexical-completeness gaps.
#[derive(Clone, Copy)]
pub(super) enum ReplacementMode {
    #[cfg(test)]
    Literals,
    Lexical,
    Gaps,
}

impl ReplacementMode {
    fn retains_strings(self) -> bool {
        match self {
            Self::Lexical => true,
            Self::Gaps => false,
            #[cfg(test)]
            Self::Literals => true,
        }
    }
}

/// Reports captures already retained by the caller and explicit capacity exhaustion.
pub(super) struct ReplacementCaptures {
    pub(super) emitted: usize,
    pub(super) exhausted: bool,
}

/// A written macro header proved independently of recovered native fields.
pub(crate) struct MacroHeader {
    pub(crate) start: usize,
    pub(crate) name: Range<usize>,
    pub(crate) replacement_start: usize,
    pub(crate) end: usize,
    pub(crate) function_like: bool,
}

/// Proves the original header and its complete admitted replacement extent.
///
/// A native recovery name such as `tail` is insufficient: the written first
/// identifier must exist as an exact clean native identifier, possibly in ERROR.
/// Malformed/unproved headers retain their original native error handling.
///
/// # Errors
/// Propagates cancellation and checked source-coordinate failures.
pub(crate) fn macro_header(
    node: Node<'_>,
    source: &[u8],
    ranges: &[tree_sitter::Range],
    cancellation: &Cancellation,
) -> Result<Option<MacroHeader>, AdapterError> {
    let Some(limit) = admitted_macro_limit(
        node.start_byte(),
        node.end_byte(),
        source.len(),
        ranges,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    macro_header_at(
        node,
        source,
        node.start_byte(),
        limit,
        PreprocessorDialect::C,
        cancellation,
    )
}

fn macro_header_at(
    node: Node<'_>,
    source: &[u8],
    start: usize,
    limit: usize,
    dialect: PreprocessorDialect,
    cancellation: &Cancellation,
) -> Result<Option<MacroHeader>, AdapterError> {
    let bytes = source
        .get(start..limit)
        .ok_or_else(|| query_failure("query-c-preprocessor-range"))?;
    let mut scan = Scanner {
        bytes,
        cursor: 0,
        cancellation,
    };
    if scan.next()?.map(|(_, byte)| byte) != Some(b'#') {
        return Ok(None);
    }
    header_space(&mut scan)?;
    for expected in b"define" {
        if scan.next()?.map(|(_, byte)| byte) != Some(*expected) {
            return Ok(None);
        }
    }
    if !scan
        .peek()?
        .is_some_and(|(_, byte)| horizontal_space(byte) || byte == b'/')
    {
        return Ok(None);
    }
    if !parameter_space(&mut scan)? {
        return Ok(None);
    }
    let Some((name_start, first)) = scan.next()? else {
        return Ok(None);
    };
    if !(first.is_ascii_alphabetic() || first == b'_' || first >= 0x80) {
        return Ok(None);
    }
    while scan.peek()?.is_some_and(|(_, byte)| identifier_byte(byte)) {
        scan.next()?;
    }
    let name = (start + name_start)..(start + scan.cursor);
    let Some(identifier) = node.named_descendant_for_byte_range(name.start, name.end) else {
        return Ok(None);
    };
    if identifier.kind() != "identifier"
        || identifier.byte_range() != name
        || identifier.is_error()
        || identifier.is_missing()
        || identifier.has_error()
    {
        return Ok(None);
    }
    let function_like = scan.peek()?.is_some_and(|(_, byte)| byte == b'(');
    if function_like && !header_parameters(&mut scan)? {
        return Ok(None);
    }
    let replacement_start = start
        .checked_add(scan.cursor)
        .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
    let end = directive_end_for(source, replacement_start, limit, dialect, cancellation)?;
    Ok(Some(MacroHeader {
        start,
        name,
        replacement_start,
        end,
        function_like,
    }))
}

fn horizontal_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | 0x0b | 0x0c)
}

fn header_space(scan: &mut Scanner<'_>) -> Result<(), AdapterError> {
    while scan.peek()?.is_some_and(|(_, byte)| horizontal_space(byte)) {
        scan.next()?;
    }
    Ok(())
}

fn header_parameters(scan: &mut Scanner<'_>) -> Result<bool, AdapterError> {
    scan.next()?;
    let mut expecting = true;
    let mut any = false;
    let mut variadic = false;
    loop {
        if !parameter_space(scan)? {
            return Ok(false);
        }
        let Some((_, byte)) = scan.next()? else {
            return Ok(false);
        };
        if byte == b')' {
            return Ok(!expecting || !any);
        }
        if !expecting && byte == b',' && !variadic {
            expecting = true;
            continue;
        }
        if byte == b'.' && (expecting || !variadic) {
            if scan.next()?.map(|(_, byte)| byte) != Some(b'.')
                || scan.next()?.map(|(_, byte)| byte) != Some(b'.')
            {
                return Ok(false);
            }
            expecting = false;
            any = true;
            variadic = true;
            continue;
        }
        if !expecting || !(byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80) {
            return Ok(false);
        }
        while scan.peek()?.is_some_and(|(_, byte)| identifier_byte(byte)) {
            scan.next()?;
        }
        expecting = false;
        any = true;
    }
}

fn parameter_space(scan: &mut Scanner<'_>) -> Result<bool, AdapterError> {
    loop {
        header_space(scan)?;
        if !scan.peek()?.is_some_and(|(_, byte)| byte == b'/') {
            return Ok(true);
        }
        scan.next()?;
        if scan.next()?.map(|(_, byte)| byte) != Some(b'*') {
            return Ok(false);
        }
        let mut complete = false;
        while let Some((_, byte)) = scan.next()? {
            if byte == b'*' && scan.peek()?.is_some_and(|(_, byte)| byte == b'/') {
                scan.next()?;
                complete = true;
                break;
            }
        }
        if !complete {
            return Ok(false);
        }
    }
}

/// Returns exact language-specific macro labels without changing written names.
pub(super) fn macro_syntax(
    node: Node<'_>,
    role: StructuralRole,
    dialect: PreprocessorDialect,
) -> Option<&'static str> {
    match (node.kind(), role) {
        ("preproc_def", StructuralRole::Declaration) => {
            Some(dialect.syntax("c.preprocessor_constant", "cpp.preprocessor_constant"))
        }
        ("preproc_function_def", StructuralRole::Declaration) => {
            Some(dialect.syntax("c.function_macro", "cpp.function_macro"))
        }
        ("preproc_def" | "preproc_function_def", StructuralRole::Signature) => {
            Some(dialect.syntax("c.written_macro", "cpp.written_macro"))
        }
        _ => None,
    }
}

/// Scans only a native macro's authored replacement list on original view bytes.
///
/// Lexical mode emits original strings and comments; gap mode emits required
/// markers for unfinished quotes/comments. Identity extraction calls gap mode from the
/// mandatory macro signature capture, so preflight and normal replay agree.
/// `maximum` is the caller's remaining capture capacity, not a payload-size cap.
/// Candidates stream directly into `retain`; no intermediate capture list is built.
///
/// # Errors
///
/// Propagates cancellation, sink failures and checked-coordinate failures.
#[allow(clippy::too_many_arguments)]
pub(super) fn replacement_candidates(
    node: Node<'_>,
    source: &[u8],
    end: usize,
    mode: ReplacementMode,
    maximum: usize,
    dialect: PreprocessorDialect,
    cancellation: &Cancellation,
    retain: &mut dyn FnMut(QueryCandidate) -> Result<(), AdapterError>,
) -> Result<ReplacementCaptures, AdapterError> {
    cancellation.check()?;
    let Some(name) = node.child_by_field_name("name") else {
        return Ok(ReplacementCaptures {
            emitted: 0,
            exhausted: false,
        });
    };
    let start = node
        .child_by_field_name("parameters")
        .map_or(name.end_byte(), |parameters| parameters.end_byte());
    let depth = native_depth(node, cancellation)?
        .checked_add(1)
        .ok_or_else(|| query_failure("query-c-preprocessor-depth"))?;
    scan_replacement_for(
        source,
        start..end,
        depth,
        mode,
        maximum,
        dialect,
        cancellation,
        retain,
    )
}

/// Returns the physical end of the directive owning this native macro name.
///
/// Recovery can end `preproc_arg` before a block comment. A lexical extent keeps
/// its later replacement tokens under the same written owner and header.
/// A native node crossing included ranges has no single admitted owner extent.
/// Default sentinel range ends are clamped to the actual source bytes.
///
/// # Errors
/// Propagates cancellation and invalid native/source coordinates.
pub(super) fn macro_end(
    node: Node<'_>,
    source: &[u8],
    ranges: &[tree_sitter::Range],
    dialect: PreprocessorDialect,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    cancellation.check()?;
    let Some(limit) = admitted_macro_limit(
        node.start_byte(),
        node.end_byte(),
        source.len(),
        ranges,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    let Some(name) = node.child_by_field_name("name") else {
        return Ok(Some(node.end_byte()));
    };
    let start = node
        .child_by_field_name("parameters")
        .map_or(name.end_byte(), |parameters| parameters.end_byte());
    if name.start_byte() < node.start_byte() || start > node.end_byte() {
        return Err(query_failure("query-c-preprocessor-owner"));
    }
    directive_end_for(source, start, limit, dialect, cancellation).map(Some)
}

fn admitted_macro_limit(
    start: usize,
    end: usize,
    source_length: usize,
    ranges: &[tree_sitter::Range],
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    for range in ranges {
        cancellation.check()?;
        let limit = range.end_byte.min(source_length);
        if range.start_byte <= start && end <= limit {
            return Ok(Some(limit));
        }
    }
    Ok(None)
}

fn directive_end_for(
    source: &[u8],
    start: usize,
    end: usize,
    dialect: PreprocessorDialect,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let bytes = source
        .get(start..end)
        .ok_or_else(|| query_failure("query-c-preprocessor-range"))?;
    let mut scan = Scanner {
        bytes,
        cursor: 0,
        cancellation,
    };
    while let Some((_, byte)) = scan.next()? {
        if byte == b'\n' {
            break;
        }
        if byte == b'/'
            && scan
                .peek()?
                .is_some_and(|(_, next)| matches!(next, b'/' | b'*'))
        {
            scan.consume_comment()?;
            if scan.cursor > 0 && bytes.get(scan.cursor - 1) == Some(&b'\n') {
                break;
            }
            continue;
        }
        if byte.is_ascii_digit()
            || (byte == b'.' && scan.peek()?.is_some_and(|(_, next)| next.is_ascii_digit()))
        {
            scan.consume_number(byte)?;
            continue;
        }
        if let Some((quote, raw)) = scan.literal_prefix(byte, dialect)? {
            scan.consume_literal(quote, raw)?;
            if scan.cursor > 0 && bytes.get(scan.cursor - 1) == Some(&b'\n') {
                break;
            }
        }
    }
    start
        .checked_add(scan.cursor)
        .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))
}

// Keep existing C extent controls on their original dialect.
#[cfg(test)]
fn directive_end(
    source: &[u8],
    start: usize,
    end: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    directive_end_for(source, start, end, PreprocessorDialect::C, cancellation)
}

#[cfg(test)]
fn scan_replacement(
    source: &[u8],
    range: Range<usize>,
    depth: usize,
    mode: ReplacementMode,
    maximum: usize,
    cancellation: &Cancellation,
    retain: &mut dyn FnMut(QueryCandidate) -> Result<(), AdapterError>,
) -> Result<ReplacementCaptures, AdapterError> {
    scan_replacement_for(
        source,
        range,
        depth,
        mode,
        maximum,
        PreprocessorDialect::C,
        cancellation,
        retain,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_replacement_for(
    source: &[u8],
    range: Range<usize>,
    depth: usize,
    mode: ReplacementMode,
    maximum: usize,
    dialect: PreprocessorDialect,
    cancellation: &Cancellation,
    retain: &mut dyn FnMut(QueryCandidate) -> Result<(), AdapterError>,
) -> Result<ReplacementCaptures, AdapterError> {
    let bytes = source
        .get(range.clone())
        .ok_or_else(|| query_failure("query-c-preprocessor-range"))?;
    let mut scan = Scanner {
        bytes,
        cursor: 0,
        cancellation,
    };
    let mut output = Emitter {
        capture: ReplacementCaptures {
            emitted: 0,
            exhausted: false,
        },
        maximum,
        retain,
    };
    while let Some((start, byte)) = scan.next()? {
        cancellation.check()?;
        if byte == b'/'
            && scan
                .peek()?
                .is_some_and(|(_, next)| matches!(next, b'/' | b'*'))
        {
            let complete = scan.consume_comment()?;
            let end = if bytes.get(scan.cursor.saturating_sub(1)) == Some(&b'\n') {
                scan.cursor - 1
            } else {
                scan.cursor
            };
            append_comment(
                &mut output,
                mode,
                start..end,
                range.start,
                depth,
                bytes,
                dialect,
            )?;
            if !complete {
                append_gap(
                    &mut output,
                    mode,
                    start..bytes.len(),
                    range.start,
                    depth,
                    dialect,
                )?;
                break;
            }
            if output.capture.exhausted {
                break;
            }
            continue;
        }
        if byte.is_ascii_digit()
            || (byte == b'.' && scan.peek()?.is_some_and(|(_, next)| next.is_ascii_digit()))
        {
            scan.consume_number(byte)?;
            continue;
        }
        let Some((quote, raw)) = scan.literal_prefix(byte, dialect)? else {
            continue;
        };
        if !scan.consume_literal(quote, raw)? {
            append_gap(
                &mut output,
                mode,
                start..bytes.len(),
                range.start,
                depth,
                dialect,
            )?;
            break;
        }
        if quote == b'"' && mode.retains_strings() {
            let literal_start = range
                .start
                .checked_add(start)
                .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
            let end = range
                .start
                .checked_add(scan.cursor)
                .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
            let syntax = if raw {
                "cpp.preprocessor_raw_string"
            } else {
                dialect.syntax("c.preprocessor_string", "cpp.preprocessor_string")
            };
            append(
                &mut output,
                literal_start..end,
                depth,
                StructuralRole::StringLiteral,
                syntax,
            )?;
            if output.capture.exhausted {
                break;
            }
        }
    }
    cancellation.check()?;
    Ok(output.capture)
}

/// Visits complete source-written CPP headers under the admitted native ranges.
///
/// CPP recovery can discard the macro owner itself. Whole-context lexical proof
/// excludes strings/comments, and every name still needs an exact native clean
/// identifier. A failed proof withholds the affected CPP partition rather than
/// creating a declaration from a physical substring.
///
/// # Errors
/// Propagates cancellation, checked coordinates and the caller's admission error.
pub(crate) fn cpp_macro_headers<'tree>(
    root: Node<'tree>,
    source: &[u8],
    ranges: &[tree_sitter::Range],
    cancellation: &Cancellation,
    retain: &mut dyn FnMut(MacroHeader, Option<Node<'tree>>) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    for range in ranges {
        cancellation.check()?;
        let start = range.start_byte;
        let end = range.end_byte.min(source.len());
        let Some(bytes) = source.get(start..end) else {
            return Ok(false);
        };
        let mut scan = Scanner {
            bytes,
            // A decoded BOM remains in NativeSourceView. Only its file-start
            // position is lexical trivia; range-local or interior bytes are not.
            cursor: if start == 0 && bytes.starts_with(b"\xef\xbb\xbf") {
                3
            } else {
                0
            },
            cancellation,
        };
        let mut leading = true;
        while let Some((position, byte)) = scan.next()? {
            if byte == b'\n' {
                leading = true;
                continue;
            }
            if horizontal_space(byte) {
                continue;
            }
            if byte == b'/'
                && scan
                    .peek()?
                    .is_some_and(|(_, next)| matches!(next, b'/' | b'*'))
            {
                if !scan.consume_comment()? {
                    return Ok(false);
                }
                if bytes.get(scan.cursor.saturating_sub(1)) == Some(&b'\n') {
                    leading = true;
                }
                continue;
            }
            if leading && byte == b'#' {
                let mut probe = Scanner {
                    bytes,
                    cursor: scan.cursor,
                    cancellation,
                };
                if !parameter_space(&mut probe)? {
                    return Ok(false);
                }
                let mut is_define = true;
                for expected in b"define" {
                    is_define &= probe.next()?.map(|(_, next)| next) == Some(*expected);
                }
                if is_define && probe.peek()?.is_some_and(|(_, next)| identifier_byte(next)) {
                    return Ok(false);
                }
                if is_define {
                    let Some(header) = macro_header_at(
                        root,
                        source,
                        start + position,
                        end,
                        PreprocessorDialect::Cpp,
                        cancellation,
                    )?
                    else {
                        return Ok(false);
                    };
                    let owner = root
                        .named_descendant_for_byte_range(header.name.start, header.name.end)
                        .and_then(|name| name.parent())
                        .filter(|node| {
                            matches!(node.kind(), "preproc_def" | "preproc_function_def")
                                && node.start_byte() == header.start
                        });
                    scan.cursor = header
                        .end
                        .checked_sub(start)
                        .ok_or_else(|| query_failure("query-cpp-preprocessor-coordinate"))?;
                    retain(header, owner)?;
                    leading = true;
                    continue;
                }
            }
            leading = false;
            if byte.is_ascii_digit()
                || (byte == b'.' && scan.peek()?.is_some_and(|(_, next)| next.is_ascii_digit()))
            {
                scan.consume_number(byte)?;
                continue;
            }
            if let Some((quote, raw)) = scan.literal_prefix(byte, PreprocessorDialect::Cpp)?
                && !scan.consume_literal(quote, raw)?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn append_comment(
    output: &mut Emitter<'_>,
    mode: ReplacementMode,
    local: Range<usize>,
    offset: usize,
    depth: usize,
    source: &[u8],
    dialect: PreprocessorDialect,
) -> Result<(), AdapterError> {
    if !matches!(mode, ReplacementMode::Lexical) {
        return Ok(());
    }
    let start = offset
        .checked_add(local.start)
        .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
    let end = offset
        .checked_add(local.end)
        .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
    append(
        output,
        start..end,
        depth,
        StructuralRole::Comment,
        dialect.syntax("c.comment", "cpp.comment"),
    )?;
    if source.get(local.start..local.start.saturating_add(3)) == Some(b"/**".as_slice()) {
        append(
            output,
            start..end,
            depth,
            StructuralRole::Documentation,
            dialect.syntax("c.comment", "cpp.comment"),
        )?;
    }
    Ok(())
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn append_gap(
    output: &mut Emitter<'_>,
    mode: ReplacementMode,
    local: Range<usize>,
    offset: usize,
    depth: usize,
    dialect: PreprocessorDialect,
) -> Result<(), AdapterError> {
    if matches!(mode, ReplacementMode::Gaps) {
        let start = local
            .start
            .checked_add(offset)
            .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
        let end = local
            .end
            .checked_add(offset)
            .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
        append(
            output,
            start..end,
            depth,
            StructuralRole::Signature,
            dialect.syntax(
                "c.preprocessor_lexical_unavailable",
                "cpp.preprocessor_lexical_unavailable",
            ),
        )?;
    }
    Ok(())
}

fn append(
    output: &mut Emitter<'_>,
    range: Range<usize>,
    depth: usize,
    role: StructuralRole,
    syntax: &'static str,
) -> Result<(), AdapterError> {
    if output.capture.emitted == output.maximum {
        output.capture.exhausted = true;
        return Ok(());
    }
    (output.retain)(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role == StructuralRole::Signature,
        native_depth: depth,
    })?;
    output.capture.emitted = output
        .capture
        .emitted
        .checked_add(1)
        .ok_or_else(|| query_failure("query-c-capture-accounting"))?;
    Ok(())
}

struct Emitter<'a> {
    capture: ReplacementCaptures,
    maximum: usize,
    retain: &'a mut dyn FnMut(QueryCandidate) -> Result<(), AdapterError>,
}

struct Scanner<'a> {
    bytes: &'a [u8],
    cursor: usize,
    cancellation: &'a Cancellation,
}

impl Scanner<'_> {
    fn next(&mut self) -> Result<Option<(usize, u8)>, AdapterError> {
        loop {
            self.cancellation.check()?;
            let Some(&byte) = self.bytes.get(self.cursor) else {
                return Ok(None);
            };
            let tail = self
                .bytes
                .get(self.cursor..)
                .ok_or_else(|| query_failure("query-c-preprocessor-scan"))?;
            let splice = if tail.starts_with(b"\\\r\n") {
                3
            } else if tail.starts_with(b"\\\n") {
                2
            } else {
                0
            };
            let position = self.cursor;
            self.cursor = self
                .cursor
                .checked_add(if splice == 0 { 1 } else { splice })
                .ok_or_else(|| query_failure("query-c-preprocessor-coordinate"))?;
            if splice == 0 {
                return Ok(Some((position, byte)));
            }
        }
    }

    fn consume_comment(&mut self) -> Result<bool, AdapterError> {
        let second = self.next()?.map(|(_, byte)| byte);
        if second == Some(b'/') {
            while let Some((_, byte)) = self.next()? {
                if byte == b'\n' {
                    break;
                }
            }
            return Ok(true);
        }
        while let Some((_, byte)) = self.next()? {
            if byte == b'*' && self.peek()?.is_some_and(|(_, next)| next == b'/') {
                self.next()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn literal_prefix(
        &mut self,
        first: u8,
        dialect: PreprocessorDialect,
    ) -> Result<Option<(u8, bool)>, AdapterError> {
        if matches!(first, b'"' | b'\'') {
            return Ok(Some((first, false)));
        }
        if !identifier_byte(first) {
            return Ok(None);
        }
        let mut prefix = [0u8; 3];
        prefix[0] = first;
        let mut length = 1usize;
        while self.peek()?.is_some_and(|(_, byte)| identifier_byte(byte)) {
            let (_, byte) = self
                .next()?
                .ok_or_else(|| query_failure("query-c-preprocessor-identifier"))?;
            if let Some(slot) = prefix.get_mut(length) {
                *slot = byte;
            }
            length = length
                .checked_add(1)
                .ok_or_else(|| query_failure("query-c-preprocessor-length"))?;
        }
        let ordinary = (length == 1 && matches!(prefix[0], b'L' | b'u' | b'U'))
            || (length == 2 && &prefix[..2] == b"u8");
        let raw = dialect == PreprocessorDialect::Cpp
            && ((length == 1 && prefix[0] == b'R')
                || (length == 2 && matches!(&prefix[..2], b"LR" | b"uR" | b"UR"))
                || (length == 3 && prefix == *b"u8R"));
        if self.peek()?.is_some_and(|(_, byte)| {
            (ordinary && matches!(byte, b'"' | b'\'')) || (raw && byte == b'"')
        }) {
            let (_, quote) = self
                .next()?
                .ok_or_else(|| query_failure("query-c-preprocessor-prefix"))?;
            Ok(Some((quote, raw)))
        } else {
            Ok(None)
        }
    }

    fn consume_literal(&mut self, quote: u8, raw: bool) -> Result<bool, AdapterError> {
        if raw {
            return self.consume_raw_literal();
        }
        while let Some((_, byte)) = self.next()? {
            if byte == b'\n' {
                return Ok(false);
            }
            if byte == b'\\' {
                if self.next()?.is_none() {
                    return Ok(false);
                }
            } else if byte == quote {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn consume_raw_literal(&mut self) -> Result<bool, AdapterError> {
        let mut delimiter = [0u8; 16];
        let mut length = 0usize;
        loop {
            self.cancellation.check()?;
            let Some(&byte) = self.bytes.get(self.cursor) else {
                return Ok(false);
            };
            self.cursor += 1;
            if byte == b'(' {
                break;
            }
            if length == delimiter.len()
                || !(0x21..=0x7e).contains(&byte)
                || matches!(byte, b')' | b'\\')
            {
                return Ok(false);
            }
            delimiter[length] = byte;
            length += 1;
        }
        // Raw contents retain physical splices and newlines; Scanner::next
        // deliberately implements logical splices only outside this body.
        while let Some(&byte) = self.bytes.get(self.cursor) {
            self.cancellation.check()?;
            self.cursor += 1;
            if byte == b')'
                && self
                    .bytes
                    .get(self.cursor..self.cursor.saturating_add(length))
                    == Some(&delimiter[..length])
                && self.bytes.get(self.cursor.saturating_add(length)) == Some(&b'"')
            {
                self.cursor = self
                    .cursor
                    .checked_add(length + 1)
                    .ok_or_else(|| query_failure("query-cpp-preprocessor-coordinate"))?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn consume_number(&mut self, mut previous: u8) -> Result<(), AdapterError> {
        while let Some((_, byte)) = self.peek()? {
            let continuation = if byte == b'\'' {
                let mut after_separator = Self {
                    bytes: self.bytes,
                    cursor: self.cursor,
                    cancellation: self.cancellation,
                };
                after_separator.next()?;
                after_separator
                    .peek()?
                    .is_some_and(|(_, next)| identifier_byte(next))
            } else {
                identifier_byte(byte)
                    || byte == b'.'
                    || (matches!(byte, b'+' | b'-')
                        && matches!(previous, b'e' | b'E' | b'p' | b'P'))
            };
            if !continuation {
                break;
            }
            self.next()?
                .ok_or_else(|| query_failure("query-c-preprocessor-number"))?;
            previous = byte;
        }
        Ok(())
    }

    fn peek(&self) -> Result<Option<(usize, u8)>, AdapterError> {
        Self {
            bytes: self.bytes,
            cursor: self.cursor,
            cancellation: self.cancellation,
        }
        .next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Collected {
        candidates: Vec<QueryCandidate>,
        exhausted: bool,
    }

    fn scan_collect(
        source: &[u8],
        range: Range<usize>,
        depth: usize,
        mode: ReplacementMode,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Collected, AdapterError> {
        let mut candidates = Vec::new();
        let result = scan_replacement(
            source,
            range,
            depth,
            mode,
            maximum,
            cancellation,
            &mut |candidate| {
                candidates.push(candidate);
                Ok(())
            },
        )?;
        assert_eq!(result.emitted, candidates.len());
        Ok(Collected {
            candidates,
            exhausted: result.exhausted,
        })
    }

    fn literals(raw: &[u8]) -> Vec<&[u8]> {
        let result = scan_collect(
            raw,
            0..raw.len(),
            1,
            ReplacementMode::Literals,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(!result.exhausted);
        result
            .candidates
            .into_iter()
            .map(|candidate| &raw[candidate.start..candidate.end])
            .collect()
    }

    #[test]
    fn lexical_mode_keeps_comments_documentation_and_exact_literal_budget() {
        let raw = b"/** hidden \"not-string\" */ \"tail\" // line\r\n";
        let all = scan_collect(
            raw,
            0..raw.len(),
            2,
            ReplacementMode::Lexical,
            4,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(!all.exhausted);
        assert_eq!(all.candidates.len(), 4);
        assert_eq!(
            all.candidates
                .iter()
                .map(|candidate| candidate.role)
                .collect::<Vec<_>>(),
            [
                StructuralRole::Comment,
                StructuralRole::Documentation,
                StructuralRole::StringLiteral,
                StructuralRole::Comment
            ]
        );
        let literal = &all.candidates[2];
        assert_eq!(&raw[literal.start..literal.end], b"\"tail\"");
        let line = &all.candidates[3];
        assert_eq!(&raw[line.start..line.end], b"// line\r");
        let short = scan_collect(
            raw,
            0..raw.len(),
            2,
            ReplacementMode::Lexical,
            2,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(short.exhausted);
        assert_eq!(short.candidates.len(), 2);
        let broken = b"1 /* comment */ 'unfinished\r\n";
        let gaps = scan_collect(
            broken,
            0..broken.len(),
            2,
            ReplacementMode::Gaps,
            1,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(gaps.candidates.len(), 1);
        let gap = &gaps.candidates[0];
        assert!(gap.required);
        assert_eq!(&broken[gap.start..gap.end], b"'unfinished\r\n");
    }

    #[test]
    fn replacement_literals_exclude_characters_comments_and_stringification() {
        assert_eq!(
            literals(br#"'"' /* "block" */ "written" #value // "line""#),
            [b"\"written\"".as_slice()]
        );
    }

    #[test]
    fn prefixes_adjacent_literals_and_escaped_quotes_preserve_physical_tokens() {
        let raw = br#"L"wide" u8"utf8" U"utf32" u"utf16" "a\"b""adjacent" TOKEN"suffix""#;
        assert_eq!(
            literals(raw),
            [
                b"L\"wide\"".as_slice(),
                b"u8\"utf8\"",
                b"U\"utf32\"",
                b"u\"utf16\"",
                br#""a\"b""#,
                b"\"adjacent\"",
                b"\"suffix\""
            ]
        );
    }

    #[test]
    fn physical_lf_and_crlf_splices_keep_comments_opaque_and_strings_exact() {
        let raw = b"// continued \\\r\n\"hidden\"\n\"first\\\nsecond\" u\\\r\n8\"third\"";
        assert_eq!(
            literals(raw),
            [b"\"first\\\nsecond\"".as_slice(), b"u\\\r\n8\"third\""]
        );
    }

    #[test]
    fn preprocessing_numbers_keep_separators_and_following_strings_distinct() {
        let cancellation = Cancellation::new();
        for number in [
            b"1'000".as_slice(),
            b"0xDEAD'BEEF",
            b"0b10'01",
            b"12'345UL",
            b".1'25e+2",
            b"0x1'fp-2",
            b"1e+2'0",
            b".25E-1",
            b"1P+2",
            b"1'\\\n000",
            b"0xA'\\\r\nB",
            b"1e\\\n+2",
            b"1'unit",
        ] {
            let gaps = scan_collect(
                number,
                0..number.len(),
                1,
                ReplacementMode::Gaps,
                8,
                &cancellation,
            )
            .unwrap();
            assert!(gaps.candidates.is_empty(), "{number:?}");
            assert!(!gaps.exhausted);
            assert!(literals(number).is_empty());
            let mut followed = number.to_vec();
            followed.extend_from_slice(b" \"written\"");
            assert_eq!(
                literals(&followed),
                [b"\"written\"".as_slice()],
                "{number:?}"
            );
        }
    }

    #[test]
    fn numbers_do_not_hide_real_characters_comments_or_adjacent_quotes() {
        assert_eq!(
            literals(b"1'000 'x' L'\\\"' /* \"comment\" */ 2e+3 \"left\"\"right\""),
            [b"\"left\"".as_slice(), b"\"right\""]
        );
        let raw = b"1'000 'unfinished";
        let result = scan_collect(
            raw,
            0..raw.len(),
            1,
            ReplacementMode::Gaps,
            1,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(
            (result.candidates[0].start, result.candidates[0].end),
            (6, raw.len())
        );
        assert!(literals(raw).is_empty());
    }

    #[test]
    fn unterminated_tokens_emit_exact_gaps_only_in_identity_mode() {
        for raw in [
            b"\"unfinished\n".as_slice(),
            b"/* unfinished",
            b"'unfinished",
            b"L'unfinished",
        ] {
            let cancellation = Cancellation::new();
            let gaps = scan_collect(
                raw,
                0..raw.len(),
                1,
                ReplacementMode::Gaps,
                1,
                &cancellation,
            )
            .unwrap();
            assert_eq!(gaps.candidates.len(), 1);
            assert_eq!(
                (gaps.candidates[0].start, gaps.candidates[0].end),
                (0, raw.len())
            );
            assert!(literals(raw).is_empty());
        }
    }

    #[test]
    fn view_offsets_and_sink_failures_preserve_the_caller_contract() {
        let raw = b"prefix: \"written\" suffix";
        let cancellation = Cancellation::new();
        let result =
            scan_collect(raw, 8..17, 7, ReplacementMode::Literals, 1, &cancellation).unwrap();
        assert_eq!(result.candidates.len(), 1);
        let capture = &result.candidates[0];
        assert_eq!(
            (capture.start, capture.end, capture.native_depth),
            (8, 17, 7)
        );
        let error = scan_replacement(
            raw,
            8..17,
            7,
            ReplacementMode::Literals,
            1,
            &cancellation,
            &mut |_| Err(query_failure("test-c-replacement-sink")),
        );
        assert!(error.is_err());
    }

    #[test]
    fn capture_exhaustion_and_cancellation_are_explicit() {
        let raw = b"\"one\" \"two\"";
        let result = scan_collect(
            raw,
            0..raw.len(),
            1,
            ReplacementMode::Literals,
            1,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert!(result.exhausted);
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            scan_collect(
                raw,
                0..raw.len(),
                1,
                ReplacementMode::Literals,
                1,
                &cancellation
            )
            .is_err()
        );
    }
    #[test]
    fn directive_extent_keeps_comments_splices_and_later_directives_distinct() {
        for (body, following) in [
            (
                b" 1'000 /* \"hidden\" */ \"tail\"\n".as_slice(),
                b"#define NEXT \"next\"\n".as_slice(),
            ),
            (
                b" /* comment\n#define FAKE \"hidden\"\n*/ \"tail\"\n",
                b"#define NEXT \"next\"\n",
            ),
            (
                b" /* comment\r\n*/ L\"tail\"\r\n",
                b"#define NEXT \"next\"\r\n",
            ),
            (b" 1'\\\r\n000 \"tail\"\r\n", b"#define NEXT \"next\"\r\n"),
            (
                b" \"// not a comment\" '\"' \"tail\"\n",
                b"#define NEXT \"next\"\n",
            ),
            (
                b" // hidden\\\n#define FAKE \"hidden\"\n",
                b"#define NEXT \"next\"\n",
            ),
            (b" 'unfinished\n", b"#define NEXT \"next\"\n"),
        ] {
            let prefix = b"prefix:";
            let mut source = prefix.to_vec();
            source.extend_from_slice(body);
            source.extend_from_slice(following);
            let end =
                directive_end(&source, prefix.len(), source.len(), &Cancellation::new()).unwrap();
            assert_eq!(end, prefix.len() + body.len(), "{source:?}");
            let result = scan_collect(
                &source,
                prefix.len()..end,
                1,
                ReplacementMode::Literals,
                32,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(!result.exhausted);
            for candidate in result.candidates {
                assert!(candidate.end <= end);
                assert_ne!(&source[candidate.start..candidate.end], b"\"next\"");
                assert_ne!(&source[candidate.start..candidate.end], b"\"hidden\"");
            }
        }
        let raw = b" /* unfinished\n#define FAKE \"hidden\"\n";
        assert_eq!(
            directive_end(raw, 0, raw.len(), &Cancellation::new()).unwrap(),
            raw.len()
        );
        let gaps = scan_collect(
            raw,
            0..raw.len(),
            1,
            ReplacementMode::Gaps,
            1,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(gaps.candidates.len(), 1);
        assert_eq!(
            (gaps.candidates[0].start, gaps.candidates[0].end),
            (1, raw.len())
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(directive_end(b" \"tail\"\n", 0, 8, &cancellation).is_err());
    }
    #[test]
    fn macro_extent_cannot_cross_native_included_ranges_or_default_sentinels() {
        let source = b" 1 /* selected */ \"decoy\"\n#define NEXT \"next\"\n";
        let selected_end = b" 1 /* selected */".len();
        let next_start = source
            .windows(b"#define NEXT".len())
            .position(|bytes| bytes == b"#define NEXT")
            .unwrap();
        let point = tree_sitter::Point { row: 0, column: 0 };
        let ranges = [
            tree_sitter::Range {
                start_byte: 0,
                end_byte: selected_end,
                start_point: point,
                end_point: point,
            },
            tree_sitter::Range {
                start_byte: next_start,
                end_byte: source.len(),
                start_point: point,
                end_point: point,
            },
        ];
        let cancellation = Cancellation::new();
        let limit = admitted_macro_limit(0, selected_end, source.len(), &ranges, &cancellation)
            .unwrap()
            .unwrap();
        assert_eq!(limit, selected_end);
        let end = directive_end(source, 0, limit, &cancellation).unwrap();
        assert_eq!(end, selected_end);
        assert!(
            scan_collect(
                source,
                0..end,
                1,
                ReplacementMode::Literals,
                8,
                &cancellation
            )
            .unwrap()
            .candidates
            .is_empty()
        );
        assert_eq!(
            admitted_macro_limit(0, next_start + 1, source.len(), &ranges, &cancellation).unwrap(),
            None
        );
        let full = [tree_sitter::Range {
            start_byte: 0,
            end_byte: usize::MAX,
            start_point: point,
            end_point: point,
        }];
        assert_eq!(
            admitted_macro_limit(0, selected_end, source.len(), &full, &cancellation).unwrap(),
            Some(source.len())
        );
        assert_eq!(
            directive_end(source, 0, source.len(), &cancellation).unwrap(),
            next_start
        );
    }

    fn cpp_scan(
        source: &[u8],
        mode: ReplacementMode,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Collected, AdapterError> {
        let mut candidates = Vec::new();
        let result = scan_replacement_for(
            source,
            0..source.len(),
            1,
            mode,
            maximum,
            PreprocessorDialect::Cpp,
            cancellation,
            &mut |candidate| {
                candidates.push(candidate);
                Ok(())
            },
        )?;
        assert_eq!(result.emitted, candidates.len());
        Ok(Collected {
            candidates,
            exhausted: result.exhausted,
        })
    }

    #[test]
    fn cpp_raw_prefixes_keep_whole_tokens_and_hide_comment_decoys() {
        for prefix in ["R", "LR", "uR", "UR", "u8R"] {
            for delimiter in ["", "tag", "1234567890abcdef"] {
                let source = format!(
                    "{prefix}\"{delimiter}(literal \\\r\n#define FAKE \"quoted\" // line /* block */){delimiter}\""
                );
                let captured = cpp_scan(
                    source.as_bytes(),
                    ReplacementMode::Lexical,
                    1,
                    &Cancellation::new(),
                )
                .unwrap();
                assert!(!captured.exhausted);
                assert_eq!(captured.candidates.len(), 1);
                let literal = &captured.candidates[0];
                assert_eq!(literal.role, StructuralRole::StringLiteral);
                assert_eq!(literal.syntax, "cpp.preprocessor_raw_string");
                assert_eq!(
                    &source.as_bytes()[literal.start..literal.end],
                    source.as_bytes()
                );
            }
        }
    }

    #[test]
    fn cpp_raw_delimiter_failures_are_mandatory_gaps_without_partial_literals() {
        for source in [
            b"R\"tag(unclosed".as_slice(),
            b"u8R\"bad delimiter(body)bad delimiter\"",
            b"R\"1234567890abcdefg(body)1234567890abcdefg\"",
            b"R\"tag(body)other\"",
        ] {
            let lexical =
                cpp_scan(source, ReplacementMode::Lexical, 8, &Cancellation::new()).unwrap();
            assert!(lexical.candidates.is_empty());
            let gaps = cpp_scan(source, ReplacementMode::Gaps, 1, &Cancellation::new()).unwrap();
            assert_eq!(gaps.candidates.len(), 1);
            assert!(gaps.candidates[0].required);
            assert_eq!(
                gaps.candidates[0].syntax,
                "cpp.preprocessor_lexical_unavailable"
            );
            assert_eq!(
                &source[gaps.candidates[0].start..gaps.candidates[0].end],
                source
            );
        }
    }

    #[test]
    fn cpp_numbers_characters_comments_and_adjacent_strings_keep_separate_sites() {
        let source = b"1'000 'x' u8\"prefix\" R\"tag(/* hidden */ \\\n)tag\" \"suffix\" /* real */";
        let captured = cpp_scan(source, ReplacementMode::Lexical, 4, &Cancellation::new()).unwrap();
        assert!(!captured.exhausted);
        assert_eq!(
            captured
                .candidates
                .iter()
                .map(|candidate| (candidate.role, &source[candidate.start..candidate.end]))
                .collect::<Vec<_>>(),
            [
                (StructuralRole::StringLiteral, b"u8\"prefix\"".as_slice()),
                (
                    StructuralRole::StringLiteral,
                    b"R\"tag(/* hidden */ \\\n)tag\"".as_slice()
                ),
                (StructuralRole::StringLiteral, b"\"suffix\"".as_slice()),
                (StructuralRole::Comment, b"/* real */".as_slice()),
            ]
        );
        let short = cpp_scan(source, ReplacementMode::Lexical, 1, &Cancellation::new()).unwrap();
        assert!(short.exhausted);
        assert_eq!(short.candidates.len(), 1);
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(cpp_scan(source, ReplacementMode::Lexical, 4, &cancellation).is_err());
    }

    #[test]
    fn cpp_header_bom_is_trivia_only_at_the_physical_file_start() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let cancellation = Cancellation::new();
        for prefix in [b"".as_slice(), b"\xef\xbb\xbf".as_slice()] {
            let source = [prefix, b"#define WRITTEN 1\n".as_slice()].concat();
            let tree = parser.parse(&source, None).unwrap();
            let mut headers = Vec::new();
            assert!(
                cpp_macro_headers(
                    tree.root_node(),
                    &source,
                    &tree.included_ranges(),
                    &cancellation,
                    &mut |header, _| {
                        headers.push(header);
                        Ok(())
                    }
                )
                .unwrap()
            );
            assert_eq!(headers.len(), 1);
            assert_eq!(headers[0].start, prefix.len());
            assert_eq!(&source[headers[0].name.clone()], b"WRITTEN");
            assert_eq!(headers[0].end, source.len());
        }
        let prefix = b"int before;\n";
        let source = [
            prefix.as_slice(),
            b"\xef\xbb\xbf#define HIDDEN 1\n#define VISIBLE 2\n".as_slice(),
        ]
        .concat();
        let tree = parser.parse(&source, None).unwrap();
        for ranges in [
            tree.included_ranges(),
            vec![tree_sitter::Range {
                start_byte: prefix.len(),
                end_byte: source.len(),
                start_point: tree_sitter::Point::new(1, 0),
                end_point: tree_sitter::Point::new(3, 0),
            }],
        ] {
            let mut names = Vec::new();
            assert!(
                cpp_macro_headers(
                    tree.root_node(),
                    &source,
                    &ranges,
                    &cancellation,
                    &mut |header, _| {
                        names.push(source[header.name.clone()].to_vec());
                        Ok(())
                    }
                )
                .unwrap()
            );
            assert_eq!(names, [b"VISIBLE".to_vec()]);
        }
    }
}
