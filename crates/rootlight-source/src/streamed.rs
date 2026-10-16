//! Bounded source responses staged during authenticated generation-owned visits.
//! Provider callbacks are provisional: no response escapes before complete raw
//! identity verification, text validation and request controls have succeeded.

use std::{collections::VecDeque, fmt, mem::size_of};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{FileRecord, SourceRef};
use rootlight_vfs::{StreamingSourceTextValidator, TextEncoding};

use super::{
    PreparedRequest, RawSourceRead, SourceBudget, SourceChunk, SourceControl, SourceEncoding,
    SourceError, SourceReadMode, SourceReadOptions, SourceReadResult, SourceService, SourceTrust,
    lossless_text, try_clone_string, try_reserve_vec,
};

/// Generation-owned raw source capability without caller-controlled paths.
///
/// Implementations retain the matching immutable generation and private storage
/// owner/lease. Every callback is provisional until the complete operation
/// succeeds; consumers must discard all staged state on any error.
pub trait VerifiedSourceBacking: fmt::Debug {
    /// Returns the repository whose retained sources this capability authorizes.
    fn repository(&self) -> RepositoryId;

    /// Returns the immutable generation retained by this capability.
    fn generation(&self) -> GenerationId;

    /// Visits all original bytes in contiguous order and authenticates their identity.
    ///
    /// The provider authorizes the complete `FileRecord` against its own retained
    /// generation. Success requires exact original length/hash and private-file
    /// ownership, link, traversal and race checks where applicable. Text decoding
    /// is a consumer obligation, so unsupported text codecs remain raw-readable.
    /// The visitor receives borrowed bounded chunks starting at offset zero.
    /// Empty sources may have no callbacks. The returned count is the full length.
    ///
    /// # Errors
    /// Returns the original visitor error or a source-redacted authorization,
    /// identity, storage, verification-budget or cancellation failure. No partial
    /// success is permitted, including after the last visitor callback.
    #[expect(
        clippy::type_complexity,
        reason = "the capability directly accepts borrowed physical chunks and preserves the consumer's typed error"
    )]
    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum_verified_bytes: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError>;
}

pub(super) fn visit(
    backing: &dyn VerifiedSourceBacking,
    file: &FileRecord,
    control: &SourceControl<'_>,
    mut consumer: impl FnMut(u64, &[u8]) -> Result<(), SourceError>,
) -> Result<(), SourceError> {
    let mut consumed = 0u64;
    let mut callback_error = None;
    let returned = control.controlled(|| {
        backing.visit_verified(
            file,
            file.byte_length,
            control.cancellation,
            &mut |offset, bytes| {
                if let Some(error) = callback_error {
                    return Err(error);
                }
                let result = (|| {
                    control.check()?;
                    if offset != consumed {
                        return Err(SourceError::StaleSource);
                    }
                    let next = offset
                        .checked_add(
                            u64::try_from(bytes.len()).map_err(|_| SourceError::StaleSource)?,
                        )
                        .filter(|end| *end <= file.byte_length)
                        .ok_or(SourceError::StaleSource)?;
                    consumer(offset, bytes)?;
                    consumed = next;
                    control.check()
                })();
                if let Err(error) = result {
                    callback_error = Some(error);
                }
                result
            },
        )
    });
    if let Some(error) = callback_error {
        control.check()?;
        return Err(error);
    }
    let returned = returned?;
    if returned != file.byte_length || consumed != returned {
        return Err(SourceError::StaleSource);
    }
    control.check()
}

pub(super) struct ScalarDecoder {
    encoding: TextEncoding,
    carry: [u8; 4],
    length: usize,
    start: u64,
}

impl ScalarDecoder {
    pub(super) const fn new(encoding: TextEncoding) -> Self {
        Self {
            encoding,
            carry: [0; 4],
            length: 0,
            start: 0,
        }
    }

