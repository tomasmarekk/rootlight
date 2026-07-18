//! Generation-pinned, capability-confined source retrieval.
//!
//! The service resolves indexed file identities through a validated generation
//! before touching the VFS. Returned paths and bytes remain explicitly
//! untrusted repository data. Callers resolve and retain the generation before
//! construction; this crate does not select or reclaim catalog generations.

#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use rootlight_ir::{FileRecord, SourceRef};
use rootlight_storage::GenerationSnapshot;
use rootlight_vfs::{MAX_PATH_BYTES, RelativePath, RepositoryRoot, SourceSnapshot, VfsError};

/// Maximum source selectors accepted by one interactive request.
pub const HARD_MAX_SOURCE_SELECTORS: usize = 32;
/// Maximum context lines accepted on either side of a source selection.
pub const HARD_MAX_CONTEXT_LINES: u16 = 50;
/// Maximum source bytes returned by one interactive request.
pub const HARD_MAX_SOURCE_BYTES: usize = 512 * 1024;
/// Maximum UTF-8 path bytes copied into one source chunk.
pub const HARD_MAX_SOURCE_PATH_BYTES: usize = MAX_PATH_BYTES * 2;
/// Maximum language-identity bytes copied into one source chunk.
pub const HARD_MAX_SOURCE_LANGUAGE_BYTES: usize = 256;
/// Maximum aggregate path and language bytes copied into one response.
pub const HARD_MAX_SOURCE_METADATA_BYTES: usize =
    HARD_MAX_SOURCE_SELECTORS * (HARD_MAX_SOURCE_PATH_BYTES + HARD_MAX_SOURCE_LANGUAGE_BYTES);
/// Maximum aggregate variable-sized bytes owned by one source response.
pub const HARD_MAX_SOURCE_RESPONSE_MEMORY_BYTES: usize =
    HARD_MAX_SOURCE_BYTES + HARD_MAX_SOURCE_METADATA_BYTES;
/// Maximum source snapshot bytes retained while serving one request.
pub const HARD_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum cooperative synchronous source-read duration.
pub const HARD_MAX_SOURCE_DURATION: Duration = Duration::from_secs(10);
const UTF8_CHECK_CHUNK_BYTES: usize = 4 * 1024;

/// Resource limits for one generation-pinned source request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceBudget {
    /// Maximum selectors in one request.
    pub max_selectors: usize,
    /// Maximum context lines before or after a selection.
    pub max_context_lines: u16,
    /// Maximum aggregate bytes returned across all chunks.
    pub max_source_bytes: usize,
    /// Maximum aggregate path and language bytes copied into chunks.
    pub max_metadata_bytes: usize,
    /// Maximum aggregate source, path, and language bytes owned by the response.
    pub max_response_memory_bytes: usize,
    /// Maximum aggregate file bytes snapshotted for verification.
    pub max_snapshot_bytes: u64,
    /// Maximum cooperative monotonic wall time spent in the service.
    pub max_duration: Duration,
}

impl SourceBudget {
    /// Creates the default interactive source budget.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_selectors: HARD_MAX_SOURCE_SELECTORS,
            max_context_lines: 10,
            max_source_bytes: 64 * 1024,
            max_metadata_bytes: 256 * 1024,
            max_response_memory_bytes: 320 * 1024,
            max_snapshot_bytes: 16 * 1024 * 1024,
            max_duration: Duration::from_secs(2),
        }
    }

    /// Returns a budget with the selector ceiling replaced.
    #[must_use]
    pub const fn with_max_selectors(mut self, maximum: usize) -> Self {
        self.max_selectors = maximum;
        self
    }

    /// Returns a budget with the context-line ceiling replaced.
    #[must_use]
    pub const fn with_max_context_lines(mut self, maximum: u16) -> Self {
        self.max_context_lines = maximum;
        self
    }

    /// Returns a budget with the raw source-byte ceiling replaced.
    #[must_use]
    pub const fn with_max_source_bytes(mut self, maximum: usize) -> Self {
        self.max_source_bytes = maximum;
        self
    }

    /// Returns a budget with the copied metadata-byte ceiling replaced.
    #[must_use]
    pub const fn with_max_metadata_bytes(mut self, maximum: usize) -> Self {
        self.max_metadata_bytes = maximum;
        self
    }

    /// Returns a budget with the response-memory ceiling replaced.
    #[must_use]
    pub const fn with_max_response_memory_bytes(mut self, maximum: usize) -> Self {
        self.max_response_memory_bytes = maximum;
        self
    }

    /// Returns a budget with the aggregate snapshot-byte ceiling replaced.
    #[must_use]
    pub const fn with_max_snapshot_bytes(mut self, maximum: u64) -> Self {
        self.max_snapshot_bytes = maximum;
        self
    }

    /// Returns a budget with the cooperative duration ceiling replaced.
    #[must_use]
    pub const fn with_max_duration(mut self, maximum: Duration) -> Self {
        self.max_duration = maximum;
        self
    }
}

impl Default for SourceBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Presentation controls for bounded source chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceReadOptions {
    /// Whole lines included before the selected span.
    pub context_lines_before: u16,
    /// Whole lines included after the selected span.
    pub context_lines_after: u16,
}

impl SourceReadOptions {
    /// Creates the default source presentation options.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_lines_before: 2,
            context_lines_after: 2,
        }
    }

    /// Returns options with the leading context count replaced.
    #[must_use]
    pub const fn with_context_lines_before(mut self, lines: u16) -> Self {
        self.context_lines_before = lines;
        self
    }

    /// Returns options with the trailing context count replaced.
    #[must_use]
    pub const fn with_context_lines_after(mut self, lines: u16) -> Self {
        self.context_lines_after = lines;
        self
    }
}

impl Default for SourceReadOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable marker attached to every repository-controlled source value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceTrust {
    /// Bytes and path text are untrusted repository data, never instructions.
    UntrustedRepositoryData,
}

/// Encoding used to interpret a returned source chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceEncoding {
    /// The complete verified file was valid UTF-8 and bytes were not normalized.
    Utf8,
}

/// One verified source chunk with immutable identity and trust metadata.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceChunk {
    /// Exact indexed source reference selected by the caller.
    pub reference: SourceRef,
    /// Canonical repository-relative path from the pinned generation.
    pub path: String,
    /// Expanded chunk start in the verified file.
    pub start_byte: u64,
    /// Expanded chunk end in the verified file.
    pub end_byte: u64,
    /// One-based first line included in the chunk.
    pub start_line: u64,
    /// One-based last line included in the chunk.
    pub end_line: u64,
    /// Exact unnormalized bytes from the verified file.
    pub bytes: Vec<u8>,
    /// Immutable content identity verified before selection.
    pub content_hash: ContentHash,
    /// Indexed language identity.
    pub language: String,
    /// Whether the indexed file is generated.
    pub generated: bool,
    /// Verified interpretation of the bytes.
    pub encoding: SourceEncoding,
    /// Mandatory untrusted-data marker.
    pub trust: SourceTrust,
}

