//! Generation-bound literal search over authenticated source backing.
//!
//! File identity comes from the generation; source bytes remain borrowed during
//! matching, and responses own only bounded metadata and exact source references.

use std::{fmt, mem::size_of, ops::Range};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, GenerationId};
use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_vfs::{SourceText, TextEncoding};

use super::{
    LiteralPattern, LiteralSearchBudget, LiteralSearchError, LiteralSearchStop, SourceBudget,
    SourceControl, SourceError, SourceReadMode, SourceService, SourceTrust, lossless_text,
    push_preallocated, try_clone_string, try_reserve_vec, usize_to_u64,
};

/// Independent source admission, response, and literal matcher allowances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFindBudget {
    /// Existing source identity, snapshot, metadata, response, and duration limits.
    /// `max_source_bytes` does not limit scanning because no source body is returned.
    pub source: SourceBudget,
    /// Query, forward scan, match count, matcher memory, and cursor restoration limits.
    pub matcher: LiteralSearchBudget,
}

impl SourceFindBudget {
    /// Creates the source-service defaults with a separate bounded matcher.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            source: SourceBudget::new(),
            matcher: LiteralSearchBudget {
                max_query_bytes: 64 * 1024,
                max_scan_bytes: 64 * 1024 * 1024,
                max_matches: 128,
                max_memory_bytes: 2 * 1024 * 1024,
                max_restore_bytes: 256 * 1024,
            },
        }
    }

    /// Validates source limits and nonzero scan/result allowances.
    ///
    /// # Errors
    /// Returns the underlying source or matcher budget error.
    pub fn validate(self) -> Result<(), SourceFindError> {
        self.source.validate()?;
        if self.matcher.max_scan_bytes == 0 || self.matcher.max_matches == 0 {
            return Err(LiteralSearchError::InvalidBudget.into());
        }
        Ok(())
    }
}

impl Default for SourceFindBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Continuation parts validated against the verified source before each scan.
///
/// Transport callers must authenticate repository, generation, file, content
/// hash, and query identity in addition to these bounded matcher parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFindCursor {
    /// Next original byte to scan, at a Unicode scalar boundary.
    pub next_byte: usize,
    /// Query scalars matched immediately before that byte.
    pub matched_prefix: usize,
}

/// Why the returned forward work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFindStop {
    /// The complete verified source has been examined through EOF.
    EndOfSource,
    /// Forward work reached the original-byte scan allowance.
    ScanLimit,
    /// The requested match count was reached.
    MatchLimit,
    /// Internal pattern/span memory filled its independent allowance.
    MatcherMemoryLimit,
    /// Source-reference response capacity filled the source response allowance.
    ResponseMemoryLimit,
}

/// Verified file metadata shared by every match and scan range in one response.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceFindFile {
    /// Selected immutable file identity.
    pub id: FileId,
    /// Generation-owned repository-relative display path.
    pub path: String,
    /// Indexed language identity.
    pub language: String,
    /// Hash of the complete verified physical source bytes.
    pub content_hash: ContentHash,
    /// Complete original byte length, including any physical BOM.
    pub byte_length: u64,
    /// Verified physical text encoding.
    pub encoding: TextEncoding,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Mandatory marker for repository-controlled data.
    pub trust: SourceTrust,
    /// Whole immediate container dependency for an extracted virtual source.
    pub extracted_from: Option<SourceRef>,
}

impl fmt::Debug for SourceFindFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceFindFile")
            .field("id", &self.id)
            .field("content_hash", &self.content_hash)
            .field("byte_length", &self.byte_length)
            .field("encoding", &self.encoding)
            .field("generated", &self.generated)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// Exact match evidence and explicit forward coverage from one literal search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFindResult {
    /// Immutable generation that owns the selected file and returned evidence.
    pub generation: GenerationId,
    /// One shared verified file-metadata record.
    pub file: SourceFindFile,
    /// Every match found in this page, including overlaps; no bodies are copied.
    pub matches: Vec<SourceRef>,
    /// Original bytes examined forward, excluding cursor restoration work.
    pub scanned: Range<u64>,
    /// Preceding original bytes examined once to validate the continuation.
    pub restoration_bytes: usize,
    /// Explicit EOF or resource boundary; no exact unexamined total is implied.
    pub stop: SourceFindStop,
    /// A positive-progress continuation when source remains unexamined.
    pub continuation: Option<SourceFindCursor>,
    /// Owned path and language lengths.
    pub total_metadata_bytes: usize,
    /// Actual capacities of owned metadata and the source-reference vector.
    pub total_response_memory_bytes: usize,
}