    pub(super) fn push(
        &mut self,
        offset: u64,
        bytes: &[u8],
        control: &SourceControl<'_>,
        mut scalar: impl FnMut(u64, u64, char, &[u8]) -> Result<(), SourceError>,
    ) -> Result<(), SourceError> {
        for (index, byte) in bytes.iter().copied().enumerate() {
            if index.is_multiple_of(4096) {
                control.check()?;
            }
            if self.length == 0 {
                self.start = offset
                    .checked_add(u64::try_from(index).map_err(|_| SourceError::InvalidSourceSpan)?)
                    .ok_or(SourceError::InvalidSourceSpan)?;
            }
            *self
                .carry
                .get_mut(self.length)
                .ok_or(SourceError::EncodingUnsupported)? = byte;
            self.length += 1;
            let required = match self.encoding {
                TextEncoding::Utf8 => match self.carry[0] {
                    0..=0x7f => 1,
                    first => {
                        super::utf8_sequence_width(first).ok_or(SourceError::EncodingUnsupported)?
                    }
                },
                TextEncoding::Utf16Le | TextEncoding::Utf16Be if self.length < 2 => 2,
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                    if (0xd800..=0xdbff).contains(&self.unit(0)) {
                        4
                    } else {
                        2
                    }
                }
            };
            if self.length != required {
                continue;
            }
            let character = match self.encoding {
                TextEncoding::Utf8 => std::str::from_utf8(&self.carry[..required])
                    .ok()
                    .and_then(|text| text.chars().next())
                    .ok_or(SourceError::EncodingUnsupported)?,
                TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                    let first = self.unit(0);
                    let code = if required == 4 {
                        let second = self.unit(2);
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err(SourceError::EncodingUnsupported);
                        }
                        0x10000 + (u32::from(first) - 0xd800) * 1024 + u32::from(second) - 0xdc00
                    } else {
                        u32::from(first)
                    };
                    char::from_u32(code)
                        .filter(|value| *value != '\0')
                        .ok_or(SourceError::EncodingUnsupported)?
                }
            };
            let end = self
                .start
                .checked_add(u64::try_from(required).map_err(|_| SourceError::InvalidSourceSpan)?)
                .ok_or(SourceError::InvalidSourceSpan)?;
            scalar(self.start, end, character, &self.carry[..required])?;
            self.length = 0;
        }
        control.check()
    }

    fn unit(&self, offset: usize) -> u16 {
        let pair = [self.carry[offset], self.carry[offset + 1]];
        match self.encoding {
            TextEncoding::Utf16Le => u16::from_le_bytes(pair),
            TextEncoding::Utf16Be => u16::from_be_bytes(pair),
            TextEncoding::Utf8 => unreachable!("UTF-8 does not decode code units"),
        }
    }
}

struct LinePrefix {
    start: u64,
    line: u64,
    bytes: Vec<u8>,
    omitted: bool,
}

struct ContextPrefix {
    lines: VecDeque<LinePrefix>,
    maximum_lines: usize,
    maximum_bytes: usize,
    encoding: TextEncoding,
}

impl ContextPrefix {
    fn new(
        before: u16,
        encoding: TextEncoding,
        budget: SourceBudget,
        control: &SourceControl<'_>,
    ) -> Result<Self, SourceError> {
        let maximum_lines = if before == 0 {
            0
        } else {
            usize::from(before) + 1
        };
        let capacity = maximum_lines
            .checked_mul(
                budget
                    .max_source_bytes
                    .checked_add(size_of::<LinePrefix>())
                    .ok_or(SourceError::SnapshotBudgetExceeded)?,
            )
            .ok_or(SourceError::SnapshotBudgetExceeded)?;
        if u64::try_from(capacity).map_err(|_| SourceError::SnapshotBudgetExceeded)?
            > budget.max_snapshot_bytes
        {
            return Err(SourceError::SnapshotBudgetExceeded);
        }
        let mut lines = VecDeque::new();
        control.controlled(|| {
            lines
                .try_reserve_exact(maximum_lines)
                .map_err(|_| SourceError::MemoryUnavailable)
        })?;
        let actual_capacity = maximum_lines
            .checked_mul(budget.max_source_bytes)
            .and_then(|bytes| {
                lines
                    .capacity()
                    .checked_mul(size_of::<LinePrefix>())
                    .and_then(|slots| bytes.checked_add(slots))
            })
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(SourceError::SnapshotBudgetExceeded)?;
        if actual_capacity > budget.max_snapshot_bytes {
            return Err(SourceError::SnapshotBudgetExceeded);
        }
        let mut result = Self {
            lines,
            maximum_lines,
            maximum_bytes: budget.max_source_bytes,
            encoding,
        };
        if maximum_lines != 0 {
            result.next_line(0, 1, control)?;
        }
        Ok(result)
    }