impl std::fmt::Debug for SourceChunk {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceChunk")
            .field("reference", &self.reference)
            .field("start_byte", &self.start_byte)
            .field("end_byte", &self.end_byte)
            .field("start_line", &self.start_line)
            .field("end_line", &self.end_line)
            .field("byte_length", &self.bytes.len())
            .field("content_hash", &self.content_hash)
            .field("generated", &self.generated)
            .field("encoding", &self.encoding)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// Aggregate source-read response for one pinned generation.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceReadResult {
    /// Generation that owned every resolved reference.
    pub generation: GenerationId,
    /// Chunks in request order.
    pub chunks: Vec<SourceChunk>,
    /// Aggregate raw source bytes before transport escaping.
    pub total_source_bytes: usize,
    /// Aggregate path and language bytes owned by all chunks.
    pub total_metadata_bytes: usize,
    /// Aggregate variable-sized bytes owned by all chunks.
    pub total_response_memory_bytes: usize,
}

impl std::fmt::Debug for SourceReadResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceReadResult")
            .field("generation", &self.generation)
            .field("chunk_count", &self.chunks.len())
            .field("total_source_bytes", &self.total_source_bytes)
            .field("total_metadata_bytes", &self.total_metadata_bytes)
            .field(
                "total_response_memory_bytes",
                &self.total_response_memory_bytes,
            )
            .finish()
    }
}

/// Source reader bound to one repository root and immutable generation.
pub struct SourceService<'a> {
    root: &'a RepositoryRoot,
    generation: &'a GenerationSnapshot,
}

impl<'a> SourceService<'a> {
    /// Binds a repository capability to matching immutable generation data.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::RepositoryMismatch`] when the VFS capability
    /// belongs to another repository.
    pub fn new(
        root: &'a RepositoryRoot,
        generation: &'a GenerationSnapshot,
    ) -> Result<Self, SourceError> {
        if root.repository() != generation.metadata().repository() {
            return Err(SourceError::RepositoryMismatch);
        }
        Ok(Self { root, generation })
    }

    /// Reads verified UTF-8 source selections with bounded line context.
    ///
    /// Selectors are resolved through generation-owned `FileRecord` values.
    /// Every selector is preflighted before the first VFS operation, and
    /// client-controlled paths are never accepted at this boundary. This
    /// strict first-slice primitive returns selectors in request order; a
    /// higher layer may later resolve additional selector kinds or merge
    /// overlapping references without changing this contract.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError`] for invalid budgets, foreign or stale
    /// references, unavailable encodings, VFS failures, cancellation, or
    /// resource exhaustion.
    pub fn read(
        &self,
        references: &[SourceRef],
        options: SourceReadOptions,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceReadResult, SourceError> {
        validate_budget(budget)?;
        if references.is_empty() || references.len() > budget.max_selectors {
            return Err(SourceError::SelectorLimit);
        }
        if options.context_lines_before > budget.max_context_lines
            || options.context_lines_after > budget.max_context_lines
        {
            return Err(SourceError::ContextLimit);
        }

        let control = SourceControl::new(cancellation, budget.max_duration);
        control.check()?;
        let metadata = self.generation.metadata();
        let prepared = self.preflight(
            references,
            metadata.repository(),
            metadata.generation(),
            budget,
            &control,
        )?;
        let mut snapshots = Vec::<SourceSnapshot>::new();
        try_reserve_vec(&mut snapshots, prepared.files.len(), &control)?;
        for prepared_file in &prepared.files {
            let file = self
                .generation
                .document()
                .files
                .get(prepared_file.file_index)
                .ok_or(SourceError::FileNotFound)?;
            let snapshot = control.controlled(|| {
                self.root
                    .snapshot_cancellable(
                        &prepared_file.path,
                        file.byte_length.max(1),
                        cancellation,
                        control.deadline,
                    )
                    .map_err(map_snapshot_error)
            })?;
            if snapshot.file() != file.id
                || snapshot.metadata().length != file.byte_length
                || snapshot.content_hash() != file.content_hash
            {
                return Err(SourceError::StaleSource);
            }
            ensure_utf8(file, snapshot.content(), &control)?;
            push_preallocated(&mut snapshots, snapshot, &control)?;
        }

        let mut chunks = Vec::new();
        try_reserve_vec(&mut chunks, references.len(), &control)?;
        let mut total_source_bytes = 0usize;
        let total_metadata_bytes = prepared.metadata_bytes;
        let mut total_response_memory_bytes = total_metadata_bytes;

        for (reference, file_slot) in references.iter().zip(prepared.file_slots) {
            control.check()?;
            let prepared_file = prepared
                .files
                .get(file_slot)
                .ok_or(SourceError::FileNotFound)?;
            let file = self
                .generation
                .document()
                .files
                .get(prepared_file.file_index)
                .ok_or(SourceError::FileNotFound)?;
            let snapshot = snapshots.get(file_slot).ok_or(SourceError::ReadFailed)?;
            let range = expand_context(snapshot.content(), reference, options, &control)?;
            let selected = snapshot
                .content()
                .get(range.start..range.end)
                .ok_or(SourceError::InvalidSourceSpan)?;
            let next_source_bytes = total_source_bytes
                .checked_add(selected.len())
                .ok_or(SourceError::SourceBudgetExceeded)?;
            if next_source_bytes > budget.max_source_bytes {
                return Err(SourceError::SourceBudgetExceeded);
            }
            let next_response_memory_bytes = total_metadata_bytes
                .checked_add(next_source_bytes)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            if next_response_memory_bytes > budget.max_response_memory_bytes {
                return Err(SourceError::ResponseMemoryBudgetExceeded);
            }
            let path = try_clone_string(&file.path, &control)?;
            let language = try_clone_string(&file.language, &control)?;
            let bytes = try_clone_bytes(selected, &control)?;
            total_source_bytes = next_source_bytes;
            total_response_memory_bytes = next_response_memory_bytes;
            push_preallocated(
                &mut chunks,
                SourceChunk {
                    reference: reference.clone(),
                    path,
                    start_byte: usize_to_u64(range.start)?,
                    end_byte: usize_to_u64(range.end)?,
                    start_line: range.start_line,
                    end_line: range.end_line,
                    bytes,
                    content_hash: file.content_hash,
                    language,
                    generated: file.generated,
                    encoding: SourceEncoding::Utf8,
                    trust: SourceTrust::UntrustedRepositoryData,
                },
                &control,
            )?;
        }
        control.check()?;

        Ok(SourceReadResult {
            generation: metadata.generation(),
            chunks,
            total_source_bytes,
            total_metadata_bytes,
            total_response_memory_bytes,
        })
    }

    fn preflight(
        &self,
        references: &[SourceRef],
        repository: RepositoryId,
        generation: GenerationId,
        budget: SourceBudget,
        control: &SourceControl<'_>,
    ) -> Result<PreparedRequest, SourceError> {
        let mut file_slots = Vec::new();
        try_reserve_vec(&mut file_slots, references.len(), control)?;
        let mut files = Vec::<PreparedFile>::new();
        try_reserve_vec(&mut files, references.len(), control)?;
        let mut metadata_bytes = 0usize;
        let mut minimum_source_bytes = 0usize;
        let mut snapshot_bytes = 0u64;

        for reference in references {
            control.check()?;
            validate_reference(reference, repository, generation)?;
            let file_index = self
                .generation
                .document()
                .files
                .binary_search_by_key(&reference.span().file(), |file| file.id)
                .map_err(|_| SourceError::FileNotFound)?;
            let file = self
                .generation
                .document()
                .files
                .get(file_index)
                .ok_or(SourceError::FileNotFound)?;
            validate_file(file, reference)?;
            validate_encoding(file)?;
            metadata_bytes = metadata_bytes
                .checked_add(chunk_metadata_bytes(file)?)
                .ok_or(SourceError::MetadataBudgetExceeded)?;
            if metadata_bytes > budget.max_metadata_bytes {
                return Err(SourceError::MetadataBudgetExceeded);
            }
            let selected_bytes = reference
                .span()
                .end_byte()
                .checked_sub(reference.span().start_byte())
                .and_then(|bytes| usize::try_from(bytes).ok())
                .ok_or(SourceError::SourceBudgetExceeded)?;
            minimum_source_bytes = minimum_source_bytes
                .checked_add(selected_bytes)
                .ok_or(SourceError::SourceBudgetExceeded)?;
            if minimum_source_bytes > budget.max_source_bytes {
                return Err(SourceError::SourceBudgetExceeded);
            }
            let minimum_response_bytes = metadata_bytes
                .checked_add(minimum_source_bytes)
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            if minimum_response_bytes > budget.max_response_memory_bytes {
                return Err(SourceError::ResponseMemoryBudgetExceeded);
            }

            let file_slot = if let Some(file_slot) = files
                .iter()
                .position(|prepared_file| prepared_file.file == file.id)
            {
                file_slot
            } else {
                snapshot_bytes = snapshot_bytes
                    .checked_add(file.byte_length)
                    .ok_or(SourceError::SnapshotBudgetExceeded)?;
                if snapshot_bytes > budget.max_snapshot_bytes {
                    return Err(SourceError::SnapshotBudgetExceeded);
                }
                let path = control.controlled(|| validated_path(self.root, file))?;
                let file_slot = files.len();
                push_preallocated(
                    &mut files,
                    PreparedFile {
                        file: file.id,
                        file_index,
                        path,
                    },
                    control,
                )?;
                file_slot
            };
            push_preallocated(&mut file_slots, file_slot, control)?;
        }

        Ok(PreparedRequest {
            file_slots,
            files,
            metadata_bytes,
        })
    }
}

impl std::fmt::Debug for SourceService<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceService")
            .field("repository", &self.generation.metadata().repository())
            .field("generation", &self.generation.metadata().generation())
            .finish_non_exhaustive()
    }
}