/// Preserves the layer that rejected a source search.
#[derive(Debug, thiserror::Error)]
pub enum SourceFindError {
    /// Source identity, snapshot, encoding, response, or control failure.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// Literal query, continuation, work, or matcher allocation failure.
    #[error(transparent)]
    Literal(#[from] LiteralSearchError),
}

impl SourceService<'_> {
    /// Finds an exact case-sensitive literal in a generation-owned file.
    ///
    /// Query admission precedes storage access. The complete immutable source
    /// is authenticated before any match returns, independently of response size.
    /// Source BOM and scalar behavior follows [`SourceText`]; continuation parts
    /// are checked against the actual source, then scanned strictly forward.
    ///
    /// # Errors
    /// Returns the original source or matcher error. Failures return no partial
    /// response and do not advance the supplied continuation.
    pub fn find_literal(
        &self,
        file: FileId,
        query: &str,
        cursor: Option<SourceFindCursor>,
        budget: SourceFindBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceFindResult, SourceFindError> {
        budget.validate()?;
        let control = SourceControl::for_service(self, cancellation, budget.source);
        let pattern = literal_controlled(&control, |check| {
            LiteralPattern::compile_with_check(query, budget.matcher, check)
        })?;
        let file = self
            .generation
            .find_file(file)
            .ok_or(SourceError::FileNotFound)?;
        let metadata = self.generation.metadata();
        // This identity-only selection requests no body output. Preflight keeps
        // full verification work separate from snapshot or streaming memory.
        let identity = SourceRef::new(
            metadata.repository(),
            metadata.generation(),
            SourceSpan::new(file.id, 0, 0).map_err(|_| SourceError::InvalidSourceSpan)?,
            file.content_hash,
            None,
        );
        let prepared = self.preflight(
            std::slice::from_ref(&identity),
            metadata.repository(),
            metadata.generation(),
            SourceReadMode::LosslessText,
            budget.source,
            &control,
        )?;
        let prepared_file = prepared.files.first().ok_or(SourceError::FileNotFound)?;
        if let super::SourceBacking::Verified(backing) = self.backing {
            return super::streamed_find::find(
                self, backing, file, &pattern, cursor, budget, prepared, &control,
            );
        }
        let snapshot = self.resolve_snapshot(prepared_file, file, cancellation, &control)?;
        let snapshot = snapshot.as_ref();
        if snapshot.file() != file.id
            || snapshot.metadata().length != file.byte_length
            || snapshot.content_hash() != file.content_hash
        {
            return Err(SourceError::StaleSource.into());
        }
        let text = SourceText::parse_with_check(snapshot.content(), || control.check())?
            .ok_or(SourceError::EncodingUnsupported)?;
        if text.encoding() != lossless_text::physical_encoding(file)? {
            return Err(SourceError::EncodingUnsupported.into());
        }
        let path = try_clone_string(&file.path, &control)?;
        let language = try_clone_string(&file.language, &control)?;
        let metadata_capacity = path
            .capacity()
            .checked_add(language.capacity())
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        let response_slots = budget
            .source
            .max_response_memory_bytes
            .checked_sub(metadata_capacity)
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?
            / size_of::<SourceRef>();
        let match_limit = budget.matcher.max_matches.min(response_slots);
        if match_limit == 0 {
            return Err(SourceError::ResponseMemoryBudgetExceeded.into());
        }
        let matcher = LiteralSearchBudget {
            max_matches: match_limit,
            ..budget.matcher
        };
        let page = literal_controlled(&control, |check| {
            pattern.scan_parts_with_check(
                text,
                cursor.map(|cursor| (cursor.next_byte, cursor.matched_prefix)),
                matcher,
                check,
            )
        })?;
        let mut matches = Vec::new();
        try_reserve_vec(&mut matches, page.matches.len(), &control)?;
        let total_response_memory_bytes = matches
            .capacity()
            .checked_mul(size_of::<SourceRef>())
            .and_then(|bytes| bytes.checked_add(metadata_capacity))
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
        if total_response_memory_bytes > budget.source.max_response_memory_bytes {
            return Err(SourceError::ResponseMemoryBudgetExceeded.into());
        }
        for span in page.matches {
            let span = SourceSpan::new(file.id, usize_to_u64(span.start)?, usize_to_u64(span.end)?)
                .map_err(|_| SourceError::InvalidSourceSpan)?;
            push_preallocated(
                &mut matches,
                SourceRef::new(
                    metadata.repository(),
                    metadata.generation(),
                    span,
                    file.content_hash,
                    None,
                ),
                &control,
            )?;
        }
        let stop = match page.stop {
            LiteralSearchStop::EndOfSource => SourceFindStop::EndOfSource,
            LiteralSearchStop::ScanLimit => SourceFindStop::ScanLimit,
            LiteralSearchStop::MatchLimit if match_limit < budget.matcher.max_matches => {
                SourceFindStop::ResponseMemoryLimit
            }
            LiteralSearchStop::MatchLimit => SourceFindStop::MatchLimit,
            LiteralSearchStop::MemoryLimit => SourceFindStop::MatcherMemoryLimit,
        };
        let continuation = page.continuation.map(|cursor| SourceFindCursor {
            next_byte: cursor.next_byte(),
            matched_prefix: cursor.matched_prefix(),
        });
        control.check()?;
        Ok(SourceFindResult {
            generation: metadata.generation(),
            file: SourceFindFile {
                id: file.id,
                path,
                language,
                content_hash: file.content_hash,
                byte_length: file.byte_length,
                encoding: text.encoding(),
                generated: file.generated,
                trust: SourceTrust::UntrustedRepositoryData,
                extracted_from: prepared_file.extracted_from.clone(),
            },
            matches,
            scanned: usize_to_u64(page.scanned.start)?..usize_to_u64(page.scanned.end)?,
            restoration_bytes: page.restoration_bytes,
            stop,
            continuation,
            total_metadata_bytes: prepared.metadata_bytes,
            total_response_memory_bytes,
        })
    }
}

pub(super) fn literal_controlled<T>(
    control: &SourceControl<'_>,
    operation: impl FnOnce(
        &mut dyn FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<T, LiteralSearchError>,
) -> Result<T, SourceFindError> {
    let mut source_error = None;
    let result = operation(&mut || match control.check() {
        Ok(()) => Ok(()),
        Err(error) => {
            source_error = Some(error);
            Err(LiteralSearchError::ExternalControlStopped)
        }
    });
    match source_error {
        Some(error) => Err(error.into()),
        None => result.map_err(SourceFindError::Literal),
    }
}
