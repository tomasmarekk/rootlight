//! CMake header templates keep their physical C declarations and template gaps.
//! Private parse masks never become evidence or generated identifiers. Recognition
//! examines the complete decoded source before the host grammar is replaced.

use std::ops::Range;

use rootlight_adapter_sdk::{AdapterError, AnalysisLimits, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;

use super::*;

pub(crate) const CMAKE_HEADER_PROJECTION_POLICY: &str =
    "rootlight.treesitter.cmake-header-template-input/2";

pub(super) fn is_header_path(path: &str) -> bool {
    path.get(path.len().saturating_sub(8)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".h.cmake"))
}

#[derive(Clone, Copy)]
struct Marker {
    start: usize,
    end: usize,
    mask: bool,
    kind: MarkerKind,
    mask_prefix_end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerKind {
    Substitution,
    Directive,
    AmbiguousInvocation,
    QuotedCrlf,
}

pub(super) struct HeaderPlan {
    markers: Vec<Marker>,
}

impl HeaderPlan {
    pub(super) fn recognize(
        path: &str,
        source: &NativeSourceView<'_>,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        cancellation.check()?;
        if !is_header_path(path) {
            return Ok(None);
        }
        if complete_cmake_script(source.as_bytes(), cancellation)? {
            return Ok(None);
        }
        if Self::scan(source, limits, false, cancellation)?.is_none() {
            return Ok(None);
        }
        Self::scan(source, limits, true, cancellation)
    }