struct PreparedRequest {
    file_slots: Vec<usize>,
    files: Vec<PreparedFile>,
    metadata_bytes: usize,
}

struct PreparedFile {
    file: FileId,
    file_index: usize,
    path: RelativePath,
}

#[derive(Debug, Clone, Copy)]
struct ExpandedRange {
    start: usize,
    end: usize,
    start_line: u64,
    end_line: u64,
}

struct SourceControl<'a> {
    cancellation: &'a Cancellation,
    deadline: Instant,
    #[cfg(test)]
    after_operation: Option<&'a dyn Fn()>,
}

impl<'a> SourceControl<'a> {
    fn new(cancellation: &'a Cancellation, max_duration: Duration) -> Self {
        let started = Instant::now();
        Self {
            cancellation,
            deadline: started.checked_add(max_duration).unwrap_or(started),
            #[cfg(test)]
            after_operation: None,
        }
    }

    fn check(&self) -> Result<(), SourceError> {
        self.cancellation
            .check()
            .map_err(|cancelled| SourceError::Cancelled(cancelled.reason()))?;
        if Instant::now() >= self.deadline {
            return Err(SourceError::Cancelled(CancellationReason::DeadlineExceeded));
        }
        Ok(())
    }

    fn controlled<T>(
        &self,
        operation: impl FnOnce() -> Result<T, SourceError>,
    ) -> Result<T, SourceError> {
        self.check()?;
        let result = operation();
        #[cfg(test)]
        if let Some(after_operation) = self.after_operation {
            after_operation();
        }
        self.check()?;
        result
    }

    #[cfg(test)]
    fn with_after_operation(mut self, after_operation: &'a dyn Fn()) -> Self {
        self.after_operation = Some(after_operation);
        self
    }
}

fn map_snapshot_error(error: VfsError) -> SourceError {
    match error {
        VfsError::Cancelled(reason) => SourceError::Cancelled(reason),
        VfsError::UnstableFile | VfsError::FileTooLarge { .. } => SourceError::StaleSource,
        VfsError::MemoryUnavailable => SourceError::MemoryUnavailable,
        _ => SourceError::ReadFailed,
    }
}

fn validate_budget(budget: SourceBudget) -> Result<(), SourceError> {
    if budget.max_selectors == 0 || budget.max_selectors > HARD_MAX_SOURCE_SELECTORS {
        return Err(SourceError::InvalidBudget);
    }
    if budget.max_context_lines > HARD_MAX_CONTEXT_LINES
        || budget.max_source_bytes == 0
        || budget.max_source_bytes > HARD_MAX_SOURCE_BYTES
        || budget.max_metadata_bytes == 0
        || budget.max_metadata_bytes > HARD_MAX_SOURCE_METADATA_BYTES
        || budget.max_response_memory_bytes == 0
        || budget.max_response_memory_bytes > HARD_MAX_SOURCE_RESPONSE_MEMORY_BYTES
        || budget.max_snapshot_bytes == 0
        || budget.max_snapshot_bytes > HARD_MAX_SNAPSHOT_BYTES
        || budget.max_duration.is_zero()
        || budget.max_duration > HARD_MAX_SOURCE_DURATION
    {
        return Err(SourceError::InvalidBudget);
    }
    Ok(())
}

fn validated_path(root: &RepositoryRoot, file: &FileRecord) -> Result<RelativePath, SourceError> {
    let locator = file
        .path_locator
        .as_ref()
        .ok_or(SourceError::InvalidIndexedPath)?;
    let path = RelativePath::from_locator(locator).map_err(|_| SourceError::InvalidIndexedPath)?;
    if path.as_str() != file.path || root.file_id(&path) != file.id {
        return Err(SourceError::InvalidIndexedPath);
    }
    Ok(path)
}

fn chunk_metadata_bytes(file: &FileRecord) -> Result<usize, SourceError> {
    if file.path.len() > HARD_MAX_SOURCE_PATH_BYTES
        || file.language.len() > HARD_MAX_SOURCE_LANGUAGE_BYTES
    {
        return Err(SourceError::MetadataStringLimitExceeded);
    }
    file.path
        .len()
        .checked_add(file.language.len())
        .ok_or(SourceError::MetadataStringLimitExceeded)
}

fn validate_reference(
    reference: &SourceRef,
    repository: RepositoryId,
    generation: GenerationId,
) -> Result<(), SourceError> {
    if reference.repository() != repository {
        return Err(SourceError::RepositoryMismatch);
    }
    if reference.generation() != generation {
        return Err(SourceError::GenerationMismatch);
    }
    Ok(())
}

fn validate_file(file: &FileRecord, reference: &SourceRef) -> Result<(), SourceError> {
    if file.repository != reference.repository()
        || file.generation != reference.generation()
        || file.id != reference.span().file()
        || file.content_hash != reference.content_hash()
        || reference.span().end_byte() > file.byte_length
    {
        return Err(SourceError::StaleSource);
    }
    Ok(())
}

