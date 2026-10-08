//! Lossless text presentation over exact generation-bound source chunks.
//!
//! Raw bytes remain the editing authority. Only bounded UTF-16 selections own
//! a decoded presentation; UTF-8 borrows its already verified chunk bytes.

use std::{collections::VecDeque, fmt};

use rootlight_cancel::Cancellation;
use rootlight_ids::GenerationId;
use rootlight_ir::{FileRecord, SourceRef};
use rootlight_vfs::{SourceText, TextEncoding};

use super::{
    ExpandedRange, SourceBudget, SourceChunk, SourceControl, SourceError, SourceReadMode,
    SourceReadOptions, SourceService, ensure_utf8, push_back_preallocated, push_preallocated,
    try_reserve_vec,
};

/// Context controls for verified Unicode source presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LosslessTextReadOptions {
    /// Whole lines included before the selected physical span.
    pub context_lines_before: u16,
    /// Whole lines included after the selected physical span.
    pub context_lines_after: u16,
}

impl LosslessTextReadOptions {
    /// Creates a text read with two context lines on each side.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_lines_before: 2,
            context_lines_after: 2,
        }
    }

    /// Replaces the leading context count.
    #[must_use]
    pub const fn with_context_lines_before(mut self, lines: u16) -> Self {
        self.context_lines_before = lines;
        self
    }

    /// Replaces the trailing context count.
    #[must_use]
    pub const fn with_context_lines_after(mut self, lines: u16) -> Self {
        self.context_lines_after = lines;
        self
    }
}

impl Default for LosslessTextReadOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact original bytes with a validated, unnormalized Unicode presentation.
///
/// The selected BOM is preserved as U+FEFF. Physical offsets and hashes always
/// address the original file; UTF-8 presentation positions are not source offsets.
#[derive(PartialEq, Eq)]
pub struct SourceTextChunk {
    raw: SourceChunk,
    encoding: TextEncoding,
    presentation: Option<String>,
}

impl SourceTextChunk {
    /// Borrows the exact original-byte chunk and its immutable source identity.
    #[must_use]
    pub const fn raw(&self) -> &SourceChunk {
        &self.raw
    }

    /// Borrows the lossless text without copying UTF-8 chunk bytes.
    ///
    /// # Panics
    /// Panics only if private construction violates its validated UTF-8 invariant.
    #[must_use]
    pub fn text(&self) -> &str {
        self.presentation.as_deref().unwrap_or_else(|| {
            std::str::from_utf8(&self.raw.bytes)
                .expect("private text chunk construction validates UTF-8 bytes")
        })
    }

    /// Returns the verified physical encoding of the complete source file.
    #[must_use]
    pub const fn physical_encoding(&self) -> TextEncoding {
        self.encoding
    }

    /// Returns the owned decoded buffer capacity, excluding original bytes.
    #[must_use]
    pub fn owned_presentation_bytes(&self) -> usize {
        self.presentation.as_ref().map_or(0, String::capacity)
    }

    /// Moves exact bytes, physical encoding and optional presentation without copying.
    ///
    /// The presentation is absent only for UTF-8. A present UTF-16 presentation
    /// re-encodes exactly to the original selected bytes in the returned byte order.
    #[must_use]
    pub fn into_parts(self) -> (SourceChunk, TextEncoding, Option<String>) {
        (self.raw, self.encoding, self.presentation)
    }
}

impl fmt::Debug for SourceTextChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTextChunk")
            .field("raw", &self.raw)
            .field("physical_encoding", &self.encoding)
            .field("owned_presentation_bytes", &self.owned_presentation_bytes())
            .finish_non_exhaustive()
    }
}

/// Verified text chunks and separately conserved physical/presentation costs.
#[derive(PartialEq, Eq)]
pub struct SourceTextReadResult {
    generation: GenerationId,
    chunks: Vec<SourceTextChunk>,
    total_source_bytes: usize,
    total_metadata_bytes: usize,
    total_text_bytes: usize,
    total_response_memory_bytes: usize,
}