    #[allow(clippy::too_many_lines)]
    fn scan(
        source: &NativeSourceView<'_>,
        limits: &AnalysisLimits,
        collect: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        let bytes = source.as_bytes();
        let mut plan = Self {
            markers: Vec::new(),
        };
        let mut index = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
        let mut line_start = true;
        let mut c_evidence = false;
        let mut typed_declaration = false;
        let mut command_ambiguity = false;
        let mut block_comment = false;
        let mut line_comment = false;
        let mut c_line_comment = false;
        let mut quote = None;
        let mut braces = 0usize;
        let mut marker_retry_at = 0usize;
        while let Some(byte) = bytes.get(index).copied() {
            check_scan(index, cancellation)?;
            if index >= marker_retry_at
                && let Some(marker) = template_marker(bytes, index, cancellation)?
            {
                let end = marker.end;
                let mask = !block_comment && !line_comment && quote.is_none();
                if mask || marker.complete {
                    if collect {
                        plan.push(
                            Marker {
                                start: index,
                                end,
                                mask,
                                kind: MarkerKind::Substitution,
                                mask_prefix_end: 0,
                            },
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?;
                    }
                    index = end;
                    line_start = false;
                    continue;
                }
                // Keep C literal/comment transitions visible without repeatedly
                // rescanning the suffix of an incomplete nested substitution.
                marker_retry_at = end;
            }
            if line_comment {
                if c_line_comment && let Some(end) = escaped_line_end(bytes, index) {
                    index = end;
                    continue;
                }
                if matches!(byte, b'\r' | b'\n') {
                    line_comment = false;
                    line_start = true;
                }
                index += 1;
                continue;
            }
            if block_comment {
                if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    block_comment = false;
                    index += 2;
                } else {
                    if matches!(byte, b'\r' | b'\n') {
                        line_start = true;
                    }
                    index += 1;
                }
                continue;
            }
            if let Some(delimiter) = quote {
                if matches!(byte, b'\r' | b'\n') {
                    quote = None;
                    line_start = true;
                    index += 1;
                    continue;
                }
                if byte == b'\\' && bytes.get(index + 1).is_some() {
                    if let Some(end) = escaped_line_end(bytes, index) {
                        if collect && end - index == 3 {
                            plan.push(
                                Marker {
                                    start: index,
                                    end,
                                    mask: false,
                                    kind: MarkerKind::QuotedCrlf,
                                    mask_prefix_end: 0,
                                },
                                source.owned_bytes(),
                                limits,
                                cancellation,
                            )?;
                        }
                        index = end;
                        continue;
                    }
                    // An escape does not suppress configure-file substitution;
                    // revisit a following marker while keeping C literal state.
                    index += if bytes
                        .get(index + 1)
                        .is_some_and(|next| matches!(next, b'@' | b'$'))
                    {
                        1
                    } else {
                        2
                    };
                } else {
                    if byte == delimiter {
                        quote = None;
                    }
                    index += 1;
                }
                continue;
            }
            if matches!(byte, b'\r' | b'\n') {
                line_start = true;
                index += 1;
                continue;
            }
            if byte.is_ascii_whitespace() {
                index += 1;
                continue;
            }
            if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
                block_comment = true;
                index += 2;
                continue;
            }
            if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
                line_comment = true;
                c_line_comment = true;
                index += 2;
                continue;
            }
            if line_start
                && (bytes.get(index..).is_some_and(|tail| {
                    tail.starts_with(b"diff --git ")
                        || tail.starts_with(b"--- ")
                        || tail.starts_with(b"Index: ")
                        || tail.starts_with(b"@@ ")
                }))
            {
                return Ok(None);
            }
            if line_start && byte == b'#' {
                let mut name_start = index + 1;
                while bytes
                    .get(name_start)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                {
                    check_scan(name_start, cancellation)?;
                    name_start += 1;
                }
                if bytes.get(name_start) == Some(&b'[') {
                    return Ok(None);
                }
                let mut name_end = name_start;
                while bytes.get(name_end).is_some_and(u8::is_ascii_alphanumeric) {
                    check_scan(name_end, cancellation)?;
                    name_end += 1;
                }
                let name = bytes.get(name_start..name_end).unwrap_or_default();
                if matches!(name, b"cmakedefine" | b"cmakedefine01") {
                    let end = directive_end(bytes, index, cancellation)?;
                    if collect {
                        plan.push(
                            Marker {
                                start: index,
                                end,
                                mask: true,
                                kind: MarkerKind::Directive,
                                mask_prefix_end: 0,
                            },
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?;
                        plan.directive_substitutions(
                            bytes,
                            name_end,
                            end,
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?;
                    }
                    c_evidence = true;
                    index = end;
                    line_start = false;
                    continue;
                } else if matches!(
                    name,
                    b"include"
                        | b"define"
                        | b"undef"
                        | b"if"
                        | b"ifdef"
                        | b"ifndef"
                        | b"elif"
                        | b"else"
                        | b"endif"
                        | b"pragma"
                        | b"line"
                        | b"error"
                        | b"warning"
                ) {
                    c_evidence = true;
                    let end = directive_end(bytes, index, cancellation)?;
                    block_comment = if collect {
                        plan.directive_substitutions(
                            bytes,
                            name_end,
                            end,
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?
                    } else {
                        walk_directive(bytes, name_end, end, cancellation, |_, _, _| Ok(()))?
                    };
                    index = end;
                    line_start = false;
                    continue;
                } else {
                    // A CMake line comment is opaque even if its prose happens
                    // to contain C syntax or another directive spelling.
                    line_comment = true;
                    c_line_comment = false;
                }
            }
            if line_start && braces == 0 && (byte.is_ascii_alphabetic() || byte == b'_') {
                let mut end = index + 1;
                while bytes
                    .get(end)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    check_scan(end, cancellation)?;
                    end += 1;
                }
                let mut next = end;
                while bytes
                    .get(next)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                {
                    check_scan(next, cancellation)?;
                    next += 1;
                }
                if bytes.get(next) == Some(&b'(') {
                    let Some(mut end) = command_end(bytes, next, cancellation)? else {
                        return Ok(None);
                    };
                    let word = bytes
                        .get(index..end.min(next))
                        .unwrap_or_default()
                        .trim_ascii_end();
                    let mut after = end;
                    while bytes
                        .get(after)
                        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                    {
                        check_scan(after, cancellation)?;
                        after += 1;
                    }
                    if matches!(
                        word,
                        b"int"
                            | b"char"
                            | b"short"
                            | b"long"
                            | b"float"
                            | b"double"
                            | b"signed"
                            | b"unsigned"
                            | b"_Bool"
                    ) && bytes.get(after) == Some(&b';')
                    {
                        typed_declaration = true;
                        c_evidence = true;
                        line_start = false;
                        index += 1;
                        continue;
                    }
                    while bytes.get(end) == Some(&b';') {
                        check_scan(end, cancellation)?;
                        end += 1;
                    }
                    command_ambiguity = true;
                    if collect {
                        plan.push(
                            Marker {
                                start: index,
                                end,
                                mask: true,
                                kind: MarkerKind::AmbiguousInvocation,
                                mask_prefix_end: 0,
                            },
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?;
                        plan.directive_substitutions(
                            bytes,
                            next,
                            end,
                            source.owned_bytes(),
                            limits,
                            cancellation,
                        )?;
                    }
                    index = end;
                    line_start = true;
                    continue;
                }
                let word = bytes.get(index..end).unwrap_or_default();
                if matches!(
                    word,
                    b"struct"
                        | b"union"
                        | b"enum"
                        | b"typedef"
                        | b"extern"
                        | b"static"
                        | b"const"
                        | b"volatile"
                        | b"signed"
                        | b"unsigned"
                        | b"void"
                        | b"char"
                        | b"short"
                        | b"int"
                        | b"long"
                        | b"float"
                        | b"double"
                        | b"_Bool"
                        | b"_Atomic"
                        | b"inline"
                        | b"_Static_assert"
                        | b"_Thread_local"
                ) {
                    typed_declaration = true;
                    c_evidence = true;
                }
            }
            line_start = false;
            if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            }
            match byte {
                b'{' => {
                    braces = braces.saturating_add(1);
                }
                b'}' => {
                    braces = braces.saturating_sub(1);
                }
                _ => {}
            }
            index += 1;
        }
        cancellation.check()?;
        // A bare top-level invocation is also valid CMake syntax, including a
        // trailing semicolon. Typed C neighbors prove only those declarations;
        // they cannot decide whether the invocation expands into generated C.
        Ok((c_evidence && (!command_ambiguity || typed_declaration)).then_some(plan))
    }

    fn directive_substitutions(
        &mut self,
        bytes: &[u8],
        index: usize,
        end: usize,
        existing: usize,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        walk_directive(bytes, index, end, cancellation, |start, stop, mask| {
            self.push(
                Marker {
                    start,
                    end: stop,
                    mask,
                    kind: MarkerKind::Substitution,
                    mask_prefix_end: 0,
                },
                existing,
                limits,
                cancellation,
            )
        })
    }