fn ensure_utf8(
    file: &FileRecord,
    content: &[u8],
    control: &SourceControl<'_>,
) -> Result<(), SourceError> {
    validate_encoding(file)?;

    let mut offset = 0usize;
    while offset < content.len() {
        control.check()?;
        let end = offset
            .saturating_add(UTF8_CHECK_CHUNK_BYTES)
            .min(content.len());
        match std::str::from_utf8(&content[offset..end]) {
            Ok(_) => offset = end,
            Err(error) if error.error_len().is_none() && end < content.len() => {
                let sequence_start = offset
                    .checked_add(error.valid_up_to())
                    .ok_or(SourceError::EncodingUnsupported)?;
                let sequence_width = utf8_sequence_width(content[sequence_start])
                    .ok_or(SourceError::EncodingUnsupported)?;
                let sequence_end = sequence_start
                    .checked_add(sequence_width)
                    .filter(|sequence_end| *sequence_end <= content.len())
                    .ok_or(SourceError::EncodingUnsupported)?;
                std::str::from_utf8(&content[sequence_start..sequence_end])
                    .map_err(|_| SourceError::EncodingUnsupported)?;
                offset = sequence_end;
            }
            Err(_) => return Err(SourceError::EncodingUnsupported),
        }
    }
    Ok(())
}