impl SourceTextReadResult {
    /// Returns the immutable generation that served the read.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Borrows chunks in the original selector order.
    #[must_use]
    pub fn chunks(&self) -> &[SourceTextChunk] {
        &self.chunks
    }

    /// Returns aggregate original selected byte lengths, including duplicates.
    #[must_use]
    pub const fn total_source_bytes(&self) -> usize {
        self.total_source_bytes
    }

    /// Returns aggregate path and language byte lengths.
    #[must_use]
    pub const fn total_metadata_bytes(&self) -> usize {
        self.total_metadata_bytes
    }

    /// Returns aggregate UTF-8 presentation lengths for token/transport accounting.
    #[must_use]
    pub const fn total_text_bytes(&self) -> usize {
        self.total_text_bytes
    }

    /// Returns raw, metadata and owned-presentation buffer capacities.
    ///
    /// Snapshot admission and bounded fixed-size containers are accounted separately.
    #[must_use]
    pub const fn total_response_memory_bytes(&self) -> usize {
        self.total_response_memory_bytes
    }

    /// Moves the validated chunks without copying source or presentation buffers.
    #[must_use]
    pub fn into_chunks(self) -> Vec<SourceTextChunk> {
        self.chunks
    }
}

impl fmt::Debug for SourceTextReadResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTextReadResult")
            .field("generation", &self.generation)
            .field("chunk_count", &self.chunks.len())
            .field("total_source_bytes", &self.total_source_bytes)
            .field("total_metadata_bytes", &self.total_metadata_bytes)
            .field("total_text_bytes", &self.total_text_bytes)
            .field(
                "total_response_memory_bytes",
                &self.total_response_memory_bytes,
            )
            .finish()
    }
}

/// Scalar-bounded text prefixes with separately conserved original-byte costs.
///
/// Each chunk retains the full approved selection in `raw().reference`, while
/// its actual start and end address only the returned prefix of the selected or
/// context-expanded range. An empty prefix is valid when the first scalar
/// exceeds the cap.
#[derive(PartialEq, Eq)]
pub struct SourceTextPrefixReadResult {
    inner: SourceTextReadResult,
    truncated: bool,
}

impl SourceTextPrefixReadResult {
    /// Returns the immutable generation that served the read.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.inner.generation()
    }

    /// Borrows prefixes in the original selector order, including duplicates.
    #[must_use]
    pub fn chunks(&self) -> &[SourceTextChunk] {
        self.inner.chunks()
    }

    /// Reports whether any selected or context-expanded range was reduced.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns aggregate returned original-byte lengths, including duplicates.
    #[must_use]
    pub const fn total_source_bytes(&self) -> usize {
        self.inner.total_source_bytes()
    }

    /// Returns aggregate path and language byte lengths.
    #[must_use]
    pub const fn total_metadata_bytes(&self) -> usize {
        self.inner.total_metadata_bytes()
    }

    /// Returns aggregate UTF-8 presentation lengths, including duplicates.
    #[must_use]
    pub const fn total_text_bytes(&self) -> usize {
        self.inner.total_text_bytes()
    }

    /// Returns raw, metadata and owned-presentation buffer capacities.
    #[must_use]
    pub const fn total_response_memory_bytes(&self) -> usize {
        self.inner.total_response_memory_bytes()
    }

    /// Moves prefixes without copying their source or presentation buffers.
    #[must_use]
    pub fn into_chunks(self) -> Vec<SourceTextChunk> {
        self.inner.into_chunks()
    }

    /// Moves the validated text result and its aggregate prefix truncation flag.
    #[must_use]
    pub fn into_parts(self) -> (SourceTextReadResult, bool) {
        (self.inner, self.truncated)
    }
}