    fn push(
        &mut self,
        mut marker: Marker,
        existing: usize,
        limits: &AnalysisLimits,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        cancellation.check()?;
        require_provider_limit(
            ResourceKind::SyntaxNodes,
            self.markers.len().saturating_add(1),
            limits.max_syntax_nodes(),
        )?;
        if self.markers.len() == self.markers.capacity() {
            let next = self
                .markers
                .len()
                .saturating_mul(2)
                .max(4)
                .min(limits.max_syntax_nodes());
            let added = next
                .checked_sub(self.markers.capacity())
                .and_then(|count| count.checked_mul(std::mem::size_of::<Marker>()))
                .ok_or(SinkError::AccountingOverflow)?;
            self.admit_memory(existing, added, limits.max_reported_memory_bytes())?;
            self.markers
                .try_reserve_exact(next.saturating_sub(self.markers.len()))
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        self.admit_memory(existing, 0, limits.max_reported_memory_bytes())?;
        marker.mask_prefix_end = self
            .markers
            .last()
            .map_or(0, |previous| previous.mask_prefix_end)
            .max(if marker.mask { marker.end } else { 0 });
        self.markers.push(marker);
        Ok(())
    }

    fn admit_memory(
        &self,
        existing: usize,
        added: usize,
        maximum: usize,
    ) -> Result<(), AdapterError> {
        let markers = self
            .markers
            .capacity()
            .checked_mul(std::mem::size_of::<Marker>())
            .ok_or(SinkError::AccountingOverflow)?;
        let total = existing
            .checked_add(markers)
            .and_then(|total| total.checked_add(added))
            .ok_or(SinkError::AccountingOverflow)?;
        require_provider_limit(ResourceKind::ReportedMemoryBytes, total, maximum)
    }

    fn staged_owned_bytes(&self, capacity: usize) -> Result<usize, AdapterError> {
        self.markers
            .capacity()
            .checked_mul(std::mem::size_of::<Marker>())
            .and_then(|bytes| bytes.checked_add(capacity))
            .ok_or(SinkError::AccountingOverflow.into())
    }

    pub(super) fn parse_bytes(
        &self,
        source: &NativeSourceView<'_>,
        host: bool,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, AdapterError> {
        cancellation.check()?;
        self.admit_memory(source.owned_bytes(), source.as_bytes().len(), maximum)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(source.as_bytes().len())
            .map_err(|_| SinkError::AllocationFailed)?;
        self.admit_memory(source.owned_bytes(), bytes.capacity(), maximum)?;
        for chunk in source.as_bytes().chunks(CANCELLATION_BYTE_INTERVAL) {
            cancellation.check()?;
            bytes.extend_from_slice(chunk);
        }
        if host {
            for chunk in bytes.chunks_mut(CANCELLATION_BYTE_INTERVAL) {
                cancellation.check()?;
                for byte in chunk {
                    if !matches!(byte, b'\r' | b'\n') {
                        *byte = b' ';
                    }
                }
            }
        } else {
            // A whole directive wins over a substitution nested in its value.
            // Otherwise placeholder bytes would be reintroduced as C tokens.
            for directive in [false, true] {
                for marker in self.markers.iter().filter(|marker| {
                    matches!(
                        marker.kind,
                        MarkerKind::Directive | MarkerKind::AmbiguousInvocation
                    ) == directive
                }) {
                    cancellation.check()?;
                    if marker.kind == MarkerKind::QuotedCrlf {
                        // The pinned C grammar consumes only one escaped byte.
                        // Shift its backslash across CR privately so escaped LF
                        // remains a string token; physical evidence keeps CRLF.
                        let target = bytes
                            .get_mut(marker.start..marker.end)
                            .ok_or_else(|| provider_failure("cmake-header-splice-span"))?;
                        if target != b"\\\r\n" {
                            return Err(provider_failure("cmake-header-splice-source"));
                        }
                        target.copy_from_slice(b" \\\n");
                        continue;
                    }
                    if !marker.mask {
                        continue;
                    }
                    let target = bytes
                        .get_mut(marker.start..marker.end)
                        .ok_or_else(|| provider_failure("cmake-header-mask-span"))?;
                    for chunk in target.chunks_mut(CANCELLATION_BYTE_INTERVAL) {
                        cancellation.check()?;
                        for byte in chunk {
                            if !matches!(byte, b'\r' | b'\n') {
                                *byte = if directive { b' ' } else { b'x' };
                            }
                        }
                    }
                }
            }
        }
        Ok(bytes)
    }

    fn overlaps_mask(&self, span: Range<usize>) -> bool {
        let insertion = self
            .markers
            .partition_point(|marker| marker.start < span.end);
        insertion
            .checked_sub(1)
            .is_some_and(|index| self.markers[index].mask_prefix_end > span.start)
    }

    fn append_gaps(
        &self,
        candidates: &mut Vec<QueryCandidate>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        try_reserve_cancellable(
            candidates,
            self.markers.len(),
            cancellation,
            "cmake-header-gap-allocation",
        )?;
        for marker in &self.markers {
            cancellation.check()?;
            if marker.kind == MarkerKind::QuotedCrlf {
                continue;
            }
            candidates.push(QueryCandidate {
                start: marker.start,
                end: marker.end,
                role: StructuralRole::Signature,
                syntax: if marker.kind == MarkerKind::AmbiguousInvocation {
                    "cmake.header_ambiguity"
                } else {
                    "cmake.header_template"
                },
                required: true,
                native_depth: 1,
            });
        }
        Ok(())
    }
}

fn escaped_line_end(bytes: &[u8], index: usize) -> Option<usize> {
    if bytes.get(index) != Some(&b'\\') {
        return None;
    }
    match bytes.get(index + 1) {
        Some(b'\n') => Some(index + 2),
        Some(b'\r') if bytes.get(index + 2) == Some(&b'\n') => Some(index + 3),
        _ => None,
    }
}

pub(super) fn line_end(
    bytes: &[u8],
    mut index: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    while let Some(byte) = bytes.get(index) {
        check_scan(index, cancellation)?;
        if matches!(byte, b'\r' | b'\n') {
            break;
        }
        index += 1;
    }
    Ok(index)
}

fn directive_end(
    bytes: &[u8],
    start: usize,
    cancellation: &Cancellation,
) -> Result<usize, AdapterError> {
    let mut end = line_end(bytes, start, cancellation)?;
    while end > start && bytes.get(end - 1) == Some(&b'\\') {
        cancellation.check()?;
        let next = if bytes.get(end..end.saturating_add(2)) == Some(b"\r\n") {
            end + 2
        } else if bytes.get(end).is_some() {
            end + 1
        } else {
            break;
        };
        end = line_end(bytes, next, cancellation)?;
    }
    Ok(end)
}

pub(super) enum BracketEnd {
    NotBracket,
    Unclosed,
    Closed(usize),
}

pub(super) fn bracket_end(
    bytes: &[u8],
    start: usize,
    cancellation: &Cancellation,
) -> Result<BracketEnd, AdapterError> {
    if bytes.get(start) != Some(&b'[') {
        return Ok(BracketEnd::NotBracket);
    }
    let mut open = start + 1;
    while bytes.get(open) == Some(&b'=') {
        check_scan(open, cancellation)?;
        open += 1;
    }
    if bytes.get(open) != Some(&b'[') {
        return Ok(BracketEnd::NotBracket);
    }
    let equals = open - start - 1;
    let mut index = open + 1;
    while let Some(byte) = bytes.get(index) {
        check_scan(index, cancellation)?;
        if *byte == b']' {
            let mut close = index + 1;
            while bytes.get(close) == Some(&b'=') {
                check_scan(close, cancellation)?;
                close += 1;
            }
            if close - index - 1 == equals && bytes.get(close) == Some(&b']') {
                return Ok(BracketEnd::Closed(close + 1));
            }
            index = close;
        } else {
            index += 1;
        }
    }
    Ok(BracketEnd::Unclosed)
}

pub(super) fn command_end(
    bytes: &[u8],
    open: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    let mut index = open + 1;
    let mut depth = 1usize;
    let mut quoted = false;
    while let Some(byte) = bytes.get(index).copied() {
        check_scan(index, cancellation)?;
        if byte == b'\\' {
            index = index.saturating_add(2);
            continue;
        }
        if quoted {
            if byte == b'"' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            index += 1;
            continue;
        }
        if matches!(byte, b'[' | b'#') {
            match bracket_end(bytes, index + usize::from(byte == b'#'), cancellation)? {
                BracketEnd::Closed(end) => {
                    index = end;
                    continue;
                }
                BracketEnd::Unclosed => return Ok(None),
                BracketEnd::NotBracket if byte == b'#' => {
                    index = line_end(bytes, index, cancellation)?;
                    continue;
                }
                BracketEnd::NotBracket => {}
            }
        }
        match byte {
            b'(' => depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(index + 1));
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(None)
}

struct TemplateMarker {
    end: usize,
    complete: bool,
}

fn template_marker(
    bytes: &[u8],
    start: usize,
    cancellation: &Cancellation,
) -> Result<Option<TemplateMarker>, AdapterError> {
    if bytes.get(start) == Some(&b'@') {
        let mut index = start + 1;
        while let Some(byte) = bytes.get(index) {
            check_scan(index, cancellation)?;
            if *byte == b'@' && index > start + 1 {
                return Ok(Some(TemplateMarker {
                    end: index + 1,
                    complete: true,
                }));
            }
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'+' | b'-')) {
                break;
            }
            index += 1;
        }
        return Ok((index > start + 1).then_some(TemplateMarker {
            end: index,
            complete: false,
        }));
    }
    let tail = bytes.get(start..).unwrap_or_default();
    let prefix = if tail.starts_with(b"${") {
        2
    } else if tail.starts_with(b"$ENV{") {
        5
    } else if tail.starts_with(b"$CACHE{") {
        7
    } else {
        return Ok(None);
    };
    let mut index = start + prefix;
    let mut depth = 1usize;
    while let Some(byte) = bytes.get(index) {
        check_scan(index, cancellation)?;
        if matches!(byte, b'\r' | b'\n') {
            break;
        }
        if *byte == b'\\' && bytes.get(index + 1).is_some() {
            index += 2;
            continue;
        }
        match byte {
            b'{' => depth = depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(Some(TemplateMarker {
                        end: index + 1,
                        complete: true,
                    }));
                }
            }
            _ => {}
        }
        index += 1;
    }
    Ok(Some(TemplateMarker {
        end: index,
        complete: false,
    }))
}

