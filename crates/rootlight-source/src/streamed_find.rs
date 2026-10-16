//! Literal pages staged during a complete authenticated raw visit.
//! KMP continuation restoration and forward scanning retain separate physical
//! work boundaries; unselected tails still undergo strict text validation.

use std::mem::size_of;

use rootlight_ir::{FileRecord, SourceRef, SourceSpan};
use rootlight_vfs::{StreamingSourceTextValidator, TextEncoding};

use super::{
    LiteralPattern, LiteralSearchError, PreparedRequest, SourceControl, SourceError,
    SourceFindBudget, SourceFindCursor, SourceFindError, SourceFindFile, SourceFindResult,
    SourceFindStop, SourceService, SourceTrust,
    literal_find::literal_controlled,
    lossless_text,
    streamed::{ScalarDecoder, VerifiedSourceBacking, visit},
    try_clone_string, try_reserve_vec,
};

fn advance(
    pattern: &LiteralPattern,
    prefix: usize,
    character: char,
    control: &SourceControl<'_>,
) -> Result<(usize, bool), SourceFindError> {
    literal_controlled(control, |check| {
        pattern.advance(prefix, character, &mut || check())
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the search entry point carries the compiled pattern, authenticated backing and independent matcher/source limits"
)]
pub(super) fn find(
    service: &SourceService<'_>,
    backing: &dyn VerifiedSourceBacking,
    file: &FileRecord,
    pattern: &LiteralPattern,
    cursor: Option<SourceFindCursor>,
    budget: SourceFindBudget,
    prepared: PreparedRequest,
    control: &SourceControl<'_>,
) -> Result<SourceFindResult, SourceFindError> {
    let encoding = lossless_text::physical_encoding(file)?;
    let content_start = if encoding == TextEncoding::Utf8 { 0 } else { 2 };
    let length = usize::try_from(file.byte_length).map_err(|_| SourceError::InvalidSourceSpan)?;
    let start = cursor.map_or(content_start, |cursor| {
        if cursor.next_byte == 0 {
            content_start
        } else {
            cursor.next_byte
        }
    });
    let mut prefix = cursor.map_or(0, |cursor| cursor.matched_prefix);
    if start > length || prefix >= pattern.scalar_count() {
        return Err(LiteralSearchError::InvalidCursor.into());
    }
    let restore_from = start
        .saturating_sub(pattern.encoded_width(encoding))
        .max(content_start);
    let scan_end = start
        .checked_add(budget.matcher.max_scan_bytes.min(length - start))
        .ok_or(LiteralSearchError::InvalidCursor)?;
    let path = try_clone_string(&file.path, control)?;
    let language = try_clone_string(&file.language, control)?;
    let metadata_capacity = path
        .capacity()
        .checked_add(language.capacity())
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
    let response_limit = budget
        .source
        .max_response_memory_bytes
        .checked_sub(metadata_capacity)
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?
        / size_of::<SourceRef>();
    let match_limit = budget.matcher.max_matches.min(response_limit);
    if match_limit == 0 {
        return Err(SourceError::ResponseMemoryBudgetExceeded.into());
    }
    let matcher_capacity = budget
        .matcher
        .max_memory_bytes
        .checked_sub(pattern.memory_bytes())
        .ok_or(LiteralSearchError::MemoryLimit)?
        / size_of::<std::ops::Range<usize>>();
    let capacity = match_limit.min(matcher_capacity).min(length - start);
    if capacity == 0 && start != length {
        return Err(LiteralSearchError::MemoryLimit.into());
    }
    let mut matches = Vec::<SourceRef>::new();
    try_reserve_vec(&mut matches, capacity, control)?;
    let total_memory = matches
        .capacity()
        .checked_mul(size_of::<SourceRef>())
        .and_then(|bytes| bytes.checked_add(metadata_capacity))
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
    if total_memory > budget.source.max_response_memory_bytes {
        return Err(SourceError::ResponseMemoryBudgetExceeded.into());
    }
    let mut decoder = ScalarDecoder::new(encoding);
    let mut validator = StreamingSourceTextValidator::new();
    let mut restore_start = None;
    let mut restore_prefix = 0;
    let mut restoration_bytes = 0;
    let mut restored = cursor.is_none();
    let mut next = start;
    let mut stop = None;
    let mut matching_error = None;
    let generation = service.generation.metadata();
    let result = visit(backing, file, control, |offset, bytes| {
        validator
            .push(bytes, control.cancellation)
            .map_err(|error| SourceError::Cancelled(error.reason()))?;
        if stop.is_some() {
            return Ok(());
        }
        decoder.push(offset, bytes, control, |begin, end, character, _| {
            let process = (|| -> Result<(), SourceFindError> {
                if stop.is_some() {
                    return Ok(());
                }
                let begin = usize::try_from(begin).map_err(|_| SourceError::InvalidSourceSpan)?;
                let end = usize::try_from(end).map_err(|_| SourceError::InvalidSourceSpan)?;
                if begin < start && start < end {
                    return Err(LiteralSearchError::InvalidCursor.into());
                }
                if begin < start {
                    if cursor.is_some() && begin >= restore_from {
                        restore_start.get_or_insert(begin);
                        restore_prefix = advance(pattern, restore_prefix, character, control)?.0;
                    }
                    return Ok(());
                }
                if !restored {
                    restoration_bytes = start - restore_start.unwrap_or(start);
                    if restoration_bytes > budget.matcher.max_restore_bytes {
                        return Err(LiteralSearchError::RestoreLimit.into());
                    }
                    if begin != start || restore_prefix != prefix {
                        return Err(LiteralSearchError::InvalidCursor.into());
                    }
                    restored = true;
                }
                if end > scan_end {
                    if next == start {
                        return Err(LiteralSearchError::NoProgress.into());
                    }
                    stop = Some(SourceFindStop::ScanLimit);
                    return Ok(());
                }
                let (updated, found) = advance(pattern, prefix, character, control)?;
                prefix = updated;
                next = end;
                if found {
                    let begin = end
                        .checked_sub(pattern.encoded_width(encoding))
                        .ok_or(LiteralSearchError::InvalidCursor)?;
                    let span = SourceSpan::new(
                        file.id,
                        u64::try_from(begin).map_err(|_| SourceError::InvalidSourceSpan)?,
                        u64::try_from(end).map_err(|_| SourceError::InvalidSourceSpan)?,
                    )
                    .map_err(|_| SourceError::InvalidSourceSpan)?;
                    if matches.len() == matches.capacity() {
                        return Err(LiteralSearchError::MemoryLimit.into());
                    }
                    matches.push(SourceRef::new(
                        generation.repository(),
                        generation.generation(),
                        span,
                        file.content_hash,
                        None,
                    ));
                    if matches.len() == capacity && end < length {
                        stop = Some(if capacity < match_limit {
                            SourceFindStop::MatcherMemoryLimit
                        } else if match_limit < budget.matcher.max_matches {
                            SourceFindStop::ResponseMemoryLimit
                        } else {
                            SourceFindStop::MatchLimit
                        });
                    }
                }
                if next == scan_end && next < length && stop.is_none() {
                    stop = Some(SourceFindStop::ScanLimit);
                }
                Ok(())
            })();
            match process {
                Ok(()) => Ok(()),
                Err(error) => {
                    matching_error = Some(error);
                    Err(SourceError::ReadFailed)
                }
            }
        })
    });
    if let Err(error @ SourceError::Cancelled(_)) = result {
        return Err(error.into());
    }
    if let Some(error) = matching_error {
        return Err(error);
    }
    result?;
    if validator.finish() != Some(encoding) {
        return Err(SourceError::EncodingUnsupported.into());
    }
    if !restored {
        restoration_bytes = start - restore_start.unwrap_or(start);
        if restoration_bytes > budget.matcher.max_restore_bytes {
            return Err(LiteralSearchError::RestoreLimit.into());
        }
        if start != length || restore_prefix != prefix {
            return Err(LiteralSearchError::InvalidCursor.into());
        }
    }
    let stop = stop.unwrap_or(SourceFindStop::EndOfSource);
    control.check()?;
    Ok(SourceFindResult {
        generation: generation.generation(),
        file: SourceFindFile {
            id: file.id,
            path,
            language,
            content_hash: file.content_hash,
            byte_length: file.byte_length,
            encoding,
            generated: file.generated,
            trust: SourceTrust::UntrustedRepositoryData,
            extracted_from: prepared
                .files
                .first()
                .ok_or(SourceError::FileNotFound)?
                .extracted_from
                .clone(),
        },
        matches,
        scanned: u64::try_from(start).map_err(|_| SourceError::InvalidSourceSpan)?
            ..u64::try_from(next).map_err(|_| SourceError::InvalidSourceSpan)?,
        restoration_bytes,
        stop,
        continuation: (stop != SourceFindStop::EndOfSource).then_some(SourceFindCursor {
            next_byte: next,
            matched_prefix: prefix,
        }),
        total_metadata_bytes: prepared.metadata_bytes,
        total_response_memory_bytes: total_memory,
    })
}