    fn next_line(
        &mut self,
        start: u64,
        line: u64,
        control: &SourceControl<'_>,
    ) -> Result<(), SourceError> {
        if self.maximum_lines == 0 {
            return Ok(());
        }
        let mut prefix = if self.lines.len() == self.maximum_lines {
            self.lines.pop_front().ok_or(SourceError::ReadFailed)?
        } else {
            let mut bytes = Vec::new();
            control.controlled(|| {
                bytes
                    .try_reserve_exact(self.maximum_bytes)
                    .map_err(|_| SourceError::MemoryUnavailable)
            })?;
            if bytes.capacity() > self.maximum_bytes {
                return Err(SourceError::SnapshotBudgetExceeded);
            }
            LinePrefix {
                start,
                line,
                bytes,
                omitted: false,
            }
        };
        prefix.start = start;
        prefix.line = line;
        prefix.bytes.clear();
        prefix.omitted = false;
        self.lines.push_back(prefix);
        Ok(())
    }

    fn observe(
        &mut self,
        end: u64,
        line: u64,
        character: char,
        raw: &[u8],
        control: &SourceControl<'_>,
    ) -> Result<(), SourceError> {
        if let Some(prefix) = self.lines.back_mut() {
            if !prefix.omitted && raw.len() <= self.maximum_bytes.saturating_sub(prefix.bytes.len())
            {
                prefix.bytes.extend_from_slice(raw);
            } else {
                prefix.omitted = true;
            }
        }
        if character == '\n' {
            self.next_line(end, line, control)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct ResponseUsage {
    bytes: usize,
    capacity: usize,
    presentation: usize,
}

struct Selection {
    index: usize,
    selection_start: u64,
    selection_end: u64,
    start: u64,
    end: u64,
    start_line: u64,
    newlines: u64,
    trailing_newline: bool,
    started: bool,
    complete: bool,
    expanded_end: u64,
    after: Option<usize>,
    bytes: Vec<u8>,
    text_bytes: usize,
    prefix_limit: Option<usize>,
    clipped: bool,
}

impl Selection {
    fn new(index: usize, reference: &SourceRef, mode: SourceReadMode) -> Self {
        let prefix_limit = match mode {
            SourceReadMode::LosslessTextPrefix(maximum)
            | SourceReadMode::LosslessTextContextPrefix(maximum) => Some(maximum),
            _ => None,
        };
        Self {
            index,
            selection_start: reference.span().start_byte(),
            selection_end: reference.span().end_byte(),
            start: 0,
            end: 0,
            start_line: 1,
            newlines: 0,
            trailing_newline: false,
            started: false,
            complete: false,
            expanded_end: 0,
            after: None,
            bytes: Vec::new(),
            text_bytes: 0,
            prefix_limit,
            clipped: false,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one scalar is staged against independent physical, presentation, response and cancellation limits"
    )]
    fn append(
        &mut self,
        end: u64,
        character: char,
        raw: &[u8],
        encoding: TextEncoding,
        usage: &mut ResponseUsage,
        metadata: usize,
        budget: SourceBudget,
        mode: SourceReadMode,
        control: &SourceControl<'_>,
    ) -> Result<(), SourceError> {
        let next_text = self
            .text_bytes
            .checked_add(character.len_utf8())
            .ok_or(SourceError::SourceBudgetExceeded)?;
        if self.clipped || self.prefix_limit.is_some_and(|maximum| next_text > maximum) {
            self.clipped = true;
            return Ok(());
        }
        let presentation = if mode.is_lossless_text() && encoding != TextEncoding::Utf8 {
            character.len_utf8()
        } else {
            0
        };
        self.append_raw(raw, presentation, usage, metadata, budget, control)?;
        self.end = end;
        self.text_bytes = next_text;
        self.newlines = self
            .newlines
            .checked_add(u64::from(character == '\n'))
            .ok_or(SourceError::InvalidSourceSpan)?;
        self.trailing_newline = character == '\n';
        Ok(())
    }

    fn append_raw(
        &mut self,
        raw: &[u8],
        presentation: usize,
        usage: &mut ResponseUsage,
        metadata: usize,
        budget: SourceBudget,
        control: &SourceControl<'_>,
    ) -> Result<(), SourceError> {
        let total = usage
            .bytes
            .checked_add(raw.len())
            .filter(|total| *total <= budget.max_source_bytes)
            .ok_or(SourceError::SourceBudgetExceeded)?;
        let next_presentation = usage
            .presentation
            .checked_add(presentation)
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        let needed = self
            .bytes
            .len()
            .checked_add(raw.len())
            .ok_or(SourceError::SourceBudgetExceeded)?;
        if needed > self.bytes.capacity() {
            let available = budget
                .max_response_memory_bytes
                .checked_sub(metadata)
                .and_then(|n| n.checked_sub(next_presentation))
                .and_then(|n| n.checked_sub(usage.capacity))
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            let required = needed - self.bytes.capacity();
            if available < required {
                return Err(SourceError::ResponseMemoryBudgetExceeded);
            }
            let growth = 4096usize.max(required).min(available).min(
                budget
                    .max_source_bytes
                    .saturating_sub(self.bytes.capacity()),
            );
            if growth < required {
                return Err(SourceError::ResponseMemoryBudgetExceeded);
            }
            let before = self.bytes.capacity();
            let desired = before
                .checked_add(growth)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            control.controlled(|| {
                self.bytes
                    .try_reserve_exact(desired - self.bytes.len())
                    .map_err(|_| SourceError::MemoryUnavailable)
            })?;
            usage.capacity = usage
                .capacity
                .checked_add(self.bytes.capacity() - before)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        }
        if metadata
            .checked_add(usage.capacity)
            .and_then(|n| n.checked_add(next_presentation))
            .is_none_or(|n| n > budget.max_response_memory_bytes)
        {
            return Err(SourceError::ResponseMemoryBudgetExceeded);
        }
        self.bytes.extend_from_slice(raw);
        usage.bytes = total;
        usage.presentation = next_presentation;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "selection boundaries combine physical position, line context and the existing request limits"
    )]
    fn boundary(
        &mut self,
        offset: u64,
        line: u64,
        previous: Option<char>,
        prefix: &ContextPrefix,
        options: SourceReadOptions,
        usage: &mut ResponseUsage,
        metadata: usize,
        budget: SourceBudget,
        mode: SourceReadMode,
        control: &SourceControl<'_>,
    ) -> Result<(), SourceError> {
        if offset == self.selection_start && !self.started {
            self.started = true;
            self.start = offset;
            self.end = offset;
            self.start_line = line;
            let before = if matches!(mode, SourceReadMode::LosslessTextPrefix(_)) {
                0
            } else {
                options.context_lines_before
            };
            if before != 0 {
                let first = prefix.lines.len().saturating_sub(usize::from(before) + 1);
                let earliest = prefix.lines.get(first).ok_or(SourceError::ReadFailed)?;
                self.start = earliest.start;
                self.end = earliest.start;
                self.start_line = earliest.line;
                for stored in prefix.lines.iter().skip(first) {
                    if stored.omitted && self.prefix_limit.is_none() {
                        return Err(SourceError::SourceBudgetExceeded);
                    }
                    let mut decoder = ScalarDecoder::new(prefix.encoding);
                    decoder.push(
                        stored.start,
                        &stored.bytes,
                        control,
                        |_, end, character, raw| {
                            self.append(
                                end,
                                character,
                                raw,
                                prefix.encoding,
                                usage,
                                metadata,
                                budget,
                                mode,
                                control,
                            )
                        },
                    )?;
                    if stored.omitted && !self.clipped {
                        return Err(SourceError::SourceBudgetExceeded);
                    }
                }
            }
        }
        if offset == self.selection_end && self.after.is_none() {
            let after = if matches!(mode, SourceReadMode::LosslessTextPrefix(_)) {
                0
            } else {
                options.context_lines_after
            };
            if after == 0 {
                self.expanded_end = offset;
                self.complete = true;
            } else {
                self.after = Some(
                    usize::from(after)
                        + usize::from(
                            self.selection_start == self.selection_end
                                || offset == 0
                                || previous != Some('\n'),
                        ),
                );
            }
        }
        Ok(())
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the source entry point preserves the preflighted selectors, backing authority and existing read-mode budgets"
)]
pub(super) fn read(
    service: &SourceService<'_>,
    backing: &dyn VerifiedSourceBacking,
    references: &[SourceRef],
    options: SourceReadOptions,
    budget: SourceBudget,
    mode: SourceReadMode,
    prepared: PreparedRequest,
    control: &SourceControl<'_>,
) -> Result<RawSourceRead, SourceError> {
    let mut usage = ResponseUsage::default();
    let mut selections = Vec::new();
    try_reserve_vec(&mut selections, references.len(), control)?;
    for (index, reference) in references.iter().enumerate() {
        selections.push(Selection::new(index, reference, mode));
    }
    let raw_mode = matches!(mode, SourceReadMode::Legacy(SourceEncoding::Bytes));
    let mut encodings = Vec::new();
    try_reserve_vec(&mut encodings, references.len(), control)?;
    encodings.resize(references.len(), TextEncoding::Utf8);
    for (slot, prepared_file) in prepared.files.iter().enumerate() {
        let file = service
            .generation
            .find_file(prepared_file.file)
            .ok_or(SourceError::FileNotFound)?;
        if raw_mode {
            visit(backing, file, control, |offset, bytes| {
                let end = offset
                    .checked_add(
                        u64::try_from(bytes.len()).map_err(|_| SourceError::InvalidSourceSpan)?,
                    )
                    .ok_or(SourceError::InvalidSourceSpan)?;
                for selection in selections
                    .iter_mut()
                    .filter(|selection| prepared.file_slots[selection.index] == slot)
                {
                    let start = selection.selection_start.max(offset);
                    let stop = selection.selection_end.min(end);
                    if start < stop {
                        let a = usize::try_from(start - offset)
                            .map_err(|_| SourceError::InvalidSourceSpan)?;
                        let b = usize::try_from(stop - offset)
                            .map_err(|_| SourceError::InvalidSourceSpan)?;
                        selection.append_raw(
                            &bytes[a..b],
                            0,
                            &mut usage,
                            prepared.metadata_bytes,
                            budget,
                            control,
                        )?;
                    }
                    selection.start = selection.selection_start;
                    selection.end = selection.selection_end;
                    selection.expanded_end = selection.selection_end;
                }
                Ok(())
            })?;
            continue;
        }
        let encoding = lossless_text::physical_encoding(file)?;
        let before = if matches!(mode, SourceReadMode::LosslessTextPrefix(_)) {
            0
        } else {
            options.context_lines_before
        };
        let mut prefix = ContextPrefix::new(before, encoding, budget, control)?;
        let mut decoder = ScalarDecoder::new(encoding);
        let mut validator = StreamingSourceTextValidator::new();
        let mut line = 1u64;
        let mut previous = None;
        visit(backing, file, control, |offset, bytes| {
            validator
                .push(bytes, control.cancellation)
                .map_err(|error| SourceError::Cancelled(error.reason()))?;
            if selections
                .iter()
                .filter(|s| prepared.file_slots[s.index] == slot)
                .all(|s| s.complete)
            {
                return Ok(());
            }
            decoder.push(offset, bytes, control, |start, end, character, raw| {
                for selection in selections
                    .iter_mut()
                    .filter(|s| prepared.file_slots[s.index] == slot)
                {
                    if (start < selection.selection_start && selection.selection_start < end)
                        || (start < selection.selection_end && selection.selection_end < end)
                    {
                        return Err(SourceError::InvalidSourceSpan);
                    }
                    selection.boundary(
                        start,
                        line,
                        previous,
                        &prefix,
                        options,
                        &mut usage,
                        prepared.metadata_bytes,
                        budget,
                        mode,
                        control,
                    )?;
                    if selection.started && !selection.complete {
                        selection.append(
                            end,
                            character,
                            raw,
                            encoding,
                            &mut usage,
                            prepared.metadata_bytes,
                            budget,
                            mode,
                            control,
                        )?;
                        if character == '\n'
                            && let Some(after) = &mut selection.after
                        {
                            *after = after.saturating_sub(1);
                            if *after == 0 {
                                selection.complete = true;
                                selection.expanded_end = end;
                            }
                        }
                    }
                }
                if character == '\n' {
                    line = line.checked_add(1).ok_or(SourceError::InvalidSourceSpan)?;
                }
                prefix.observe(end, line, character, raw, control)?;
                previous = Some(character);
                Ok(())
            })
        })?;
        if validator.finish() != Some(encoding) {
            return Err(SourceError::EncodingUnsupported);
        }
        for selection in selections
            .iter_mut()
            .filter(|s| prepared.file_slots[s.index] == slot)
        {
            selection.boundary(
                file.byte_length,
                line,
                previous,
                &prefix,
                options,
                &mut usage,
                prepared.metadata_bytes,
                budget,
                mode,
                control,
            )?;
            if !selection.started {
                return Err(SourceError::InvalidSourceSpan);
            }
            if !selection.complete {
                selection.complete = true;
                selection.expanded_end = file.byte_length;
            }
            encodings[selection.index] = encoding;
        }
    }
    let mut chunks = Vec::new();
    try_reserve_vec(&mut chunks, references.len(), control)?;
    let mut metadata_capacity = 0usize;
    let mut truncated = false;
    for selection in selections {
        let reference = &references[selection.index];
        let file = service
            .generation
            .find_file(reference.span().file())
            .ok_or(SourceError::FileNotFound)?;
        let path = try_clone_string(&file.path, control)?;
        let language = try_clone_string(&file.language, control)?;
        metadata_capacity = metadata_capacity
            .checked_add(path.capacity())
            .and_then(|n| n.checked_add(language.capacity()))
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        if metadata_capacity
            .checked_add(usage.capacity)
            .and_then(|n| n.checked_add(usage.presentation))
            .is_none_or(|n| n > budget.max_response_memory_bytes)
        {
            return Err(SourceError::ResponseMemoryBudgetExceeded);
        }
        truncated |= selection.end < selection.expanded_end;
        let encoding = encodings[selection.index];
        chunks.push(SourceChunk {
            reference: reference.clone(),
            path,
            language,
            bytes: selection.bytes,
            start_byte: selection.start,
            end_byte: selection.end,
            start_line: (!raw_mode && !matches!(mode, SourceReadMode::LosslessTextPrefix(_)))
                .then_some(selection.start_line),
            end_line: (!raw_mode && !matches!(mode, SourceReadMode::LosslessTextPrefix(_)))
                .then(|| {
                    selection.start_line.checked_add(
                        selection
                            .newlines
                            .saturating_sub(u64::from(selection.trailing_newline)),
                    )
                })
                .flatten(),
            content_hash: file.content_hash,
            generated: file.generated,
            extracted_from: prepared.files[prepared.file_slots[selection.index]]
                .extracted_from
                .clone(),
            encoding: if mode.is_lossless_text() {
                if encoding == TextEncoding::Utf8 {
                    SourceEncoding::Utf8
                } else {
                    SourceEncoding::Bytes
                }
            } else {
                options.encoding
            },
            trust: SourceTrust::UntrustedRepositoryData,
        });
    }
    control.check()?;
    Ok(RawSourceRead {
        result: SourceReadResult {
            generation: service.generation.metadata().generation(),
            chunks,
            total_source_bytes: usage.bytes,
            total_metadata_bytes: prepared.metadata_bytes,
            total_response_memory_bytes: metadata_capacity
                .checked_add(usage.capacity)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?,
        },
        text_encodings: if mode.is_lossless_text() {
            encodings
        } else {
            Vec::new()
        },
        deadline: control.deadline,
        truncated,
    })
}