fn complete_cmake_script(bytes: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    let mut index = usize::from(bytes.starts_with(b"\xef\xbb\xbf")) * 3;
    let mut commands = false;
    while let Some(byte) = bytes.get(index).copied() {
        check_scan(index, cancellation)?;
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'#' {
            match bracket_end(bytes, index + 1, cancellation)? {
                BracketEnd::Closed(end) => index = end,
                BracketEnd::Unclosed => return Ok(false),
                BracketEnd::NotBracket => index = line_end(bytes, index, cancellation)?,
            }
            continue;
        }
        if byte == b'@'
            && let Some(marker) = template_marker(bytes, index, cancellation)?
            && marker.complete
        {
            index = marker.end;
            continue;
        }
        if !(byte.is_ascii_alphabetic() || byte == b'_') {
            return Ok(false);
        }
        let mut open = index + 1;
        while bytes
            .get(open)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            check_scan(open, cancellation)?;
            open += 1;
        }
        while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
            check_scan(open, cancellation)?;
            open += 1;
        }
        if bytes.get(open) != Some(&b'(') {
            return Ok(false);
        }
        let Some(end) = command_end(bytes, open, cancellation)? else {
            return Ok(false);
        };
        commands = true;
        index = end;
    }
    cancellation.check()?;
    Ok(commands)
}