impl fmt::Debug for SourceTextPrefixReadResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceTextPrefixReadResult")
            .field("result", &self.inner)
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl SourceService<'_> {
    /// Reads maximal scalar prefixes of line-context-expanded physical spans.
    ///
    /// The full original reference, whole-file hash and physical encoding remain
    /// authoritative. Both original endpoints are validated before expansion or
    /// clipping. The positive text-byte ceiling applies to the expanded range;
    /// leading context may exhaust it before the approved selection is reached.
    /// Actual byte and line ranges describe only the returned prefix, including
    /// an empty prefix. Truncation also includes omitted trailing context.
    ///
    /// # Errors
    /// Returns `SourceError` for invalid ceilings/budgets/context counts, foreign
    /// or stale references, malformed files or scalar-interior approved endpoints,
    /// cancellation, allocation failure, or aggregate resource exhaustion.
    pub fn read_lossless_text_prefix_with_options(
        &self,
        references: &[SourceRef],
        options: LosslessTextReadOptions,
        maximum_text_bytes_per_chunk: usize,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextPrefixReadResult, SourceError> {
        if maximum_text_bytes_per_chunk == 0
            || maximum_text_bytes_per_chunk > super::HARD_MAX_SOURCE_BYTES
        {
            return Err(SourceError::InvalidBudget);
        }
        let raw = self.read_internal(
            references,
            SourceReadOptions::new()
                .with_context_lines_before(options.context_lines_before)
                .with_context_lines_after(options.context_lines_after),
            budget,
            cancellation,
            SourceReadMode::LosslessTextContextPrefix(maximum_text_bytes_per_chunk),
        )?;
        let truncated = raw.truncated;
        let inner = present_text(raw, budget, cancellation)?;
        Ok(SourceTextPrefixReadResult { inner, truncated })
    }

    /// Reads exact physical spans as lossless UTF-8 or BOM-backed UTF-16 text.
    ///
    /// Whole-file identity and encoding are verified once per distinct file.
    /// Context and selectors stay in original-byte coordinates. UTF-16 decoding
    /// allocates only selected presentations under the existing response budget;
    /// the same deadline covers verification, copying and presentation.
    ///
    /// # Errors
    /// Returns `SourceError` for foreign/stale references, unsupported or malformed
    /// whole-file encoding, scalar-interior spans, bounds, allocation or cancellation.
    pub fn read_lossless_text(
        &self,
        references: &[SourceRef],
        options: LosslessTextReadOptions,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextReadResult, SourceError> {
        let raw = self.read_internal(
            references,
            SourceReadOptions::new()
                .with_context_lines_before(options.context_lines_before)
                .with_context_lines_after(options.context_lines_after),
            budget,
            cancellation,
            SourceReadMode::LosslessText,
        )?;
        present_text(raw, budget, cancellation)
    }

    /// Reads maximal scalar prefixes under a per-chunk UTF-8 text-byte ceiling.
    ///
    /// The positive ceiling cannot exceed the existing source hard byte limit.
    /// No context is added. Whole-file encoding and identity, and both approved
    /// selection endpoints, are checked even when only a shorter prefix fits.
    /// The full selection remains the chunk reference; actual ranges and totals
    /// describe only returned bytes. Existing per-encoding NUL policy is retained.
    ///
    /// # Errors
    /// Returns `SourceError` for invalid ceilings/budgets, foreign or stale
    /// references, malformed files or scalar-interior approved endpoints,
    /// cancellation, allocation failure, or aggregate returned-byte exhaustion.
    pub fn read_lossless_text_prefix(
        &self,
        references: &[SourceRef],
        maximum_text_bytes_per_chunk: usize,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextPrefixReadResult, SourceError> {
        if maximum_text_bytes_per_chunk == 0
            || maximum_text_bytes_per_chunk > super::HARD_MAX_SOURCE_BYTES
        {
            return Err(SourceError::InvalidBudget);
        }
        let raw = self.read_internal(
            references,
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            budget,
            cancellation,
            SourceReadMode::LosslessTextPrefix(maximum_text_bytes_per_chunk),
        )?;
        let truncated = raw.truncated;
        let inner = present_text(raw, budget, cancellation)?;
        Ok(SourceTextPrefixReadResult { inner, truncated })
    }
}

fn present_text(
    raw: super::RawSourceRead,
    budget: SourceBudget,
    cancellation: &Cancellation,
) -> Result<SourceTextReadResult, SourceError> {
    let control = SourceControl {
        cancellation,
        deadline: raw.deadline,
        #[cfg(test)]
        after_operation: None,
    };
    control.check()?;
    if raw.result.chunks.len() != raw.text_encodings.len() {
        return Err(SourceError::ReadFailed);
    }
    let mut chunks = Vec::new();
    try_reserve_vec(&mut chunks, raw.result.chunks.len(), &control)?;
    let mut memory = raw.result.total_response_memory_bytes;
    let mut text_bytes = 0usize;
    for (raw, encoding) in raw.result.chunks.into_iter().zip(raw.text_encodings) {
        control.check()?;
        let presentation = if encoding == TextEncoding::Utf8 {
            None
        } else {
            let remaining = budget
                .max_response_memory_bytes
                .checked_sub(memory)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            let text = render_fragment(&raw.bytes, encoding, remaining, &control)?;
            memory = memory
                .checked_add(text.capacity())
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            Some(text)
        };
        text_bytes = text_bytes
            .checked_add(presentation.as_ref().map_or(raw.bytes.len(), String::len))
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        push_preallocated(
            &mut chunks,
            SourceTextChunk {
                raw,
                encoding,
                presentation,
            },
            &control,
        )?;
    }
    control.check()?;
    Ok(SourceTextReadResult {
        generation: raw.result.generation,
        chunks,
        total_source_bytes: raw.result.total_source_bytes,
        total_metadata_bytes: raw.result.total_metadata_bytes,
        total_text_bytes: text_bytes,
        total_response_memory_bytes: memory,
    })
}

pub(super) fn physical_encoding(file: &FileRecord) -> Result<TextEncoding, SourceError> {
    match file.encoding.as_str() {
        "utf-8" | "utf8" => Ok(TextEncoding::Utf8),
        "utf-16le" => Ok(TextEncoding::Utf16Le),
        "utf-16be" => Ok(TextEncoding::Utf16Be),
        _ => Err(SourceError::EncodingUnsupported),
    }
}

pub(super) fn validate_snapshot(
    file: &FileRecord,
    bytes: &[u8],
    control: &SourceControl<'_>,
) -> Result<TextEncoding, SourceError> {
    let encoding = physical_encoding(file)?;
    if encoding == TextEncoding::Utf8 {
        ensure_utf8(file, bytes, control)?;
    } else {
        let text = SourceText::parse_with_check(bytes, || control.check())?
            .ok_or(SourceError::EncodingUnsupported)?;
        if text.encoding() != encoding {
            return Err(SourceError::EncodingUnsupported);
        }
    }
    Ok(encoding)
}

fn unit_at(bytes: &[u8], offset: usize, encoding: TextEncoding) -> Result<u16, SourceError> {
    let end = offset
        .checked_add(2)
        .ok_or(SourceError::InvalidSourceSpan)?;
    let pair: [u8; 2] = bytes
        .get(offset..end)
        .and_then(|pair| pair.try_into().ok())
        .ok_or(SourceError::InvalidSourceSpan)?;
    match encoding {
        TextEncoding::Utf16Le => Ok(u16::from_le_bytes(pair)),
        TextEncoding::Utf16Be => Ok(u16::from_be_bytes(pair)),
        TextEncoding::Utf8 => Err(SourceError::EncodingUnsupported),
    }
}

fn validate_boundary(
    bytes: &[u8],
    boundary: usize,
    encoding: TextEncoding,
) -> Result<(), SourceError> {
    if boundary > bytes.len() || !boundary.is_multiple_of(2) {
        return Err(SourceError::InvalidSourceSpan);
    }
    if boundary != 0 && boundary != bytes.len() {
        let previous = unit_at(bytes, boundary - 2, encoding)?;
        let next = unit_at(bytes, boundary, encoding)?;
        if (0xd800..=0xdbff).contains(&previous) && (0xdc00..=0xdfff).contains(&next) {
            return Err(SourceError::InvalidSourceSpan);
        }
    }
    Ok(())
}

fn visit_fragment(
    bytes: &[u8],
    encoding: TextEncoding,
    control: &SourceControl<'_>,
    mut visit: impl FnMut(usize, usize, char) -> Result<bool, SourceError>,
) -> Result<(), SourceError> {
    control.check()?;
    let decode: fn([u8; 2]) -> u16 = match encoding {
        TextEncoding::Utf16Le => u16::from_le_bytes,
        TextEncoding::Utf16Be => u16::from_be_bytes,
        TextEncoding::Utf8 => return Err(SourceError::EncodingUnsupported),
    };
    let (units, tail) = bytes.as_chunks::<2>();
    if !tail.is_empty() {
        return Err(SourceError::InvalidSourceSpan);
    }
    let mut offset = 0usize;
    for (index, decoded) in char::decode_utf16(units.iter().copied().map(decode)).enumerate() {
        if index.is_multiple_of(1024) {
            control.check()?;
        }
        let character = decoded.map_err(|_| SourceError::InvalidSourceSpan)?;
        if character == '\0' {
            return Err(SourceError::EncodingUnsupported);
        }
        let end = offset
            .checked_add(encoding.character_bytes(character))
            .ok_or(SourceError::InvalidSourceSpan)?;
        if visit(offset, end, character)? {
            return control.check();
        }
        offset = end;
    }
    if offset != bytes.len() {
        return Err(SourceError::InvalidSourceSpan);
    }
    control.check()
}

pub(super) struct TextPrefixRange {
    pub(super) range: ExpandedRange,
    pub(super) text_bytes: usize,
    pub(super) truncated: bool,
}

pub(super) fn bounded_text_prefix(
    bytes: &[u8],
    reference: &SourceRef,
    maximum_text_bytes: usize,
    encoding: TextEncoding,
    control: &SourceControl<'_>,
) -> Result<TextPrefixRange, SourceError> {
    control.check()?;
    // The shared pipeline has already validated the complete snapshot.
    // Both approved endpoints still precede any early scalar clipping.
    let start = usize::try_from(reference.span().start_byte())
        .map_err(|_| SourceError::InvalidSourceSpan)?;
    let approved_end =
        usize::try_from(reference.span().end_byte()).map_err(|_| SourceError::InvalidSourceSpan)?;
    bounded_text_range_prefix(
        bytes,
        ExpandedRange {
            start,
            end: approved_end,
            start_line: None,
            end_line: None,
        },
        maximum_text_bytes,
        encoding,
        control,
    )
}

pub(super) fn bounded_text_context_prefix(
    bytes: &[u8],
    reference: &SourceRef,
    options: SourceReadOptions,
    maximum_text_bytes: usize,
    encoding: TextEncoding,
    control: &SourceControl<'_>,
) -> Result<TextPrefixRange, SourceError> {
    let range = expand_text_context(bytes, reference, options, encoding, control)?;
    bounded_text_range_prefix(bytes, range, maximum_text_bytes, encoding, control)
}

fn bounded_text_range_prefix(
    bytes: &[u8],
    range: ExpandedRange,
    maximum_text_bytes: usize,
    encoding: TextEncoding,
    control: &SourceControl<'_>,
) -> Result<TextPrefixRange, SourceError> {
    control.check()?;
    let start = range.start;
    let approved_end = range.end;
    let selected = bytes
        .get(start..approved_end)
        .ok_or(SourceError::InvalidSourceSpan)?;
    let (end, text_bytes) = if encoding == TextEncoding::Utf8 {
        for boundary in [start, approved_end] {
            if bytes.get(boundary).is_some_and(|byte| byte & 0xc0 == 0x80) {
                return Err(SourceError::InvalidSourceSpan);
            }
        }
        let mut end = start
            .checked_add(selected.len().min(maximum_text_bytes))
            .ok_or(SourceError::InvalidSourceSpan)?;
        while end > start && bytes.get(end).is_some_and(|byte| byte & 0xc0 == 0x80) {
            end -= 1;
        }
        (end, end - start)
    } else {
        validate_boundary(bytes, start, encoding)?;
        validate_boundary(bytes, approved_end, encoding)?;
        let mut physical_bytes = 0usize;
        let mut text_bytes = 0usize;
        visit_fragment(selected, encoding, control, |_, end, character| {
            let next = text_bytes
                .checked_add(character.len_utf8())
                .ok_or(SourceError::SourceBudgetExceeded)?;
            if next > maximum_text_bytes {
                return Ok(true);
            }
            physical_bytes = end;
            text_bytes = next;
            Ok(false)
        })?;
        (
            start
                .checked_add(physical_bytes)
                .ok_or(SourceError::InvalidSourceSpan)?,
            text_bytes,
        )
    };
    control.check()?;
    let end_line = if end == range.end {
        range.end_line
    } else {
        range
            .start_line
            .map(|line| prefix_end_line(bytes, start, end, encoding, line, control))
            .transpose()?
    };
    Ok(TextPrefixRange {
        range: ExpandedRange {
            start,
            end,
            start_line: range.start_line,
            end_line,
        },
        text_bytes,
        truncated: end < range.end,
    })
}

fn prefix_end_line(
    bytes: &[u8],
    start: usize,
    end: usize,
    encoding: TextEncoding,
    start_line: u64,
    control: &SourceControl<'_>,
) -> Result<u64, SourceError> {
    let prefix = bytes
        .get(start..end)
        .ok_or(SourceError::InvalidSourceSpan)?;
    let (newlines, trailing_newline) = if encoding == TextEncoding::Utf8 {
        (
            super::count_newlines(prefix, control)?,
            prefix.last() == Some(&b'\n'),
        )
    } else {
        let mut newlines = 0usize;
        let mut trailing_newline = false;
        visit_fragment(prefix, encoding, control, |_, _, character| {
            trailing_newline = character == '\n';
            if trailing_newline {
                newlines = newlines
                    .checked_add(1)
                    .ok_or(SourceError::InvalidSourceSpan)?;
            }
            Ok(false)
        })?;
        (newlines, trailing_newline)
    };
    start_line
        .checked_add(
            u64::try_from(newlines.saturating_sub(usize::from(trailing_newline)))
                .map_err(|_| SourceError::InvalidSourceSpan)?,
        )
        .ok_or(SourceError::InvalidSourceSpan)
}

pub(super) fn expand_text_context(
    bytes: &[u8],
    reference: &SourceRef,
    options: SourceReadOptions,
    encoding: TextEncoding,
    control: &SourceControl<'_>,
) -> Result<ExpandedRange, SourceError> {
    if encoding == TextEncoding::Utf8 {
        return super::expand_context(bytes, reference, options, control);
    }
    let selection_start = usize::try_from(reference.span().start_byte())
        .map_err(|_| SourceError::InvalidSourceSpan)?;
    let selection_end =
        usize::try_from(reference.span().end_byte()).map_err(|_| SourceError::InvalidSourceSpan)?;
    if selection_start > selection_end {
        return Err(SourceError::InvalidSourceSpan);
    }
    validate_boundary(bytes, selection_start, encoding)?;
    validate_boundary(bytes, selection_end, encoding)?;
    let mut starts = VecDeque::new();
    let capacity = usize::from(options.context_lines_before) + 1;
    control.controlled(|| {
        starts
            .try_reserve_exact(capacity)
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    push_back_preallocated(&mut starts, (0usize, 1u64), control)?;
    let mut line = 1u64;
    let prefix = bytes
        .get(..selection_start)
        .ok_or(SourceError::InvalidSourceSpan)?;
    visit_fragment(prefix, encoding, control, |_, end, character| {
        if character == '\n' {
            line = line.checked_add(1).ok_or(SourceError::InvalidSourceSpan)?;
            if starts.len() == capacity {
                starts.pop_front();
            }
            push_back_preallocated(&mut starts, (end, line), control)?;
        }
        Ok(false)
    })?;
    let (start, start_line) = if options.context_lines_before == 0 {
        (selection_start, line)
    } else {
        starts
            .front()
            .copied()
            .ok_or(SourceError::InvalidSourceSpan)?
    };
    let mut end = selection_end;
    if options.context_lines_after != 0 {
        let incomplete_line = selection_start == selection_end
            || selection_end == 0
            || unit_at(bytes, selection_end - 2, encoding)? != u16::from(b'\n');
        let needed = usize::from(options.context_lines_after) + usize::from(incomplete_line);
        let mut found = 0usize;
        let tail = bytes
            .get(selection_end..)
            .ok_or(SourceError::InvalidSourceSpan)?;
        end = bytes.len();
        visit_fragment(tail, encoding, control, |_, relative_end, character| {
            if character == '\n' {
                found += 1;
                if found == needed {
                    end = selection_end
                        .checked_add(relative_end)
                        .ok_or(SourceError::InvalidSourceSpan)?;
                    return Ok(true);
                }
            }
            Ok(false)
        })?;
    }
    let selected = bytes
        .get(start..end)
        .ok_or(SourceError::InvalidSourceSpan)?;
    let mut newlines = 0usize;
    let mut last = None;
    visit_fragment(selected, encoding, control, |_, _, character| {
        if character == '\n' {
            newlines = newlines
                .checked_add(1)
                .ok_or(SourceError::InvalidSourceSpan)?;
        }
        last = Some(character);
        Ok(false)
    })?;
    let lines = newlines
        .saturating_add(1)
        .saturating_sub(usize::from(last == Some('\n')));
    let end_line = start_line
        .checked_add(
            u64::try_from(lines.saturating_sub(1)).map_err(|_| SourceError::InvalidSourceSpan)?,
        )
        .ok_or(SourceError::InvalidSourceSpan)?;
    Ok(ExpandedRange {
        start,
        end,
        start_line: Some(start_line),
        end_line: Some(end_line),
    })
}

fn render_fragment(
    bytes: &[u8],
    encoding: TextEncoding,
    maximum_capacity: usize,
    control: &SourceControl<'_>,
) -> Result<String, SourceError> {
    let mut length = 0usize;
    visit_fragment(bytes, encoding, control, |_, _, character| {
        length = length
            .checked_add(character.len_utf8())
            .filter(|length| *length <= maximum_capacity)
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        Ok(false)
    })?;
    let mut text = String::new();
    control.controlled(|| {
        text.try_reserve_exact(length)
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    if text.capacity() < length || text.capacity() > maximum_capacity {
        return Err(SourceError::ResponseMemoryBudgetExceeded);
    }
    visit_fragment(bytes, encoding, control, |_, _, character| {
        if text
            .len()
            .checked_add(character.len_utf8())
            .is_none_or(|end| end > text.capacity())
        {
            return Err(SourceError::ResponseMemoryBudgetExceeded);
        }
        text.push(character);
        Ok(false)
    })?;
    Ok(text)
}

fn remaining_capacity(
    owned: usize,
    maximum: usize,
    requested: usize,
) -> Result<usize, SourceError> {
    let remaining = maximum
        .checked_sub(owned)
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
    if requested > remaining {
        return Err(SourceError::ResponseMemoryBudgetExceeded);
    }
    Ok(remaining)
}

fn charge_capacity(owned: &mut usize, maximum: usize, capacity: usize) -> Result<(), SourceError> {
    *owned = owned
        .checked_add(capacity)
        .filter(|total| *total <= maximum)
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
    Ok(())
}

pub(super) fn clone_string(
    value: &str,
    owned: &mut usize,
    maximum: usize,
    control: &SourceControl<'_>,
) -> Result<String, SourceError> {
    remaining_capacity(*owned, maximum, value.len())?;
    let mut copied = String::new();
    control.controlled(|| {
        copied
            .try_reserve_exact(value.len())
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    charge_capacity(owned, maximum, copied.capacity())?;
    control.controlled(|| {
        copied.push_str(value);
        Ok(())
    })?;
    Ok(copied)
}

pub(super) fn clone_bytes(
    value: &[u8],
    owned: &mut usize,
    maximum: usize,
    control: &SourceControl<'_>,
) -> Result<Vec<u8>, SourceError> {
    remaining_capacity(*owned, maximum, value.len())?;
    let mut copied = Vec::new();
    control.controlled(|| {
        copied
            .try_reserve_exact(value.len())
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    charge_capacity(owned, maximum, copied.capacity())?;
    control.controlled(|| {
        copied.extend_from_slice(value);
        Ok(())
    })?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    use std::{cell::Cell, time::Duration};

    #[test]
    fn lossless_fragment_admits_exact_decoded_capacity_before_allocation() {
        let cancellation = Cancellation::new();
        let control = SourceControl::new(&cancellation, Duration::from_secs(1));
        let operations = Cell::new(0usize);
        let hook = || operations.set(operations.get() + 1);
        let controlled = control.with_after_operation(&hook);
        let raw = [0xe9, 0x00];
        assert_eq!(
            render_fragment(&raw, TextEncoding::Utf16Le, 1, &controlled),
            Err(SourceError::ResponseMemoryBudgetExceeded)
        );
        assert_eq!(operations.get(), 0);
        assert_eq!(
            render_fragment(&raw, TextEncoding::Utf16Le, 2, &controlled)
                .expect("exact decoded UTF-8 capacity is admitted"),
            "\u{00e9}"
        );
        assert_eq!(operations.get(), 1);
    }

    #[test]
    fn lossless_presentation_cancellation_after_reserve_precedes_success() {
        let cancellation = Cancellation::new();
        let hook = || {
            let _ = cancellation.cancel(CancellationReason::ParentCancelled);
        };
        let control =
            SourceControl::new(&cancellation, Duration::from_secs(1)).with_after_operation(&hook);
        assert_eq!(
            render_fragment(&[0xe9, 0x00], TextEncoding::Utf16Le, 2, &control),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
    }

    #[test]
    fn lossless_raw_copies_admit_and_charge_actual_capacity_without_partial_success() {
        let cancellation = Cancellation::new();
        let control = SourceControl::new(&cancellation, Duration::from_secs(1));
        let mut memory = 2usize;
        assert_eq!(
            clone_string("path", &mut memory, 5, &control),
            Err(SourceError::ResponseMemoryBudgetExceeded)
        );
        assert_eq!(memory, 2);
        let copied = clone_string("path", &mut memory, 6, &control)
            .expect("exact metadata buffer capacity is admitted");
        assert_eq!(memory, 2 + copied.capacity());
        assert_eq!(copied, "path");
        assert_eq!(
            clone_bytes(b"raw", &mut memory, 8, &control),
            Err(SourceError::ResponseMemoryBudgetExceeded)
        );
        let copied = clone_bytes(b"raw", &mut memory, 9, &control)
            .expect("exact original-byte buffer capacity is admitted");
        assert_eq!(memory, 9);
        assert_eq!(copied, b"raw");
    }

    #[test]
    fn lossless_fragment_decoder_rejects_malformed_and_nul_without_lossy_repair() {
        let cancellation = Cancellation::new();
        let control = SourceControl::new(&cancellation, Duration::from_secs(1));
        for bytes in [
            &[0u8][..],
            &[0x00, 0xd8],
            &[0x00, 0xdc],
            &[0x00, 0xd8, 0x58, 0x00],
        ] {
            assert_eq!(
                render_fragment(bytes, TextEncoding::Utf16Le, 32, &control),
                Err(SourceError::InvalidSourceSpan)
            );
        }
        assert_eq!(
            render_fragment(&[0, 0], TextEncoding::Utf16Le, 32, &control),
            Err(SourceError::EncodingUnsupported)
        );
    }
}