fn utf8_sequence_width(first: u8) -> Option<usize> {
    match first {
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn expand_context(
    content: &[u8],
    reference: &SourceRef,
    options: SourceReadOptions,
    control: &SourceControl<'_>,
) -> Result<ExpandedRange, SourceError> {
    let selection_start = usize::try_from(reference.span().start_byte())
        .map_err(|_| SourceError::InvalidSourceSpan)?;
    let selection_end =
        usize::try_from(reference.span().end_byte()).map_err(|_| SourceError::InvalidSourceSpan)?;
    if selection_start > selection_end || selection_end > content.len() {
        return Err(SourceError::InvalidSourceSpan);
    }
    if !is_utf8_boundary(content, selection_start) || !is_utf8_boundary(content, selection_end) {
        return Err(SourceError::InvalidSourceSpan);
    }

    let starts_capacity = usize::from(options.context_lines_before) + 1;
    let mut starts = VecDeque::new();
    control.controlled(|| {
        starts
            .try_reserve_exact(starts_capacity)
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    push_back_preallocated(&mut starts, 0usize, control)?;
    for (index, byte) in content[..selection_start].iter().copied().enumerate() {
        check_iteration(index, control)?;
        if byte == b'\n' {
            if starts.len() == starts_capacity {
                starts.pop_front();
            }
            push_back_preallocated(&mut starts, index + 1, control)?;
        }
    }
    let start = starts.front().copied().unwrap_or(0);
    let start_line = count_lines_before(content, start, control)?;

    let mut end = selection_end;
    if selection_start == selection_end
        || end == 0
        || content.get(end.saturating_sub(1)) != Some(&b'\n')
    {
        end = next_line_end(content, end, control)?;
    }
    for _ in 0..options.context_lines_after {
        if end >= content.len() {
            break;
        }
        end = next_line_end(content, end, control)?;
    }
    let newline_count = count_newlines(&content[start..end], control)?;
    let trailing_newline = usize::from(content.get(end.saturating_sub(1)) == Some(&b'\n'));
    let represented_lines = newline_count
        .saturating_add(1)
        .saturating_sub(trailing_newline);
    let end_line = start_line
        .checked_add(u64::try_from(represented_lines.saturating_sub(1)).unwrap_or(u64::MAX))
        .ok_or(SourceError::InvalidSourceSpan)?;

    Ok(ExpandedRange {
        start,
        end,
        start_line,
        end_line,
    })
}

fn is_utf8_boundary(content: &[u8], boundary: usize) -> bool {
    boundary == content.len()
        || content
            .get(boundary)
            .is_some_and(|byte| !matches!(byte, 0x80..=0xbf))
}

fn next_line_end(
    content: &[u8],
    start: usize,
    control: &SourceControl<'_>,
) -> Result<usize, SourceError> {
    for (offset, byte) in content[start..].iter().copied().enumerate() {
        check_iteration(offset, control)?;
        if byte == b'\n' {
            return start
                .checked_add(offset)
                .and_then(|value| value.checked_add(1))
                .ok_or(SourceError::InvalidSourceSpan);
        }
    }
    Ok(content.len())
}

fn count_lines_before(
    content: &[u8],
    boundary: usize,
    control: &SourceControl<'_>,
) -> Result<u64, SourceError> {
    let newlines = count_newlines(&content[..boundary], control)?;
    u64::try_from(newlines)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(SourceError::InvalidSourceSpan)
}

fn count_newlines(content: &[u8], control: &SourceControl<'_>) -> Result<usize, SourceError> {
    let mut count = 0usize;
    for (index, byte) in content.iter().copied().enumerate() {
        check_iteration(index, control)?;
        if byte == b'\n' {
            count = count.checked_add(1).ok_or(SourceError::InvalidSourceSpan)?;
        }
    }
    Ok(count)
}

fn check_iteration(index: usize, control: &SourceControl<'_>) -> Result<(), SourceError> {
    if index.is_multiple_of(4_096) {
        control.check()?;
    }
    Ok(())
}

fn usize_to_u64(value: usize) -> Result<u64, SourceError> {
    u64::try_from(value).map_err(|_| SourceError::InvalidSourceSpan)
}

fn validate_encoding(file: &FileRecord) -> Result<(), SourceError> {
    if matches!(file.encoding.as_str(), "utf-8" | "utf8") {
        Ok(())
    } else {
        Err(SourceError::EncodingUnsupported)
    }
}

fn try_reserve_vec<T>(
    values: &mut Vec<T>,
    additional: usize,
    control: &SourceControl<'_>,
) -> Result<(), SourceError> {
    control.controlled(|| {
        values
            .try_reserve_exact(additional)
            .map_err(|_| SourceError::MemoryUnavailable)
    })
}

fn push_preallocated<T>(
    values: &mut Vec<T>,
    value: T,
    control: &SourceControl<'_>,
) -> Result<(), SourceError> {
    control.controlled(|| {
        if values.len() == values.capacity() {
            return Err(SourceError::MemoryUnavailable);
        }
        values.push(value);
        Ok(())
    })
}

fn push_back_preallocated<T>(
    values: &mut VecDeque<T>,
    value: T,
    control: &SourceControl<'_>,
) -> Result<(), SourceError> {
    control.controlled(|| {
        if values.len() == values.capacity() {
            return Err(SourceError::MemoryUnavailable);
        }
        values.push_back(value);
        Ok(())
    })
}

fn try_clone_string(value: &str, control: &SourceControl<'_>) -> Result<String, SourceError> {
    let mut cloned = String::new();
    control.controlled(|| {
        cloned
            .try_reserve_exact(value.len())
            .map_err(|_| SourceError::MemoryUnavailable)
    })?;
    control.controlled(|| {
        if cloned.capacity() < value.len() {
            return Err(SourceError::MemoryUnavailable);
        }
        cloned.push_str(value);
        Ok(())
    })?;
    Ok(cloned)
}

fn try_clone_bytes(value: &[u8], control: &SourceControl<'_>) -> Result<Vec<u8>, SourceError> {
    let mut cloned = Vec::new();
    try_reserve_vec(&mut cloned, value.len(), control)?;
    control.controlled(|| {
        if cloned.capacity() < value.len() {
            return Err(SourceError::MemoryUnavailable);
        }
        cloned.extend_from_slice(value);
        Ok(())
    })?;
    Ok(cloned)
}

/// Closed, source-redacted failures from bounded source retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// A caller-provided budget was zero or above a hard ceiling.
    #[error("source budget is invalid")]
    InvalidBudget,
    /// The selector set was empty or exceeded its admitted count.
    #[error("source selector limit is invalid")]
    SelectorLimit,
    /// Requested context exceeded the admitted line count.
    #[error("source context limit is invalid")]
    ContextLimit,
    /// VFS and generation repository identities differed.
    #[error("source repository does not match the pinned generation")]
    RepositoryMismatch,
    /// A source reference named another immutable generation.
    #[error("source reference generation does not match")]
    GenerationMismatch,
    /// No generation-owned file record matched a selector.
    #[error("source file was not found in the pinned generation")]
    FileNotFound,
    /// Persisted path identity was not canonical or did not match the file ID.
    #[error("indexed source path identity is invalid")]
    InvalidIndexedPath,
    /// Indexed and live immutable source identity differed.
    #[error("source reference is stale")]
    StaleSource,
    /// A selected byte span was not representable in the verified file.
    #[error("source span is invalid")]
    InvalidSourceSpan,
    /// The first slice cannot truthfully return this file's encoding.
    #[error("source encoding is unsupported")]
    EncodingUnsupported,
    /// Aggregate file verification exceeded its memory admission budget.
    #[error("source snapshot budget exceeded")]
    SnapshotBudgetExceeded,
    /// One returned path or language identity exceeded its hard string ceiling.
    #[error("source metadata string limit exceeded")]
    MetadataStringLimitExceeded,
    /// Aggregate returned path and language bytes exceeded their admission budget.
    #[error("source metadata budget exceeded")]
    MetadataBudgetExceeded,
    /// Returned source bytes exceeded the response admission budget.
    #[error("source response budget exceeded")]
    SourceBudgetExceeded,
    /// Aggregate variable-sized response memory exceeded its admission budget.
    #[error("source response memory budget exceeded")]
    ResponseMemoryBudgetExceeded,
    /// An admitted bounded response could not reserve memory.
    #[error("source response memory is unavailable")]
    MemoryUnavailable,
    /// Cooperative cancellation or a monotonic deadline stopped the read.
    #[error("source read was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// A source-redacted VFS operation failed.
    #[error("source read failed")]
    ReadFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{
        GenerationIdentity, content_hash, derive_fact, derive_generation, derive_repository,
    };
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, ExtensionSupport, FactEvidence, FileRecord, IrLimits,
        NormalizedIrDocument, ProducerIdentity, ProducerKind, ProvenanceRecord, SourceSpan,
    };
    use rootlight_storage::GenerationMetadata;
    use std::{cell::Cell, fs, path::Path};
    use tempfile::tempdir_in;

    struct Fixture {
        _temporary: tempfile::TempDir,
        root: RepositoryRoot,
        generation: GenerationSnapshot,
        reference: SourceRef,
    }

    fn fixture(content: &[u8], encoding: &str, selection: (u64, u64)) -> Fixture {
        fixture_at_path(content, encoding, selection, Path::new("src/sample.rs"))
    }

    fn fixture_at_path(
        content: &[u8],
        encoding: &str,
        selection: (u64, u64),
        relative_path: &Path,
    ) -> Fixture {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        let repository = derive_repository(b"rootlight-source-test").id();
        let root = RepositoryRoot::open(repository, temporary.path())
            .expect("temporary directory is a valid repository root");
        let path = RelativePath::parse(relative_path).expect("fixture path is canonical");
        let absolute_path = temporary.path().join(relative_path);
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).expect("fixture directory is created");
        }
        fs::write(&absolute_path, content).expect("fixture source is written");

        let manifest_hash = content_hash(b"source-test-manifest");
        let configuration_hash = content_hash(b"source-test-configuration");
        let provider_set_hash = content_hash(b"source-test-providers");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: 1,
        })
        .id();
        let file = root.file_id(&path);
        let hash = content_hash(content);
        let full_source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(
                file,
                0,
                u64::try_from(content.len()).expect("fixture length fits u64"),
            )
            .expect("full fixture span is valid"),
            hash,
            None,
        );
        let provenance = derive_fact("rootlight.provenance/v1", b"rootlight-source-test").id();
        let producer = ProducerIdentity::new("rootlight-source-test", "1.0.0", configuration_hash)
            .expect("fixture producer is valid");
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.provenance.push(ProvenanceRecord {
            id: provenance,
            repository,
            generation,
            producer_kind: ProducerKind::Rule,
            producer,
            binary_digest: content_hash(b"source-test-binary"),
            frontend_version: Some("1.0.0".to_owned()),
            language: "rust".to_owned(),
            tier: AnalysisTier::TierB,
            build_context: BuildContextIdentity::new(content_hash(b"source-test-build")),
            input_sources: vec![full_source.clone()],
            evidence_sources: vec![full_source.clone()],
            derivation_parents: Vec::new(),
            rule: Some("source-test".to_owned()),
        });
        document.files.push(FileRecord {
            id: file,
            repository,
            generation,
            path: path.as_str().to_owned(),
            path_locator: Some(path.to_locator()),
            content_hash: hash,
            byte_length: u64::try_from(content.len()).expect("fixture length fits u64"),
            language: "rust".to_owned(),
            encoding: encoding.to_owned(),
            generated: false,
            provenance,
            evidence: FactEvidence {
                source: Some(full_source),
                derivation: Vec::new(),
            },
        });
        let metadata = GenerationMetadata::new(
            repository,
            generation,
            None,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        )
        .expect("fixture metadata is valid");
        let generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("fixture generation is valid");
        let reference = SourceRef::new(
            repository,
            generation.metadata().generation(),
            SourceSpan::new(file, selection.0, selection.1)
                .expect("selected fixture span is valid"),
            hash,
            None,
        );

        Fixture {
            _temporary: temporary,
            root,
            generation,
            reference,
        }
    }

    fn add_fixture_file(
        fixture: &mut Fixture,
        relative_path: &str,
        content: &[u8],
        selection: (u64, u64),
    ) -> SourceRef {
        let path = RelativePath::parse(Path::new(relative_path))
            .expect("additional fixture path is canonical");
        let absolute_path = fixture._temporary.path().join(relative_path);
        fs::create_dir_all(
            absolute_path
                .parent()
                .expect("additional fixture path has a parent"),
        )
        .expect("additional fixture directory is created");
        fs::write(&absolute_path, content).expect("additional fixture source is written");

        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        let file = fixture.root.file_id(&path);
        let hash = content_hash(content);
        let full_source = SourceRef::new(
            metadata.repository(),
            metadata.generation(),
            SourceSpan::new(
                file,
                0,
                u64::try_from(content.len()).expect("fixture length fits u64"),
            )
            .expect("full additional fixture span is valid"),
            hash,
            None,
        );
        let provenance = document
            .provenance
            .first()
            .expect("fixture provenance exists")
            .id;
        document.files.push(FileRecord {
            id: file,
            repository: metadata.repository(),
            generation: metadata.generation(),
            path: path.as_str().to_owned(),
            path_locator: Some(path.to_locator()),
            content_hash: hash,
            byte_length: u64::try_from(content.len()).expect("fixture length fits u64"),
            language: "rust".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance,
            evidence: FactEvidence {
                source: Some(full_source),
                derivation: Vec::new(),
            },
        });
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("extended fixture generation is valid");

        SourceRef::new(
            metadata.repository(),
            metadata.generation(),
            SourceSpan::new(file, selection.0, selection.1)
                .expect("additional fixture selection is valid"),
            hash,
            None,
        )
    }

    fn read(
        fixture: &Fixture,
        references: &[SourceRef],
        options: SourceReadOptions,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceReadResult, SourceError> {
        SourceService::new(&fixture.root, &fixture.generation)
            .expect("fixture capability and generation match")
            .read(references, options, budget, cancellation)
    }

    #[test]
    fn reads_exact_unnormalized_bytes_with_bounded_line_context() {
        let fixture = fixture(b"zero\r\none\nTWO\nthree\nfour\n", "utf-8", (10, 13));
        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions {
                context_lines_before: 1,
                context_lines_after: 1,
            },
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("source selection resolves");

        assert_eq!(
            result.generation,
            fixture.generation.metadata().generation()
        );
        assert_eq!(result.total_source_bytes, 14);
        assert_eq!(result.chunks.len(), 1);
        let chunk = &result.chunks[0];
        assert_eq!(chunk.path, "src/sample.rs");
        assert_eq!((chunk.start_byte, chunk.end_byte), (6, 20));
        assert_eq!((chunk.start_line, chunk.end_line), (2, 4));
        assert_eq!(chunk.bytes, b"one\nTWO\nthree\n");
        assert_eq!(chunk.encoding, SourceEncoding::Utf8);
        assert_eq!(chunk.trust, SourceTrust::UntrustedRepositoryData);
    }

    #[cfg(unix)]
    #[test]
    fn unix_lossless_locators_disambiguate_raw_and_literal_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let mut fixture = fixture_at_path(
            b"raw\n",
            "utf-8",
            (0, 4),
            Path::new(OsStr::from_bytes(b"\xff")),
        );
        let literal = add_fixture_file(&mut fixture, "@raw-ff", b"literal\n", (0, 8));
        let result = read(
            &fixture,
            &[fixture.reference.clone(), literal],
            SourceReadOptions::default(),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("both colliding presentation paths resolve");

        assert_ne!(
            result.chunks[0].reference.span().file(),
            result.chunks[1].reference.span().file()
        );
        assert_eq!(result.chunks[0].path, result.chunks[1].path);
        assert_eq!(result.chunks[0].bytes, b"raw\n");
        assert_eq!(result.chunks[1].bytes, b"literal\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_lossless_locators_disambiguate_wide_and_literal_paths() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        let raw_name = OsString::from_wide(&[0xd800]);
        let mut fixture = fixture_at_path(b"raw\n", "utf-8", (0, 4), Path::new(&raw_name));
        let literal = add_fixture_file(&mut fixture, "@raw-00d8", b"literal\n", (0, 8));
        let result = read(
            &fixture,
            &[fixture.reference.clone(), literal],
            SourceReadOptions::default(),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("both colliding presentation paths resolve");

        assert_ne!(
            result.chunks[0].reference.span().file(),
            result.chunks[1].reference.span().file()
        );
        assert_eq!(result.chunks[0].path, result.chunks[1].path);
        assert_eq!(result.chunks[0].bytes, b"raw\n");
        assert_eq!(result.chunks[1].bytes, b"literal\n");
    }

    #[test]
    fn rejects_foreign_generations_before_reading_repository_data() {
        let fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("fixture source is removed");
        let foreign = SourceRef::new(
            fixture.reference.repository(),
            rootlight_ids::GenerationId::from_bytes([0x55; 20]),
            fixture.reference.span(),
            fixture.reference.content_hash(),
            None,
        );

        assert_eq!(
            read(
                &fixture,
                &[foreign],
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::GenerationMismatch)
        );
    }

    #[test]
    fn preflights_every_selector_before_reading_the_first_file() {
        let fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("first fixture source is removed");
        let foreign = SourceRef::new(
            fixture.reference.repository(),
            rootlight_ids::GenerationId::from_bytes([0x55; 20]),
            fixture.reference.span(),
            fixture.reference.content_hash(),
            None,
        );

        assert_eq!(
            read(
                &fixture,
                &[fixture.reference.clone(), foreign],
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::GenerationMismatch)
        );
    }

    #[test]
    fn preflights_later_encoding_before_reading_the_first_file() {
        let mut fixture = fixture(b"first\n", "utf-8", (0, 5));
        let second = add_fixture_file(&mut fixture, "src/second.rs", b"second\n", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("first fixture source is removed");
        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        document
            .files
            .iter_mut()
            .find(|file| file.id == second.span().file())
            .expect("second fixture file exists")
            .encoding = "utf-16".to_owned();
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("fixture generation permits an unsupported source encoding");

        assert_eq!(
            read(
                &fixture,
                &[fixture.reference.clone(), second],
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        );
    }

    #[test]
    fn preflights_snapshot_and_duplicate_minimums_before_vfs_access() {
        let mut snapshot_fixture = fixture(b"aaaa", "utf-8", (0, 4));
        let second = add_fixture_file(&mut snapshot_fixture, "src/second.rs", b"bbbb", (0, 4));
        fs::remove_file(snapshot_fixture._temporary.path().join("src/sample.rs"))
            .expect("first snapshot fixture source is removed");
        assert_eq!(
            read(
                &snapshot_fixture,
                &[snapshot_fixture.reference.clone(), second],
                SourceReadOptions::default(),
                SourceBudget::new().with_max_snapshot_bytes(7),
                &Cancellation::new()
            ),
            Err(SourceError::SnapshotBudgetExceeded)
        );

        let duplicate_fixture = fixture(b"aaaa", "utf-8", (0, 4));
        fs::remove_file(duplicate_fixture._temporary.path().join("src/sample.rs"))
            .expect("duplicate fixture source is removed");
        let duplicate = [
            duplicate_fixture.reference.clone(),
            duplicate_fixture.reference.clone(),
        ];
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);
        assert_eq!(
            read(
                &duplicate_fixture,
                &duplicate,
                options,
                SourceBudget::new().with_max_source_bytes(7),
                &Cancellation::new()
            ),
            Err(SourceError::SourceBudgetExceeded)
        );

        let metadata_bytes =
            chunk_metadata_bytes(&duplicate_fixture.generation.document().files[0])
                .expect("fixture metadata is bounded")
                * duplicate.len();
        assert_eq!(
            read(
                &duplicate_fixture,
                &duplicate,
                options,
                SourceBudget::new().with_max_response_memory_bytes(metadata_bytes + 7),
                &Cancellation::new()
            ),
            Err(SourceError::ResponseMemoryBudgetExceeded)
        );
    }

    #[test]
    fn rejects_invalid_indexed_paths_before_vfs_access() {
        let mut fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("fixture source is removed");
        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        document.files[0].path = "../escape.rs".to_owned();
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("generation validation does not replace the VFS path boundary");

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidIndexedPath)
        );
    }

    #[test]
    fn older_records_without_locators_never_fall_back_to_display_paths() {
        let mut fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("fixture source is removed");
        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        document.files[0].path_locator = None;
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("older compatible records remain valid IR");

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidIndexedPath)
        );
    }

    #[test]
    fn rejects_stale_content_and_unsupported_encodings() {
        let stale = fixture(b"original\n", "utf-8", (0, 8));
        fs::write(stale._temporary.path().join("src/sample.rs"), b"modified\n")
            .expect("fixture source is rewritten");
        assert_eq!(
            read(
                &stale,
                std::slice::from_ref(&stale.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::StaleSource)
        );

        let encoded = fixture(b"source\n", "utf-16", (0, 6));
        assert_eq!(
            read(
                &encoded,
                std::slice::from_ref(&encoded.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        );

        let invalid = fixture(&[0xff, 0xfe], "utf-8", (0, 2));
        assert_eq!(
            read(
                &invalid,
                std::slice::from_ref(&invalid.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        );
    }

    #[test]
    fn validates_utf8_sequences_that_cross_internal_checkpoints() {
        let mut content = vec![b'a'; UTF8_CHECK_CHUNK_BYTES - 1];
        content.extend_from_slice("🦀\n".as_bytes());
        let start = u64::try_from(UTF8_CHECK_CHUNK_BYTES - 1).expect("checkpoint fits u64");
        let end = start + 4;
        let fixture = fixture(&content, "utf-8", (start, end));

        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions {
                context_lines_before: 0,
                context_lines_after: 0,
            },
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("split UTF-8 sequence is validated");

        assert_eq!(result.chunks[0].bytes, content);
    }

    #[test]
    fn rejects_source_spans_inside_utf8_scalars() {
        let fixture = fixture("a🦀b\n".as_bytes(), "utf-8", (2, 3));

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidSourceSpan)
        );
    }

    #[test]
    fn zero_width_spans_at_line_start_select_the_current_line() {
        let fixture = fixture(b"a\nb\n", "utf-8", (2, 2));
        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("zero-width line-start selection resolves");

        let chunk = &result.chunks[0];
        assert_eq!(chunk.bytes, b"b\n");
        assert_eq!((chunk.start_byte, chunk.end_byte), (2, 4));
        assert_eq!((chunk.start_line, chunk.end_line), (2, 2));
    }

    #[test]
    fn enforces_snapshot_response_and_admission_budgets() {
        let fixture = fixture(b"one\ntwo\nthree\n", "utf-8", (4, 7));
        let snapshot_budget = SourceBudget::new().with_max_snapshot_bytes(4);
        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                snapshot_budget,
                &Cancellation::new()
            ),
            Err(SourceError::SnapshotBudgetExceeded)
        );

        let response_budget = SourceBudget::new().with_max_source_bytes(3);
        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                response_budget,
                &Cancellation::new()
            ),
            Err(SourceError::SourceBudgetExceeded)
        );

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::new().with_max_selectors(HARD_MAX_SOURCE_SELECTORS + 1),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidBudget)
        );

        let duplicate_budget = SourceBudget::new()
            .with_max_snapshot_bytes(fixture.generation.document().files[0].byte_length);
        let duplicate = read(
            &fixture,
            &[fixture.reference.clone(), fixture.reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            duplicate_budget,
            &Cancellation::new(),
        )
        .expect("one file is snapshotted once for duplicate selectors");
        assert_eq!(duplicate.chunks.len(), 2);
    }

    #[test]
    fn charges_duplicate_chunk_metadata_and_response_memory() {
        let fixture = fixture(b"one\ntwo\n", "utf-8", (4, 7));
        let references = [fixture.reference.clone(), fixture.reference.clone()];
        let per_chunk_metadata = chunk_metadata_bytes(&fixture.generation.document().files[0])
            .expect("fixture metadata is bounded");
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);

        assert_eq!(
            read(
                &fixture,
                &references,
                options,
                SourceBudget::new()
                    .with_max_metadata_bytes(per_chunk_metadata)
                    .with_max_response_memory_bytes(HARD_MAX_SOURCE_RESPONSE_MEMORY_BYTES),
                &Cancellation::new()
            ),
            Err(SourceError::MetadataBudgetExceeded)
        );

        let all_metadata = per_chunk_metadata * references.len();
        assert_eq!(
            read(
                &fixture,
                &references,
                options,
                SourceBudget::new()
                    .with_max_metadata_bytes(all_metadata)
                    .with_max_response_memory_bytes(all_metadata + 7),
                &Cancellation::new()
            ),
            Err(SourceError::ResponseMemoryBudgetExceeded)
        );

        let result = read(
            &fixture,
            &references,
            options,
            SourceBudget::new()
                .with_max_metadata_bytes(all_metadata)
                .with_max_response_memory_bytes(all_metadata + 8),
            &Cancellation::new(),
        )
        .expect("exact duplicate response memory is admitted");
        assert_eq!(result.total_metadata_bytes, all_metadata);
        assert_eq!(result.total_source_bytes, 8);
        assert_eq!(result.total_response_memory_bytes, all_metadata + 8);
    }

    #[test]
    fn rejects_oversized_metadata_before_vfs_access() {
        let mut fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("fixture source is removed");
        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        document.files[0].language = "x".repeat(HARD_MAX_SOURCE_LANGUAGE_BYTES + 1);
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("IR permits a language identity above the source hard ceiling");

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::MetadataStringLimitExceeded)
        );
    }

    #[test]
    fn enforces_aggregate_snapshot_admission_across_files() {
        let mut fixture = fixture(b"aaaa", "utf-8", (0, 4));
        let second = add_fixture_file(&mut fixture, "src/second.rs", b"bbbb", (0, 4));
        let references = [fixture.reference.clone(), second];
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);

        assert_eq!(
            read(
                &fixture,
                &references,
                options,
                SourceBudget::new().with_max_snapshot_bytes(7),
                &Cancellation::new()
            ),
            Err(SourceError::SnapshotBudgetExceeded)
        );

        let result = read(
            &fixture,
            &references,
            options,
            SourceBudget::new().with_max_snapshot_bytes(8),
            &Cancellation::new(),
        )
        .expect("exact aggregate snapshot budget is admitted");
        assert_eq!(result.total_source_bytes, 8);
    }

    #[test]
    fn enforces_exact_selector_hard_boundary() {
        let fixture = fixture(b"x\n", "utf-8", (0, 1));
        let admitted = vec![fixture.reference.clone(); HARD_MAX_SOURCE_SELECTORS];
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);

        let result = read(
            &fixture,
            &admitted,
            options,
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("the exact selector hard boundary is admitted");
        assert_eq!(result.chunks.len(), HARD_MAX_SOURCE_SELECTORS);

        let rejected = vec![fixture.reference.clone(); HARD_MAX_SOURCE_SELECTORS + 1];
        assert_eq!(
            read(
                &fixture,
                &rejected,
                options,
                SourceBudget::default(),
                &Cancellation::new()
            ),
            Err(SourceError::SelectorLimit)
        );
    }

    #[test]
    fn enforces_exact_context_line_hard_boundary() {
        let content = "x\n".repeat(101);
        let fixture = fixture(content.as_bytes(), "utf-8", (100, 101));
        let budget = SourceBudget::new().with_max_context_lines(HARD_MAX_CONTEXT_LINES);
        let admitted = SourceReadOptions::new()
            .with_context_lines_before(HARD_MAX_CONTEXT_LINES)
            .with_context_lines_after(HARD_MAX_CONTEXT_LINES);

        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            admitted,
            budget,
            &Cancellation::new(),
        )
        .expect("the exact context hard boundary is admitted");
        assert_eq!(result.chunks[0].bytes, content.as_bytes());
        assert_eq!(
            (result.chunks[0].start_line, result.chunks[0].end_line),
            (1, 101)
        );

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                admitted.with_context_lines_after(HARD_MAX_CONTEXT_LINES + 1),
                budget,
                &Cancellation::new()
            ),
            Err(SourceError::ContextLimit)
        );
    }

    #[test]
    fn enforces_exact_source_byte_hard_boundary() {
        let content = vec![b'x'; HARD_MAX_SOURCE_BYTES];
        let boundary = u64::try_from(content.len()).expect("hard boundary fits u64");
        let boundary_fixture = fixture(&content, "utf-8", (0, boundary));
        let budget = SourceBudget::new()
            .with_max_source_bytes(HARD_MAX_SOURCE_BYTES)
            .with_max_snapshot_bytes(boundary)
            .with_max_metadata_bytes(HARD_MAX_SOURCE_METADATA_BYTES)
            .with_max_response_memory_bytes(HARD_MAX_SOURCE_RESPONSE_MEMORY_BYTES)
            .with_max_duration(HARD_MAX_SOURCE_DURATION);
        let options = SourceReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);

        let result = read(
            &boundary_fixture,
            std::slice::from_ref(&boundary_fixture.reference),
            options,
            budget,
            &Cancellation::new(),
        )
        .expect("the exact source-byte hard boundary is admitted");
        assert_eq!(result.total_source_bytes, HARD_MAX_SOURCE_BYTES);

        let oversized = vec![b'x'; HARD_MAX_SOURCE_BYTES + 1];
        let oversized_boundary =
            u64::try_from(oversized.len()).expect("oversized fixture fits u64");
        let oversized_fixture = fixture(&oversized, "utf-8", (0, oversized_boundary));
        assert_eq!(
            read(
                &oversized_fixture,
                std::slice::from_ref(&oversized_fixture.reference),
                options,
                budget.with_max_snapshot_bytes(oversized_boundary),
                &Cancellation::new()
            ),
            Err(SourceError::SourceBudgetExceeded)
        );
    }

    #[test]
    fn propagates_cancellation_and_enforces_the_local_deadline() {
        let fixture = fixture(b"source\n", "utf-8", (0, 6));
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &cancellation
            ),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::new().with_max_duration(Duration::from_nanos(1)),
                &Cancellation::new()
            ),
            Err(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
        );
    }

    #[test]
    fn cancellation_after_reservations_and_copies_precedes_success() {
        let reserve_cancellation = Cancellation::new();
        let reserve_hook = || {
            let _ = reserve_cancellation.cancel(CancellationReason::ParentCancelled);
        };
        let reserve_control = SourceControl::new(&reserve_cancellation, Duration::from_secs(1))
            .with_after_operation(&reserve_hook);
        let mut reserved = Vec::<u8>::new();
        assert_eq!(
            try_reserve_vec(&mut reserved, 1, &reserve_control),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        assert!(reserved.capacity() >= 1);

        let metadata_cancellation = Cancellation::new();
        let metadata_operations = Cell::new(0usize);
        let metadata_hook = || {
            let operation = metadata_operations.get() + 1;
            metadata_operations.set(operation);
            if operation == 2 {
                let _ = metadata_cancellation.cancel(CancellationReason::ParentCancelled);
            }
        };
        let metadata_control = SourceControl::new(&metadata_cancellation, Duration::from_secs(1))
            .with_after_operation(&metadata_hook);
        assert_eq!(
            try_clone_string("repository-controlled", &metadata_control),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        assert_eq!(metadata_operations.get(), 2);

        let bytes_cancellation = Cancellation::new();
        let byte_operations = Cell::new(0usize);
        let bytes_hook = || {
            let operation = byte_operations.get() + 1;
            byte_operations.set(operation);
            if operation == 2 {
                let _ = bytes_cancellation.cancel(CancellationReason::ParentCancelled);
            }
        };
        let bytes_control = SourceControl::new(&bytes_cancellation, Duration::from_secs(1))
            .with_after_operation(&bytes_hook);
        assert_eq!(
            try_clone_bytes(b"repository-controlled", &bytes_control),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        assert_eq!(byte_operations.get(), 2);
    }

    #[test]
    fn cancellation_after_snapshot_insertion_precedes_success() {
        let fixture = fixture(b"source\n", "utf-8", (0, 6));
        let path = RelativePath::parse(Path::new("src/sample.rs"))
            .expect("fixture source path is canonical");
        let snapshot = fixture
            .root
            .snapshot(&path, 7)
            .expect("fixture source snapshot succeeds");
        let cancellation = Cancellation::new();
        let setup_control = SourceControl::new(&cancellation, Duration::from_secs(1));
        let mut snapshots = Vec::new();
        try_reserve_vec(&mut snapshots, 1, &setup_control).expect("snapshot slot is preallocated");

        let insertion_hook = || {
            let _ = cancellation.cancel(CancellationReason::ParentCancelled);
        };
        let insertion_control = SourceControl::new(&cancellation, Duration::from_secs(1))
            .with_after_operation(&insertion_hook);
        assert_eq!(
            push_preallocated(&mut snapshots, snapshot, &insertion_control),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        assert_eq!(snapshots.len(), 1);
    }

    #[test]
    fn deadline_after_an_operation_precedes_its_local_error() {
        let cancellation = Cancellation::new();
        let control = SourceControl::new(&cancellation, Duration::from_millis(10));
        let deadline = control.deadline;
        let result: Result<(), SourceError> = control.controlled(|| {
            while Instant::now() < deadline {
                std::hint::spin_loop();
            }
            Err(SourceError::MemoryUnavailable)
        });

        assert_eq!(
            result,
            Err(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
        );
    }

    #[test]
    fn source_failures_do_not_expose_repository_controlled_paths() {
        let fixture = fixture(b"source\n", "utf-8", (0, 6));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("fixture source is removed");
        let error = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::default(),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect_err("missing source fails closed");

        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("sample.rs"));
            assert!(!rendered.contains("src/"));
        }
    }

    #[test]
    fn source_response_debug_is_source_redacted() {
        let fixture = fixture(b"do not log this source\n", "utf-8", (0, 22));
        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::default(),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("fixture source resolves");

        let rendered = format!("{result:?} {:?}", result.chunks[0]);
        assert!(!rendered.contains("do not log"));
        assert!(!rendered.contains("sample.rs"));
        assert!(!rendered.contains("language"));
    }
}