fn walk_directive(
    bytes: &[u8],
    mut index: usize,
    end: usize,
    cancellation: &Cancellation,
    mut visit: impl FnMut(usize, usize, bool) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    let mut block = false;
    let mut quote = None;
    let mut line_comment = false;
    let mut marker_retry_at = 0usize;
    while index < end {
        check_scan(index, cancellation)?;
        if index >= marker_retry_at
            && let Some(marker) = template_marker(bytes, index, cancellation)?
            && marker.end <= end
        {
            let mask = !block && !line_comment && quote.is_none();
            if marker.complete || mask {
                visit(index, marker.end, mask)?;
                index = marker.end;
                continue;
            }
            marker_retry_at = marker.end;
        }
        let byte = bytes[index];
        if block {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                block = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if line_comment {
            index += 1;
            continue;
        }
        if let Some(delimiter) = quote {
            if byte == b'\\'
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| !matches!(next, b'@' | b'$'))
            {
                index += 2;
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            block = true;
            index += 2;
            continue;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            line_comment = true;
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        }
        index += 1;
    }
    Ok(block)
}

fn check_scan(index: usize, cancellation: &Cancellation) -> Result<(), AdapterError> {
    if index % CANCELLATION_BYTE_INTERVAL <= 2 {
        cancellation.check()?;
    }
    Ok(())
}

impl TreeSitterProvider {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn extract_cmake_header(
        &self,
        request: &ParseRequest<'_>,
        source: &NativeSourceView<'_>,
        traversal: &mut TraversalReport,
        candidates: &mut Vec<QueryCandidate>,
        max_facts: Option<usize>,
        cancellation: &Cancellation,
    ) -> Result<EmbeddedExtractionLimits, AdapterError> {
        let Some(plan) = HeaderPlan::recognize(
            request.source().path().as_str(),
            source,
            request.limits(),
            cancellation,
        )?
        else {
            if let Some(plan) = cmake_command_template::CommandTemplatePlan::recognize(
                source,
                request.limits(),
                cancellation,
            )? {
                let required = traversal
                    .processed_nodes
                    .checked_add(plan.required_candidate_count())
                    .ok_or(SinkError::AccountingOverflow)?;
                require_provider_limit(
                    ResourceKind::SyntaxNodes,
                    required,
                    request
                        .limits()
                        .max_syntax_nodes()
                        .min(self.config.max_syntax_nodes()),
                )?;
                traversal.processed_nodes = required;
                traversal.max_depth = traversal.max_depth.max(1);
                plan.append_gaps(candidates, cancellation)?;
            }
            return Ok(EmbeddedExtractionLimits::default());
        };
        let mut limited = EmbeddedExtractionLimits::default();
        plan.append_gaps(candidates, cancellation)?;
        try_reserve_cancellable(
            candidates,
            2,
            cancellation,
            "cmake-header-module-allocation",
        )?;
        candidates.push(QueryCandidate {
            start: 0,
            end: source.as_bytes().len(),
            role: StructuralRole::Module,
            syntax: "c.file",
            required: true,
            native_depth: 1,
        });
        candidates.push(QueryCandidate {
            start: 0,
            end: source.as_bytes().len(),
            role: StructuralRole::Expression,
            syntax: "c.header_semantics",
            required: false,
            native_depth: 1,
        });
        let remaining = request
            .limits()
            .max_syntax_nodes()
            .saturating_sub(traversal.processed_nodes);
        if remaining == 0 || request.limits().max_embedded_ranges() == 0 {
            try_reserve_cancellable(candidates, 1, cancellation, "cmake-header-limit-allocation")?;
            candidates.push(QueryCandidate {
                start: 0,
                end: source.as_bytes().len(),
                role: StructuralRole::Signature,
                syntax: "cmake.header_limit",
                required: true,
                native_depth: 1,
            });
            limited.context = true;
            return Ok(limited);
        }
        let limits = super::embedded::embedded_limits(request.limits(), remaining)?;
        let child_request = ParseRequest::new(
            request.source().clone(),
            LanguageId::new("c").map_err(|_| provider_failure("cmake-header-language"))?,
            request.encoding().clone(),
            Vec::new(),
            &limits,
        )?;
        let bytes = plan.parse_bytes(
            source,
            false,
            request.limits().max_reported_memory_bytes(),
            cancellation,
        )?;
        let parsed = match self.parse_native_tree_with_input(
            &child_request,
            &language_for(GrammarFamily::C),
            None,
            self.config.default_settings(),
            source,
            None,
            Some(&bytes),
            plan.staged_owned_bytes(bytes.capacity())?,
            cancellation,
        ) {
            Ok(parsed) => parsed,
            Err(AdapterError::ProviderFailed { code })
                if matches!(
                    code.as_str(),
                    "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
                ) =>
            {
                cancellation.check()?;
                try_reserve_cancellable(
                    candidates,
                    1,
                    cancellation,
                    "cmake-header-limit-allocation",
                )?;
                candidates.push(QueryCandidate {
                    start: 0,
                    end: source.as_bytes().len(),
                    role: StructuralRole::Signature,
                    syntax: "cmake.header_limit",
                    required: true,
                    native_depth: 1,
                });
                limited.context = true;
                return Ok(limited);
            }
            Err(error) => return Err(error),
        };
        let child = inspect_parsed_tree(
            &parsed,
            &child_request,
            source,
            remaining,
            limits.max_syntax_depth(),
            cancellation,
        )?;
        let tree = parsed.tree;
        traversal.processed_nodes = traversal
            .processed_nodes
            .checked_add(child.processed_nodes)
            .ok_or_else(|| provider_failure("cmake-header-node-accounting"))?;
        traversal.max_depth = traversal.max_depth.max(child.max_depth);
        if parsed.projection_unavailable {
            traversal.coverage = CoverageStatus::Unknown;
            traversal.skipped_regions = traversal
                .skipped_regions
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?;
            try_reserve_cancellable(
                candidates,
                1,
                cancellation,
                "cmake-header-unavailable-allocation",
            )?;
            candidates.push(QueryCandidate {
                start: 0,
                end: source.as_bytes().len(),
                role: StructuralRole::Signature,
                syntax: "c.preprocessor_projection_unavailable",
                required: true,
                native_depth: 1,
            });
            return Ok(limited);
        }
        if let Some(diagnostic) = child.primary_diagnostic {
            traversal.primary_diagnostic.get_or_insert(diagnostic);
            traversal.coverage = if matches!(
                diagnostic,
                PrimaryDiagnostic::ErrorRecovery { .. }
                    | PrimaryDiagnostic::CProjectionUnavailable { .. }
            ) {
                CoverageStatus::Unknown
            } else {
                CoverageStatus::Bounded
            };
            traversal.skipped_regions = traversal
                .skipped_regions
                .checked_add(1)
                .ok_or_else(|| provider_failure("cmake-header-gap-accounting"))?;
        }
        if !child.fully_traversed {
            limited.context = true;
            try_reserve_cancellable(candidates, 1, cancellation, "cmake-header-limit-allocation")?;
            candidates.push(QueryCandidate {
                start: 0,
                end: source.as_bytes().len(),
                role: StructuralRole::Signature,
                syntax: "cmake.header_limit",
                required: true,
                native_depth: 1,
            });
            return Ok(limited);
        }
        let pack = self
            .query_packs
            .get_for_native(GrammarFamily::C, GrammarFamily::C, cancellation)
            .map_err(|error| error.into_adapter_error("cmake-header-query-pack"))?;
        let mut captures = if let Some(maximum) = max_facts {
            let extraction = pack.extract(
                GrammarFamily::C,
                &tree,
                source.as_bytes(),
                remaining,
                maximum.saturating_sub(candidates.len()),
                cancellation,
            )?;
            limited.partition = extraction.limit.is_some();
            extraction.candidates
        } else {
            pack.extract_identity(
                GrammarFamily::C,
                &tree,
                source.as_bytes(),
                remaining,
                cancellation,
            )?
        };
        append_written_declarations(&tree, &mut captures, cancellation)?;
        let mut unavailable = Vec::<(usize, usize)>::new();
        for capture in &captures {
            cancellation.check()?;
            if capture.role == StructuralRole::Definition
                && plan.overlaps_mask(capture.start..capture.end)
            {
                let mut node = tree
                    .root_node()
                    .named_descendant_for_byte_range(capture.start, capture.end);
                while let Some(current) = node {
                    cancellation.check()?;
                    if matches!(
                        current.kind(),
                        "function_definition"
                            | "declaration"
                            | "field_declaration"
                            | "struct_specifier"
                            | "union_specifier"
                            | "enum_specifier"
                            | "type_definition"
                            | "preproc_def"
                            | "preproc_function_def"
                    ) {
                        unavailable
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        unavailable.push((current.start_byte(), current.end_byte()));
                        break;
                    }
                    node = current.parent();
                }
            }
        }
        sort_cancellable_by(&mut unavailable, cancellation, |left, right| {
            left.cmp(right)
        })?;
        let mut kept = 0usize;
        for index in 0..unavailable.len() {
            cancellation.check()?;
            let current = unavailable[index];
            if kept > 0 && unavailable[kept - 1].1 >= current.0 {
                unavailable[kept - 1].1 = unavailable[kept - 1].1.max(current.1);
            } else {
                unavailable[kept] = current;
                kept += 1;
            }
        }
        unavailable.truncate(kept);
        let mut retained = Vec::new();
        try_reserve_cancellable(
            &mut retained,
            captures.len().saturating_add(2),
            cancellation,
            "cmake-header-captures-allocation",
        )?;
        for capture in captures.drain(..) {
            cancellation.check()?;
            if capture.role == StructuralRole::Root {
                continue;
            }
            let lexical = matches!(
                capture.role,
                StructuralRole::Comment
                    | StructuralRole::Documentation
                    | StructuralRole::StringLiteral
            );
            if !lexical
                && (plan.overlaps_mask(capture.start..capture.end)
                    && matches!(
                        capture.role,
                        StructuralRole::Definition
                            | StructuralRole::Reference
                            | StructuralRole::Call
                            | StructuralRole::CallName
                    )
                    || {
                        let insertion =
                            unavailable.partition_point(|range| range.0 <= capture.start);
                        insertion
                            .checked_sub(1)
                            .is_some_and(|index| capture.end <= unavailable[index].1)
                    })
            {
                continue;
            }
            retained.push(capture);
        }
        try_reserve_cancellable(
            candidates,
            retained.len(),
            cancellation,
            "cmake-header-captures-allocation",
        )?;
        candidates.extend(retained);
        Ok(limited)
    }
}

fn append_written_declarations(
    tree: &Tree,
    captures: &mut Vec<QueryCandidate>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut cursor = tree.walk();
    let mut depth = 0usize;
    loop {
        cancellation.check()?;
        let node = cursor.node();
        if !node.is_missing() && !node.has_error() {
            match node.kind() {
                "declaration" | "field_declaration" => {
                    let mut children = node.walk();
                    for declarator in node.children_by_field_name("declarator", &mut children) {
                        if let Some((name, function)) = declarator_name(declarator, cancellation)? {
                            if function {
                                continue;
                            }
                            let syntax = if node.kind() == "field_declaration" {
                                "c.header_field"
                            } else {
                                "c.header_variable"
                            };
                            append_declaration(
                                captures,
                                declarator.byte_range(),
                                name,
                                syntax,
                                depth.saturating_add(1),
                                cancellation,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            cancellation.check()?;
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
            depth = depth.saturating_sub(1);
        }
    }
}

fn declarator_name<'tree>(
    mut node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<(Node<'tree>, bool)>, AdapterError> {
    let mut function = false;
    loop {
        cancellation.check()?;
        if node.is_missing() || node.has_error() {
            return Ok(None);
        }
        if matches!(
            node.kind(),
            "identifier" | "field_identifier" | "type_identifier"
        ) {
            return Ok(Some((node, function)));
        }
        function |= node.kind() == "function_declarator";
        let Some(child) = node.child_by_field_name("declarator").or_else(|| {
            (node.kind() == "parenthesized_declarator" && node.named_child_count() == 1)
                .then(|| node.named_child(0))
                .flatten()
        }) else {
            return Ok(None);
        };
        node = child;
    }
}

fn append_declaration(
    captures: &mut Vec<QueryCandidate>,
    span: Range<usize>,
    name: Node<'_>,
    syntax: &'static str,
    depth: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    try_reserve_cancellable(
        captures,
        3,
        cancellation,
        "cmake-header-written-declarations-allocation",
    )?;
    captures.push(QueryCandidate {
        start: span.start,
        end: span.end,
        role: StructuralRole::Declaration,
        syntax,
        required: true,
        native_depth: depth,
    });
    captures.push(QueryCandidate {
        start: name.start_byte(),
        end: name.end_byte(),
        role: StructuralRole::Definition,
        syntax: "c.identifier",
        required: true,
        native_depth: depth.saturating_add(1),
    });
    captures.push(QueryCandidate {
        start: span.start,
        end: span.end,
        role: StructuralRole::Signature,
        syntax: "c.header_written",
        required: true,
        native_depth: depth.saturating_add(1),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::{BatchThresholds, StreamLimits};
    use rootlight_cancel::CancellationReason;
    use rootlight_ir::IrLimits;

    use super::*;
    use crate::{ReuseInvalidation, ReuseStatus, SourceEdit};
    use rootlight_ids::content_hash;

    fn limits(memory: usize, nodes: usize) -> AnalysisLimits {
        let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
        let stream =
            StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch).unwrap();
        AnalysisLimits::new(
            1024 * 1024,
            nodes,
            128,
            16,
            memory,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap()
    }

    fn view(source: &[u8]) -> NativeSourceView<'_> {
        NativeSourceView::new(
            source,
            "utf-8",
            source.len(),
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn recognition_reads_complete_source_and_rejects_script_and_diff_shapes() {
        let budget = limits(1024 * 1024, 100_000);
        let source = format!(
            "#cmakedefine FEATURE\n/* {} */\nset(late value)\n",
            "x".repeat(65_536)
        );
        for text in [
            source.as_str(),
            "--- a/config.h\n+++ b/config.h\n@@ -1 +1 @@\n-#cmakedefine OLD\n+#cmakedefine NEW\n",
            "#[=[\nstruct Pretend {};\n]=]\n",
        ] {
            assert!(
                HeaderPlan::recognize(
                    "config.h.cmake",
                    &view(text.as_bytes()),
                    &budget,
                    &Cancellation::new()
                )
                .unwrap()
                .is_none()
            );
        }
        assert!(
            HeaderPlan::recognize(
                "ordinary.cmake",
                &view(b"#cmakedefine FEATURE\nstruct Written {};\n"),
                &budget,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn masks_preserve_length_newlines_literals_and_source_authority() {
        let source = "/* λ😀 @COMMENT@ */\r\n#cmakedefine01 FEATURE @VALUE@\r\nconst char *s = \"@STRING@\";\r\nstruct Generated_@NAME@ {};\r\n";
        let original = view(source.as_bytes());
        let plan = HeaderPlan::recognize(
            "config.h.cmake",
            &original,
            &limits(1024 * 1024, 1000),
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        let mask = plan
            .parse_bytes(&original, false, usize::MAX, &Cancellation::new())
            .unwrap();
        assert_eq!(mask.len(), source.len());
        assert!(
            std::str::from_utf8(&mask)
                .unwrap()
                .contains("/* λ😀 @COMMENT@ */")
        );
        assert!(std::str::from_utf8(&mask).unwrap().contains("\"@STRING@\""));
        assert!(!std::str::from_utf8(&mask).unwrap().contains("cmakedefine"));
        assert!(
            std::str::from_utf8(&mask)
                .unwrap()
                .contains("Generated_xxxxxx")
        );
        for (index, byte) in source
            .bytes()
            .enumerate()
            .filter(|(_, byte)| matches!(byte, b'\r' | b'\n'))
        {
            assert_eq!(mask[index], byte);
        }
        assert_eq!(original.as_bytes(), source.as_bytes());
        let name = source.find("@NAME@").unwrap();
        assert!(plan.overlaps_mask(name..name + 6));
        assert!(
            !plan.overlaps_mask(
                source.find("@STRING@").unwrap()..source.find("@STRING@").unwrap() + 8
            )
        );
    }

    #[test]
    fn quoted_crlf_splices_preserve_authority_without_template_facts() {
        let text = "#cmakedefine FEATURE\nconst char *s = \"first\\\r\nset(value) ${NAME}\";\n";
        let source = view(text.as_bytes());
        let cancellation = Cancellation::new();
        let plan = HeaderPlan::recognize(
            "config.h.cmake",
            &source,
            &limits(1024 * 1024, 100),
            &cancellation,
        )
        .unwrap()
        .unwrap();
        let projected = plan
            .parse_bytes(&source, false, usize::MAX, &cancellation)
            .unwrap();
        let splice = text.find("\\\r\n").unwrap();
        assert_eq!(&projected[splice..splice + 3], b" \\\n");
        assert_eq!(projected.len(), text.len());
        assert!(!plan.overlaps_mask(splice..splice + 3));
        assert_eq!(source.as_bytes(), text.as_bytes());
        let mut gaps = Vec::new();
        plan.append_gaps(&mut gaps, &cancellation).unwrap();
        assert_eq!(gaps.len(), 2);
        assert!(gaps.iter().all(|gap| gap.syntax == "cmake.header_template"));
        assert!(
            gaps.iter()
                .all(|gap| !(gap.start == splice && gap.end == splice + 3))
        );
        let bounded_source = view(b"#cmakedefine FEATURE\nconst char *s = \"x\\\r\ny\";\n");
        assert!(matches!(
            HeaderPlan::recognize(
                "config.h.cmake",
                &bounded_source,
                &limits(1024 * 1024, 1),
                &cancellation,
            ),
            Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                resource: ResourceKind::SyntaxNodes,
                ..
            }))
        ));
    }

    #[test]
    fn projection_and_marker_exhaustion_are_typed_and_leave_source_unchanged() {
        let source = view(b"#cmakedefine FEATURE\nstruct Written {};\n");
        let plan = HeaderPlan::recognize(
            "config.h.cmake",
            &source,
            &limits(1024 * 1024, 100),
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        let required =
            plan.markers.capacity() * std::mem::size_of::<Marker>() + source.as_bytes().len();
        assert!(
            plan.parse_bytes(&source, false, required, &Cancellation::new())
                .is_ok()
        );
        assert!(matches!(
            plan.parse_bytes(&source, false, required - 1, &Cancellation::new()),
            Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                resource: ResourceKind::ReportedMemoryBytes,
                ..
            }))
        ));
        let many = view(b"#cmakedefine FEATURE @ONE@ @TWO@\n");
        assert!(matches!(
            HeaderPlan::recognize(
                "config.h.cmake",
                &many,
                &limits(1024 * 1024, 2),
                &Cancellation::new()
            ),
            Err(AdapterError::RejectedRequest(RequestError::ProviderLimit {
                resource: ResourceKind::SyntaxNodes,
                ..
            }))
        ));
        assert_eq!(
            source.as_bytes(),
            b"#cmakedefine FEATURE\nstruct Written {};\n"
        );
    }

    #[test]
    fn cancellation_precedes_recognition_and_mask_allocation() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            HeaderPlan::recognize(
                "config.h.cmake",
                &view(b""),
                &limits(1024, 10),
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        let source = view(b"#cmakedefine FEATURE\n");
        let plan = HeaderPlan::recognize(
            "config.h.cmake",
            &source,
            &limits(1024, 10),
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            plan.parse_bytes(&source, false, usize::MAX, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn cmake_noop_reuses_but_physical_template_and_patch_edits_invalidate() {
        for source in [
            "set(written value)\n",
            "#cmakedefine FEATURE\nstruct Written {};\n",
            "--- a/file.cmake\n+++ b/file.cmake\n@@ -1 +1 @@\n-set(old value)\n+set(new value)\n",
        ] {
            let original = view(source.as_bytes());
            let patch = super::super::cmake_patch::CmakePatchInput::new(
                original.as_bytes(),
                1000,
                usize::MAX,
                &Cancellation::new(),
            )
            .unwrap();
            let header = if patch.is_none() {
                HeaderPlan::recognize(
                    "config.h.cmake",
                    &original,
                    &limits(1024 * 1024, 1000),
                    &Cancellation::new(),
                )
                .unwrap()
            } else {
                None
            };
            let header_bytes = header.as_ref().map(|plan| {
                plan.parse_bytes(&original, true, usize::MAX, &Cancellation::new())
                    .unwrap()
            });
            let input = patch
                .as_ref()
                .map(super::super::cmake_patch::CmakePatchInput::parse_bytes)
                .or(header_bytes.as_deref())
                .unwrap_or(original.as_bytes());
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&language_for(GrammarFamily::CMake))
                .unwrap();
            let tree = parser.parse(input, None).unwrap();
            let mut identity = invalid_identity();
            identity.family = GrammarFamily::CMake;
            identity.encoding = "utf-8".into();
            identity.content_hash = content_hash(source.as_bytes());
            let cached = CachedParse {
                entry_id: 1,
                projection_policy: NATIVE_SOURCE_PROJECTION_POLICY,
                c_preprocessor_projection_policy: C_PREPROCESSOR_PROJECTION_POLICY,
                fortran_projection_policy: FORTRAN_TEMPLATE_PROJECTION_POLICY,
                cmake_header_projection_policy: CMAKE_HEADER_PROJECTION_POLICY,
                cmake_patch_projection_policy: super::super::CMAKE_PATCH_PROJECTION_POLICY,
                cmake_command_template_projection_policy:
                    super::super::CMAKE_COMMAND_TEMPLATE_PROJECTION_POLICY,
                identity: identity.clone(),
                source: Arc::new(source.as_bytes().to_vec()),
                tree: Some(tree),
                accounted_bytes: source.len(),
                invalidation: None,
            };
            let (reused, status) = prepare_reuse(
                Some(&cached),
                &identity,
                source.as_bytes(),
                &[],
                usize::MAX,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(reused.is_some());
            assert!(matches!(status, ReuseStatus::Reused { .. }));
            let changed = format!("# marker\n{source}");
            identity.content_hash = content_hash(changed.as_bytes());
            let (reused, status) = prepare_reuse(
                Some(&cached),
                &identity,
                changed.as_bytes(),
                &[SourceEdit::new(0, 0, "# marker\n").unwrap()],
                usize::MAX,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(reused.is_none());
            assert_eq!(
                status,
                ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
            );
        }
    }
}
