//! Generation-pinned, capability-confined source retrieval.
//!
//! The service resolves indexed file identities through a validated generation
//! before reading either a live VFS capability or caller-retained snapshots.
//! Returned paths and bytes remain explicitly untrusted repository data.
//! Callers resolve and retain the generation before construction; this crate
//! does not select or reclaim catalog generations.

#![forbid(unsafe_code)]

pub mod compound;
mod lossless_text;
mod metadata;
pub mod opc;
pub mod vba_compression;
pub mod vba_dir;
pub mod vba_header;
pub mod vba_membership;
pub mod vba_project;
pub mod vba_reference;
pub mod vba_source;
pub mod vba_text;
pub mod zip;

pub use lossless_text::{
    LosslessTextReadOptions, SourceTextChunk, SourceTextPrefixReadResult, SourceTextReadResult,
};
pub use metadata::{SourceMetadata, source_metadata_memory_upper, validate_source_metadata};

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use rootlight_ir::{FileRecord, SourceMappingKind, SourceRef};
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
    /// Maximum aggregate source, metadata, and owned text-presentation buffers.
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

    /// Validates every source-budget field against its hard ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::InvalidBudget`] when a required field is zero or
    /// any field exceeds the source-service contract.
    pub fn validate(self) -> Result<(), SourceError> {
        validate_budget(self)
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
    /// Exact byte representation required by the caller.
    pub encoding: SourceEncoding,
}

impl SourceReadOptions {
    /// Creates the default source presentation options.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_lines_before: 2,
            context_lines_after: 2,
            encoding: SourceEncoding::Utf8,
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

    /// Returns options with the source representation replaced.
    #[must_use]
    pub const fn with_encoding(mut self, encoding: SourceEncoding) -> Self {
        self.encoding = encoding;
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
    /// Return exact bytes without interpreting repository content as text.
    Bytes,
}

/// One verified source chunk with immutable identity and trust metadata.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceChunk {
    /// Whole immediate container dependency, when this is an extracted source.
    /// The display path is then virtual, not an editable filesystem path;
    /// returned byte offsets address the derived source, not the container.
    pub extracted_from: Option<SourceRef>,
    /// Exact indexed source reference selected by the caller.
    pub reference: SourceRef,
    /// Canonical repository-relative path from the pinned generation.
    pub path: String,
    /// Expanded chunk start in the verified file.
    pub start_byte: u64,
    /// Expanded chunk end in the verified file.
    pub end_byte: u64,
    /// One-based first line included in the chunk.
    pub start_line: Option<u64>,
    /// One-based last line included in the chunk.
    pub end_line: Option<u64>,
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

/// Source reader bound to an immutable generation and verified source backing.
///
/// The backing is either a live repository capability that revalidates current
/// files or an ordered process-local snapshot set retained by its caller.
pub struct SourceService<'a> {
    backing: SourceBacking<'a>,
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
        Ok(Self {
            backing: SourceBacking::Live(root),
            generation,
        })
    }

    /// Binds retained process-local snapshots to matching immutable generation data.
    ///
    /// Snapshots must be strictly ordered by [`SourceSnapshot::file`]. The
    /// retained path preserves immutable first-slice reads without changing
    /// [`Self::new`]'s live-VFS, fail-closed behavior.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::InvalidSnapshotSet`] when snapshots are not
    /// strictly ordered by file identity.
    pub fn from_snapshots(
        snapshots: &'a [Arc<SourceSnapshot>],
        generation: &'a GenerationSnapshot,
    ) -> Result<Self, SourceError> {
        if snapshots
            .windows(2)
            .any(|pair| pair[0].file() >= pair[1].file())
        {
            return Err(SourceError::InvalidSnapshotSet);
        }
        Ok(Self {
            backing: SourceBacking::Retained(snapshots),
            generation,
        })
    }

    /// Reads verified UTF-8 text with line context or exact source bytes.
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
        self.read_internal(
            references,
            options,
            budget,
            cancellation,
            SourceReadMode::Legacy(options.encoding),
        )
        .map(|read| read.result)
    }

    fn read_internal(
        &self,
        references: &[SourceRef],
        options: SourceReadOptions,
        budget: SourceBudget,
        cancellation: &Cancellation,
        mode: SourceReadMode,
    ) -> Result<RawSourceRead, SourceError> {
        validate_budget(budget)?;
        if references.is_empty() || references.len() > budget.max_selectors {
            return Err(SourceError::SelectorLimit);
        }
        if options.context_lines_before > budget.max_context_lines
            || options.context_lines_after > budget.max_context_lines
            || matches!(options.encoding, SourceEncoding::Bytes)
                && (options.context_lines_before != 0 || options.context_lines_after != 0)
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
            mode,
            budget,
            &control,
        )?;
        let mut snapshots = Vec::<ResolvedSnapshot<'_>>::new();
        try_reserve_vec(&mut snapshots, prepared.files.len(), &control)?;
        let mut file_encodings = Vec::new();
        if mode.is_lossless_text() {
            try_reserve_vec(&mut file_encodings, prepared.files.len(), &control)?;
        }
        for prepared_file in &prepared.files {
            let file = self
                .generation
                .find_file(prepared_file.file)
                .ok_or(SourceError::FileNotFound)?;
            let snapshot = self.resolve_snapshot(prepared_file, file, cancellation, &control)?;
            let snapshot_ref = snapshot.as_ref();
            if snapshot_ref.file() != file.id
                || snapshot_ref.metadata().length != file.byte_length
                || snapshot_ref.content_hash() != file.content_hash
            {
                return Err(SourceError::StaleSource);
            }
            if mode.is_lossless_text() {
                let encoding =
                    lossless_text::validate_snapshot(file, snapshot_ref.content(), &control)?;
                push_preallocated(&mut file_encodings, encoding, &control)?;
            } else if matches!(options.encoding, SourceEncoding::Utf8) {
                ensure_utf8(file, snapshot_ref.content(), &control)?;
            }
            push_preallocated(&mut snapshots, snapshot, &control)?;
        }

        let mut chunks = Vec::new();
        try_reserve_vec(&mut chunks, references.len(), &control)?;
        let mut text_encodings = Vec::new();
        if mode.is_lossless_text() {
            try_reserve_vec(&mut text_encodings, references.len(), &control)?;
        }
        let mut owned_response_bytes = 0usize;
        let mut total_source_bytes = 0usize;
        let total_metadata_bytes = prepared.metadata_bytes;
        let mut total_response_memory_bytes = total_metadata_bytes;
        let mut prefix_presentation_bytes = 0usize;
        let mut truncated = false;

        for (reference, file_slot) in references.iter().zip(prepared.file_slots) {
            control.check()?;
            let prepared_file = prepared
                .files
                .get(file_slot)
                .ok_or(SourceError::FileNotFound)?;
            let file = self
                .generation
                .find_file(prepared_file.file)
                .ok_or(SourceError::FileNotFound)?;
            let snapshot = snapshots
                .get(file_slot)
                .map(ResolvedSnapshot::as_ref)
                .ok_or(SourceError::ReadFailed)?;
            let physical_encoding = file_encodings.get(file_slot).copied();
            let range = if let SourceReadMode::LosslessTextPrefix(maximum_text_bytes)
            | SourceReadMode::LosslessTextContextPrefix(maximum_text_bytes) = mode
            {
                let encoding = physical_encoding.ok_or(SourceError::EncodingUnsupported)?;
                let prefix = if matches!(mode, SourceReadMode::LosslessTextContextPrefix(_)) {
                    lossless_text::bounded_text_context_prefix(
                        snapshot.content(),
                        reference,
                        options,
                        maximum_text_bytes,
                        encoding,
                        &control,
                    )?
                } else {
                    lossless_text::bounded_text_prefix(
                        snapshot.content(),
                        reference,
                        maximum_text_bytes,
                        encoding,
                        &control,
                    )?
                };
                truncated |= prefix.truncated;
                if physical_encoding != Some(rootlight_vfs::TextEncoding::Utf8) {
                    prefix_presentation_bytes = prefix_presentation_bytes
                        .checked_add(prefix.text_bytes)
                        .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
                }
                prefix.range
            } else if mode.is_lossless_text() {
                lossless_text::expand_text_context(
                    snapshot.content(),
                    reference,
                    options,
                    physical_encoding.ok_or(SourceError::EncodingUnsupported)?,
                    &control,
                )?
            } else {
                match options.encoding {
                    SourceEncoding::Utf8 => {
                        let expanded =
                            expand_context(snapshot.content(), reference, options, &control)?;
                        ExpandedRange {
                            start: expanded.start,
                            end: expanded.end,
                            start_line: expanded.start_line,
                            end_line: expanded.end_line,
                        }
                    }
                    SourceEncoding::Bytes => ExpandedRange {
                        start: usize::try_from(reference.span().start_byte())
                            .map_err(|_| SourceError::InvalidSourceSpan)?,
                        end: usize::try_from(reference.span().end_byte())
                            .map_err(|_| SourceError::InvalidSourceSpan)?,
                        start_line: None,
                        end_line: None,
                    },
                }
            };
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
                .and_then(|bytes| bytes.checked_add(prefix_presentation_bytes))
                .ok_or(SourceError::ResponseMemoryBudgetExceeded)?;
            if next_response_memory_bytes > budget.max_response_memory_bytes {
                return Err(SourceError::ResponseMemoryBudgetExceeded);
            }
            let (path, language, bytes) = if mode.is_lossless_text() {
                let path = lossless_text::clone_string(
                    &file.path,
                    &mut owned_response_bytes,
                    budget.max_response_memory_bytes,
                    &control,
                )?;
                let language = lossless_text::clone_string(
                    &file.language,
                    &mut owned_response_bytes,
                    budget.max_response_memory_bytes,
                    &control,
                )?;
                let bytes = lossless_text::clone_bytes(
                    selected,
                    &mut owned_response_bytes,
                    budget.max_response_memory_bytes,
                    &control,
                )?;
                push_preallocated(
                    &mut text_encodings,
                    physical_encoding.ok_or(SourceError::EncodingUnsupported)?,
                    &control,
                )?;
                (path, language, bytes)
            } else {
                (
                    try_clone_string(&file.path, &control)?,
                    try_clone_string(&file.language, &control)?,
                    try_clone_bytes(selected, &control)?,
                )
            };
            total_source_bytes = next_source_bytes;
            total_response_memory_bytes = next_response_memory_bytes;
            push_preallocated(
                &mut chunks,
                SourceChunk {
                    extracted_from: prepared_file.extracted_from.clone(),
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
                    encoding: if mode.is_lossless_text() {
                        if physical_encoding == Some(rootlight_vfs::TextEncoding::Utf8) {
                            SourceEncoding::Utf8
                        } else {
                            SourceEncoding::Bytes
                        }
                    } else {
                        options.encoding
                    },
                    trust: SourceTrust::UntrustedRepositoryData,
                },
                &control,
            )?;
        }
        control.check()?;

        Ok(RawSourceRead {
            result: SourceReadResult {
                generation: metadata.generation(),
                chunks,
                total_source_bytes,
                total_metadata_bytes,
                total_response_memory_bytes: if mode.is_lossless_text() {
                    owned_response_bytes
                } else {
                    total_response_memory_bytes
                },
            },
            text_encodings,
            deadline: control.deadline,
            truncated,
        })
    }

    fn preflight(
        &self,
        references: &[SourceRef],
        repository: RepositoryId,
        generation: GenerationId,
        mode: SourceReadMode,
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
            let file = self
                .generation
                .find_file(reference.span().file())
                .ok_or(SourceError::FileNotFound)?;
            validate_file(file, reference)?;
            if mode.is_lossless_text() {
                lossless_text::physical_encoding(file)?;
            } else if matches!(mode, SourceReadMode::Legacy(SourceEncoding::Utf8)) {
                validate_encoding(file)?;
            }
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
            // A valid prefix may be empty when the first scalar does not fit.
            // Admit its actual output only after whole-file and endpoint checks.
            let minimum_selected_bytes = if matches!(
                mode,
                SourceReadMode::LosslessTextPrefix(_)
                    | SourceReadMode::LosslessTextContextPrefix(_)
            ) {
                0
            } else {
                selected_bytes
            };
            minimum_source_bytes = minimum_source_bytes
                .checked_add(minimum_selected_bytes)
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
                let path = control.controlled(|| self.validated_path(file))?;
                let file_slot = files.len();
                push_preallocated(
                    &mut files,
                    PreparedFile {
                        file: file.id,
                        path,
                        extracted_from: None,
                    },
                    control,
                )?;
                file_slot
            };
            push_preallocated(&mut file_slots, file_slot, control)?;
        }

        // The validated generation guarantees unique, whole-file extraction
        // ownership. Scan once per request, retaining only selected origins.
        for mapping in &self.generation.document().source_mappings {
            control.check()?;
            if mapping.kind == SourceMappingKind::ExtractedFromContainer {
                for file in &mut files {
                    if file.file == mapping.from.span().file() {
                        file.extracted_from = Some(mapping.to.clone());
                    }
                }
            }
        }
        Ok(PreparedRequest {
            file_slots,
            files,
            metadata_bytes,
        })
    }

    fn validated_path(&self, file: &FileRecord) -> Result<RelativePath, SourceError> {
        let path = validated_path(file)?;
        match self.backing {
            SourceBacking::Live(root) if root.file_id(&path) == file.id => Ok(path),
            SourceBacking::Retained(snapshots) => {
                let snapshot = retained_snapshot(snapshots, file.id)?;
                if snapshot.path() == &path {
                    Ok(path)
                } else {
                    Err(SourceError::InvalidIndexedPath)
                }
            }
            SourceBacking::Live(_) => Err(SourceError::InvalidIndexedPath),
        }
    }

    fn resolve_snapshot<'service>(
        &'service self,
        prepared: &PreparedFile,
        file: &FileRecord,
        cancellation: &Cancellation,
        control: &SourceControl<'_>,
    ) -> Result<ResolvedSnapshot<'service>, SourceError> {
        match self.backing {
            SourceBacking::Live(root) => control
                .controlled(|| {
                    root.snapshot_cancellable(
                        &prepared.path,
                        file.byte_length.max(1),
                        cancellation,
                        control.deadline,
                    )
                    .map_err(map_snapshot_error)
                })
                .map(ResolvedSnapshot::Live),
            SourceBacking::Retained(snapshots) => {
                control.check()?;
                let snapshot = retained_snapshot(snapshots, file.id)?;
                control.check()?;
                Ok(ResolvedSnapshot::Retained(snapshot))
            }
        }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceReadMode {
    Legacy(SourceEncoding),
    LosslessText,
    LosslessTextPrefix(usize),
    LosslessTextContextPrefix(usize),
}

impl SourceReadMode {
    fn is_lossless_text(self) -> bool {
        matches!(
            self,
            Self::LosslessText | Self::LosslessTextPrefix(_) | Self::LosslessTextContextPrefix(_)
        )
    }
}

struct RawSourceRead {
    result: SourceReadResult,
    text_encodings: Vec<rootlight_vfs::TextEncoding>,
    deadline: Instant,
    truncated: bool,
}

struct PreparedFile {
    extracted_from: Option<SourceRef>,
    file: FileId,
    path: RelativePath,
}

enum SourceBacking<'a> {
    Live(&'a RepositoryRoot),
    Retained(&'a [Arc<SourceSnapshot>]),
}

// Live snapshots stay inline because boxing would add one infallible
// allocation per file after the service's fallible, hard-bounded reservations.
#[allow(clippy::large_enum_variant)]
enum ResolvedSnapshot<'a> {
    Live(SourceSnapshot),
    Retained(&'a SourceSnapshot),
}

impl ResolvedSnapshot<'_> {
    fn as_ref(&self) -> &SourceSnapshot {
        match self {
            Self::Live(snapshot) => snapshot,
            Self::Retained(snapshot) => snapshot,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ExpandedRange {
    start: usize,
    end: usize,
    start_line: Option<u64>,
    end_line: Option<u64>,
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

fn validated_path(file: &FileRecord) -> Result<RelativePath, SourceError> {
    let locator = file
        .path_locator
        .as_ref()
        .ok_or(SourceError::InvalidIndexedPath)?;
    let path = RelativePath::from_locator(locator).map_err(|_| SourceError::InvalidIndexedPath)?;
    if path.as_str() != file.path {
        return Err(SourceError::InvalidIndexedPath);
    }
    Ok(path)
}

fn retained_snapshot(
    snapshots: &[Arc<SourceSnapshot>],
    file: FileId,
) -> Result<&SourceSnapshot, SourceError> {
    snapshots
        .binary_search_by_key(&file, |snapshot| snapshot.file())
        .ok()
        .and_then(|index| snapshots.get(index))
        .map(Arc::as_ref)
        .ok_or(SourceError::FileNotFound)
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

    let start = if options.context_lines_before == 0 {
        selection_start
    } else {
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
        starts.front().copied().unwrap_or(0)
    };
    let start_line = count_lines_before(content, start, control)?;

    let mut end = selection_end;
    if options.context_lines_after > 0 {
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
        start_line: Some(start_line),
        end_line: Some(end_line),
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
    /// No generation-owned file record or retained snapshot matched a selector.
    #[error("source file was not found in the pinned generation")]
    FileNotFound,
    /// Persisted path identity was not canonical or did not match the file ID.
    #[error("indexed source path identity is invalid")]
    InvalidIndexedPath,
    /// Retained snapshots were not strictly ordered by file identity.
    #[error("retained source snapshot set is invalid")]
    InvalidSnapshotSet,
    /// Indexed and resolved immutable source identities differed.
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
    use rootlight_storage::{GENERATION_CONTRACT_VERSION, GenerationMetadata};
    use std::{cell::Cell, fs, path::Path};
    use tempfile::tempdir_in;

    struct Fixture {
        _temporary: tempfile::TempDir,
        root: RepositoryRoot,
        generation: GenerationSnapshot,
        reference: SourceRef,
    }

    #[test]
    fn source_metadata_validation_does_not_read_or_validate_body_bytes() {
        let fixture = fixture(&[0xff, 0xfe, 0x61], "utf-16le", (0, 3));
        fs::remove_file(fixture._temporary.path().join("src/sample.rs")).unwrap();
        let references = [fixture.reference.clone()];
        let options = LosslessTextReadOptions::new()
            .with_context_lines_before(1)
            .with_context_lines_after(1);
        let metadata = validate_source_metadata(
            &fixture.generation,
            &references,
            options,
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].reference(), &fixture.reference);
        assert_eq!(
            metadata[0].file().content_hash,
            fixture.reference.content_hash()
        );
        assert_eq!(
            metadata[0].physical_encoding().unwrap(),
            rootlight_vfs::TextEncoding::Utf16Le
        );
        assert!(metadata[0].path_memory_upper() > metadata[0].file().path.len());
    }

    #[test]
    fn source_metadata_validation_rejects_stale_foreign_duplicate_and_path_drift() {
        let fixture = fixture(b"text", "utf-8", (0, 4));
        let original = &fixture.reference;
        let options = LosslessTextReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0);
        let check = |references: &[SourceRef]| {
            validate_source_metadata(
                &fixture.generation,
                references,
                options,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .map(|_| ())
        };
        let wrong_hash = SourceRef::new(
            original.repository(),
            original.generation(),
            original.span(),
            content_hash(b"different"),
            None,
        );
        assert_eq!(check(&[wrong_hash]), Err(SourceError::StaleSource));
        let wrong_range = SourceRef::new(
            original.repository(),
            original.generation(),
            SourceSpan::new(original.span().file(), 0, 5).unwrap(),
            original.content_hash(),
            None,
        );
        assert_eq!(check(&[wrong_range]), Err(SourceError::StaleSource));
        let foreign = SourceRef::new(
            derive_repository(b"foreign-metadata").id(),
            original.generation(),
            original.span(),
            original.content_hash(),
            None,
        );
        assert_eq!(check(&[foreign]), Err(SourceError::RepositoryMismatch));
        assert_eq!(
            check(&[original.clone(), original.clone()]),
            Err(SourceError::SelectorLimit)
        );
        let mut document = fixture.generation.document().clone();
        document.files[0].path = "src/other.rs".to_owned();
        let changed = GenerationSnapshot::new(
            fixture.generation.metadata(),
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        assert_eq!(
            validate_source_metadata(
                &changed,
                std::slice::from_ref(original),
                options,
                SourceBudget::new(),
                &Cancellation::new()
            )
            .map(|_| ()),
            Err(SourceError::InvalidIndexedPath)
        );
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
            format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
                | u32::from(GENERATION_CONTRACT_VERSION.minor()),
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

    fn read_lossless(
        fixture: &Fixture,
        references: &[SourceRef],
        options: LosslessTextReadOptions,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextReadResult, SourceError> {
        SourceService::new(&fixture.root, &fixture.generation)
            .expect("fixture capability and generation match")
            .read_lossless_text(references, options, budget, cancellation)
    }

    fn read_prefix(
        fixture: &Fixture,
        references: &[SourceRef],
        maximum_text_bytes: usize,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextPrefixReadResult, SourceError> {
        SourceService::new(&fixture.root, &fixture.generation)
            .expect("fixture capability and generation match")
            .read_lossless_text_prefix(references, maximum_text_bytes, budget, cancellation)
    }

    fn read_prefix_with_options(
        fixture: &Fixture,
        references: &[SourceRef],
        options: LosslessTextReadOptions,
        maximum_text_bytes: usize,
        budget: SourceBudget,
        cancellation: &Cancellation,
    ) -> Result<SourceTextPrefixReadResult, SourceError> {
        SourceService::new(&fixture.root, &fixture.generation)
            .expect("fixture capability and generation match")
            .read_lossless_text_prefix_with_options(
                references,
                options,
                maximum_text_bytes,
                budget,
                cancellation,
            )
    }

    fn encode_lossless_fragment(text: &str, encoding: rootlight_vfs::TextEncoding) -> Vec<u8> {
        match encoding {
            rootlight_vfs::TextEncoding::Utf8 => text.as_bytes().to_vec(),
            rootlight_vfs::TextEncoding::Utf16Le | rootlight_vfs::TextEncoding::Utf16Be => text
                .encode_utf16()
                .flat_map(|unit| match encoding {
                    rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                    rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                    rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
                })
                .collect(),
        }
    }

    fn no_text_context() -> LosslessTextReadOptions {
        LosslessTextReadOptions::new()
            .with_context_lines_before(0)
            .with_context_lines_after(0)
    }

    fn physical_utf16(body: &str, encoding: rootlight_vfs::TextEncoding) -> Vec<u8> {
        std::iter::once(0xfeff)
            .chain(body.encode_utf16())
            .flat_map(|unit| match encoding {
                rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                rootlight_vfs::TextEncoding::Utf8 => panic!("UTF-16 fixture requires byte order"),
            })
            .collect()
    }

    fn selected_reference(fixture: &Fixture, start: usize, end: usize) -> SourceRef {
        SourceRef::new(
            fixture.reference.repository(),
            fixture.reference.generation(),
            SourceSpan::new(
                fixture.reference.span().file(),
                u64::try_from(start).expect("fixture start fits u64"),
                u64::try_from(end).expect("fixture end fits u64"),
            )
            .expect("fixture selection is ordered"),
            fixture.reference.content_hash(),
            None,
        )
    }

    #[test]
    fn lossless_utf8_borrows_exact_legacy_chunks() {
        let fixture = fixture(b"zero\r\none\nTWO\nthree\nfour\n", "utf8", (10, 13));
        let cancellation = Cancellation::new();
        let legacy = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new()
                .with_context_lines_before(1)
                .with_context_lines_after(1),
            SourceBudget::new(),
            &cancellation,
        )
        .expect("legacy UTF-8 source is readable");
        let text = read_lossless(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            LosslessTextReadOptions::new()
                .with_context_lines_before(1)
                .with_context_lines_after(1),
            SourceBudget::new(),
            &cancellation,
        )
        .expect("lossless UTF-8 source is readable");
        assert_eq!(text.generation(), legacy.generation);
        assert_eq!(text.total_source_bytes(), legacy.total_source_bytes);
        assert_eq!(text.total_metadata_bytes(), legacy.total_metadata_bytes);
        assert_eq!(text.chunks().len(), 1);
        let chunk = &text.chunks()[0];
        assert_eq!(chunk.raw(), &legacy.chunks[0]);
        assert_eq!(chunk.text(), "one\nTWO\nthree\n");
        assert_eq!(chunk.text().as_ptr(), chunk.raw().bytes.as_ptr());
        assert_eq!(chunk.physical_encoding(), rootlight_vfs::TextEncoding::Utf8);
        assert_eq!(chunk.owned_presentation_bytes(), 0);
        assert_eq!(text.total_text_bytes(), chunk.raw().bytes.len());
        assert_eq!(
            text.total_response_memory_bytes(),
            chunk.raw().path.capacity()
                + chunk.raw().language.capacity()
                + chunk.raw().bytes.capacity()
        );
        let (raw, encoding, presentation) = text.into_chunks().remove(0).into_parts();
        assert_eq!(raw, legacy.chunks[0]);
        assert_eq!(encoding, rootlight_vfs::TextEncoding::Utf8);
        assert_eq!(presentation, None);
    }

    #[test]
    fn lossless_context_prefix_preserves_expanded_scalar_and_line_geometry() {
        let text = "\u{feff}head\u{010a}\r\nA\u{2603}\u{1f642}B\r\nend\r\ntail";
        let selection_start = text.find('\u{2603}').unwrap();
        let selection_end = selection_start + "\u{2603}\u{1f642}".len();
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let reference = selected_reference(
                &fixture,
                encode_lossless_fragment(&text[..selection_start], encoding).len(),
                encode_lossless_fragment(&text[..selection_end], encoding).len(),
            );
            for (before, after, expanded_start, expanded_end) in [
                (1, 1, 0, text.find("tail").unwrap()),
                (0, 1, selection_start, text.find("tail").unwrap()),
                (1, 0, 0, selection_end),
                (0, 0, selection_start, selection_end),
            ] {
                let options = no_text_context()
                    .with_context_lines_before(before)
                    .with_context_lines_after(after);
                let expanded = &text[expanded_start..expanded_end];
                for maximum in 1..=expanded.len() + 1 {
                    let end = expanded
                        .char_indices()
                        .map(|(start, character)| start + character.len_utf8())
                        .take_while(|end| *end <= maximum)
                        .last()
                        .unwrap_or(0);
                    let expected = &expanded[..end];
                    let result = read_prefix_with_options(
                        &fixture,
                        std::slice::from_ref(&reference),
                        options,
                        maximum,
                        SourceBudget::new(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                    let chunk = &result.chunks()[0];
                    assert_eq!(chunk.text(), expected);
                    assert_eq!(chunk.raw().reference, reference);
                    assert_eq!(result.truncated(), end < expanded.len());
                    let encoded = encode_lossless_fragment(expected, encoding);
                    let physical_start =
                        encode_lossless_fragment(&text[..expanded_start], encoding).len();
                    let physical_end = physical_start + encoded.len();
                    assert_eq!(chunk.raw().bytes, encoded);
                    assert_eq!(chunk.raw().bytes, original[physical_start..physical_end]);
                    assert_eq!(
                        chunk.raw().start_byte,
                        u64::try_from(physical_start).unwrap()
                    );
                    assert_eq!(chunk.raw().end_byte, u64::try_from(physical_end).unwrap());
                    let start_line = 1 + u64::try_from(
                        text[..expanded_start]
                            .chars()
                            .filter(|character| *character == '\n')
                            .count(),
                    )
                    .unwrap();
                    let end_line = start_line
                        + u64::try_from(
                            expected
                                .chars()
                                .filter(|character| *character == '\n')
                                .count()
                                .saturating_sub(usize::from(expected.ends_with('\n'))),
                        )
                        .unwrap();
                    assert_eq!(
                        (chunk.raw().start_line, chunk.raw().end_line),
                        (Some(start_line), Some(end_line))
                    );
                    assert_eq!(chunk.raw().content_hash, content_hash(&original));
                    assert_eq!(chunk.physical_encoding(), encoding);
                    assert_eq!(result.total_source_bytes(), encoded.len());
                    assert_eq!(result.total_text_bytes(), expected.len());
                    if encoding == rootlight_vfs::TextEncoding::Utf8 {
                        assert_eq!(chunk.owned_presentation_bytes(), 0);
                        assert_eq!(chunk.text().as_ptr(), chunk.raw().bytes.as_ptr());
                    }
                }
                let exact = read_lossless(
                    &fixture,
                    std::slice::from_ref(&reference),
                    options,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
                let (inner, truncated) = read_prefix_with_options(
                    &fixture,
                    std::slice::from_ref(&reference),
                    options,
                    expanded.len() + 1,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap()
                .into_parts();
                assert_eq!(inner, exact);
                assert!(!truncated);
            }
        }
    }

    #[test]
    fn lossless_context_prefix_reports_shortened_context_when_selection_fits() {
        let text = "\u{feff}A\r\nnext\u{2603}\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let start = encode_lossless_fragment("\u{feff}", encoding).len();
            let end = encode_lossless_fragment("\u{feff}A", encoding).len();
            let reference = selected_reference(&fixture, start, end);
            let result = read_prefix_with_options(
                &fixture,
                std::slice::from_ref(&reference),
                no_text_context().with_context_lines_after(1),
                1,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            let chunk = &result.chunks()[0];
            assert_eq!(chunk.text(), "A");
            assert_eq!(chunk.raw().reference, reference);
            assert_eq!(chunk.raw().end_byte, reference.span().end_byte());
            assert_eq!(
                (chunk.raw().start_line, chunk.raw().end_line),
                (Some(1), Some(1))
            );
            assert!(
                result.truncated(),
                "omitted context is not false completeness"
            );
            let zero_context = read_prefix(
                &fixture,
                std::slice::from_ref(&reference),
                1,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            assert!(!zero_context.truncated());
            assert_eq!(
                (
                    zero_context.chunks()[0].raw().start_line,
                    zero_context.chunks()[0].raw().end_line
                ),
                (None, None)
            );
        }
    }

    #[test]
    fn lossless_context_prefix_preserves_empty_approved_ranges() {
        let text = "\u{feff}first\r\n\u{2603}\u{1f642}\r\nend";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            for text_offset in [
                0,
                "\u{feff}".len(),
                text.find('\u{2603}').unwrap(),
                text.len(),
            ] {
                let offset = encode_lossless_fragment(&text[..text_offset], encoding).len();
                let reference = selected_reference(&fixture, offset, offset);
                let result = read_prefix_with_options(
                    &fixture,
                    std::slice::from_ref(&reference),
                    no_text_context(),
                    1,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
                let chunk = &result.chunks()[0];
                let line = 1 + u64::try_from(
                    text[..text_offset]
                        .chars()
                        .filter(|character| *character == '\n')
                        .count(),
                )
                .unwrap();
                assert_eq!(chunk.text(), "");
                assert_eq!(chunk.raw().reference, reference);
                assert_eq!(chunk.raw().start_byte, u64::try_from(offset).unwrap());
                assert_eq!(chunk.raw().end_byte, u64::try_from(offset).unwrap());
                assert_eq!(
                    (chunk.raw().start_line, chunk.raw().end_line),
                    (Some(line), Some(line))
                );
                assert!(!result.truncated());
                let options = no_text_context()
                    .with_context_lines_before(1)
                    .with_context_lines_after(1);
                let exact = read_lossless(
                    &fixture,
                    std::slice::from_ref(&reference),
                    options,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
                let prefix = read_prefix_with_options(
                    &fixture,
                    std::slice::from_ref(&reference),
                    options,
                    text.len(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
                let (inner, truncated) = prefix.into_parts();
                assert_eq!(inner, exact);
                assert!(!truncated);
            }
        }
    }

    #[test]
    fn lossless_context_prefix_validates_original_endpoints_before_clipping() {
        let text = "\u{feff}a\r\nx\u{1f642}z\r\nend";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let start = encode_lossless_fragment(&text[..text.find('x').unwrap()], encoding).len();
            let scalar =
                encode_lossless_fragment(&text[..text.find('\u{1f642}').unwrap()], encoding).len();
            let options = no_text_context()
                .with_context_lines_before(1)
                .with_context_lines_after(1);
            for invalid in [scalar + 1, scalar + 2, scalar + 3] {
                for (start, end) in [
                    (start, invalid),
                    (invalid, original.len()),
                    (invalid, invalid),
                ] {
                    let reference = selected_reference(&fixture, start, end);
                    assert_eq!(
                        read_prefix_with_options(
                            &fixture,
                            std::slice::from_ref(&reference),
                            options,
                            1,
                            SourceBudget::new(),
                            &Cancellation::new()
                        ),
                        Err(SourceError::InvalidSourceSpan)
                    );
                }
            }
            let reference = selected_reference(&fixture, start, original.len());
            let result = read_prefix_with_options(
                &fixture,
                std::slice::from_ref(&reference),
                options,
                1,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(result.chunks()[0].raw().reference, reference);
            assert_eq!(result.chunks()[0].text(), "");
            assert!(result.truncated());
        }
    }

    #[test]
    fn lossless_context_prefix_validates_malformed_unselected_tails() {
        let text = "\u{feff}a\r\n\u{2603}\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let valid = encode_lossless_fragment(text, encoding);
            let end = encode_lossless_fragment("\u{feff}a", encoding).len();
            let mut malformed = Vec::new();
            let mut odd = valid.clone();
            odd.push(0xff);
            malformed.push(odd);
            if encoding != rootlight_vfs::TextEncoding::Utf8 {
                for unit in [0xd800u16, 0xdc00, 0] {
                    let mut bytes = valid.clone();
                    bytes.extend_from_slice(&match encoding {
                        rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                        rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                        rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
                    });
                    malformed.push(bytes);
                }
            }
            for bytes in malformed {
                let fixture = fixture(&bytes, encoding.as_str(), (0, u64::try_from(end).unwrap()));
                assert_eq!(
                    read_prefix_with_options(
                        &fixture,
                        std::slice::from_ref(&fixture.reference),
                        no_text_context().with_context_lines_after(1),
                        1,
                        SourceBudget::new(),
                        &Cancellation::new()
                    ),
                    Err(SourceError::EncodingUnsupported)
                );
            }
        }
        let fixture = fixture(b"a\0b\r\nnext", "utf-8", (1, 2));
        let prefix = read_prefix_with_options(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            no_text_context().with_context_lines_after(1),
            1,
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(prefix.chunks()[0].text(), "\0");
        assert_eq!(prefix.chunks()[0].raw().bytes, b"\0");
        assert!(prefix.truncated());
    }

    #[test]
    fn lossless_context_prefix_duplicates_conserve_actual_resource_admission() {
        let text = "\u{feff}\u{2603}\u{1f642}\r\nnext\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let length = u64::try_from(original.len()).unwrap();
            let fixture = fixture(&original, encoding.as_str(), (0, length));
            let start = encode_lossless_fragment("\u{feff}", encoding).len();
            let end = start + encode_lossless_fragment("\u{2603}", encoding).len();
            let reference = selected_reference(&fixture, start, end);
            let references = [reference.clone(), reference.clone()];
            let options = no_text_context().with_context_lines_after(1);
            let result = read_prefix_with_options(
                &fixture,
                &references,
                options,
                7,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            let physical: usize = result
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().bytes.len())
                .sum();
            let metadata: usize = result
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().path.len() + chunk.raw().language.len())
                .sum();
            let text_bytes: usize = result.chunks().iter().map(|chunk| chunk.text().len()).sum();
            let owned: usize = result
                .chunks()
                .iter()
                .map(|chunk| {
                    chunk.raw().path.capacity()
                        + chunk.raw().language.capacity()
                        + chunk.raw().bytes.capacity()
                        + chunk.owned_presentation_bytes()
                })
                .sum();
            assert_eq!(result.total_source_bytes(), physical);
            assert_eq!(result.total_metadata_bytes(), metadata);
            assert_eq!(result.total_text_bytes(), text_bytes);
            assert_eq!(result.total_response_memory_bytes(), owned);
            assert_eq!(text_bytes, 14);
            assert!(result.truncated());
            assert_eq!(result.chunks().len(), 2);
            for chunk in result.chunks() {
                assert_eq!(chunk.text(), "\u{2603}\u{1f642}");
                assert_eq!(chunk.raw().reference, reference);
                assert_eq!(
                    chunk.raw().bytes,
                    encode_lossless_fragment(chunk.text(), encoding)
                );
                assert_eq!(
                    (chunk.raw().start_line, chunk.raw().end_line),
                    (Some(1), Some(1))
                );
            }
            let exact = SourceBudget::new()
                .with_max_source_bytes(physical)
                .with_max_metadata_bytes(metadata)
                .with_max_response_memory_bytes(owned)
                .with_max_snapshot_bytes(length);
            assert_eq!(
                read_prefix_with_options(
                    &fixture,
                    &references,
                    options,
                    7,
                    exact,
                    &Cancellation::new()
                )
                .unwrap(),
                result
            );
            for (budget, expected) in [
                (
                    exact.with_max_source_bytes(physical - 1),
                    SourceError::SourceBudgetExceeded,
                ),
                (
                    exact.with_max_metadata_bytes(metadata - 1),
                    SourceError::MetadataBudgetExceeded,
                ),
                (
                    exact.with_max_response_memory_bytes(owned - 1),
                    SourceError::ResponseMemoryBudgetExceeded,
                ),
                (
                    exact.with_max_snapshot_bytes(length - 1),
                    SourceError::SnapshotBudgetExceeded,
                ),
            ] {
                assert_eq!(
                    read_prefix_with_options(
                        &fixture,
                        &references,
                        options,
                        7,
                        budget,
                        &Cancellation::new()
                    ),
                    Err(expected)
                );
            }
        }
    }

    #[test]
    fn lossless_context_prefix_retains_selector_context_and_cancellation_controls() {
        let fixture = fixture(b"head\r\nbody\r\ntail", "utf-8", (6, 10));
        let references = std::slice::from_ref(&fixture.reference);
        let options = no_text_context()
            .with_context_lines_before(1)
            .with_context_lines_after(1);
        for maximum in [0, HARD_MAX_SOURCE_BYTES + 1] {
            assert_eq!(
                read_prefix_with_options(
                    &fixture,
                    references,
                    options,
                    maximum,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::InvalidBudget)
            );
        }
        assert_eq!(
            read_prefix_with_options(
                &fixture,
                &[],
                options,
                1,
                SourceBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceError::SelectorLimit)
        );
        let duplicates = [fixture.reference.clone(), fixture.reference.clone()];
        assert_eq!(
            read_prefix_with_options(
                &fixture,
                &duplicates,
                options,
                1,
                SourceBudget::new().with_max_selectors(1),
                &Cancellation::new()
            ),
            Err(SourceError::SelectorLimit)
        );
        assert_eq!(
            read_prefix_with_options(
                &fixture,
                references,
                options,
                1,
                SourceBudget::new().with_max_context_lines(0),
                &Cancellation::new()
            ),
            Err(SourceError::ContextLimit)
        );
        assert_eq!(
            read_prefix_with_options(
                &fixture,
                references,
                options,
                1,
                SourceBudget::new().with_max_source_bytes(0),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidBudget)
        );
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            read_prefix_with_options(
                &fixture,
                references,
                options,
                1,
                SourceBudget::new(),
                &cancellation
            ),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
    }

    #[test]
    fn lossless_context_prefix_rejects_stale_identity_outside_returned_bytes() {
        let text = "\u{feff}A\r\nnext\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = encode_lossless_fragment(text, encoding);
            let end = encode_lossless_fragment("\u{feff}A", encoding).len();
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(end).unwrap()),
            );
            let options = no_text_context().with_context_lines_after(1);
            let wrong_hash = SourceRef::new(
                fixture.reference.repository(),
                fixture.reference.generation(),
                fixture.reference.span(),
                content_hash(b"different source"),
                None,
            );
            assert_eq!(
                read_prefix_with_options(
                    &fixture,
                    std::slice::from_ref(&wrong_hash),
                    options,
                    1,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::StaleSource)
            );
            let changed = encode_lossless_fragment("\u{feff}A\r\nnext\r\nfail", encoding);
            assert_eq!(original.len(), changed.len());
            fs::write(fixture._temporary.path().join("src/sample.rs"), changed).unwrap();
            assert_eq!(
                read_prefix_with_options(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    options,
                    1,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::StaleSource)
            );
        }
    }

    #[test]
    fn lossless_context_prefix_cancellation_during_expansion_precedes_copy() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original =
                encode_lossless_fragment("\u{feff}a\r\n\u{2603}\u{1f642}\r\ntail", encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let cancellation = Cancellation::new();
            let operations = Cell::new(0usize);
            let hook = || {
                operations.set(operations.get() + 1);
                let _ = cancellation.cancel(CancellationReason::ParentCancelled);
            };
            let control = SourceControl::new(&cancellation, Duration::from_secs(1))
                .with_after_operation(&hook);
            let options = SourceReadOptions::new()
                .with_context_lines_before(1)
                .with_context_lines_after(1);
            assert_eq!(
                lossless_text::bounded_text_context_prefix(
                    &original,
                    &fixture.reference,
                    options,
                    1,
                    encoding,
                    &control
                )
                .err(),
                Some(SourceError::Cancelled(CancellationReason::ParentCancelled))
            );
            assert_eq!(
                operations.get(),
                1,
                "context allocation is interrupted before source copying"
            );
            let uncancelled = Cancellation::new();
            let expired = SourceControl {
                cancellation: &uncancelled,
                deadline: Instant::now(),
                after_operation: None,
            };
            assert_eq!(
                lossless_text::bounded_text_context_prefix(
                    &original,
                    &fixture.reference,
                    options,
                    1,
                    encoding,
                    &expired
                )
                .err(),
                Some(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
            );
        }
    }

    #[test]
    fn lossless_prefix_preserves_scalar_boundaries_and_original_identity() {
        let body = "A\u{2603}\u{1f600}\r\nZ";
        let text = format!("\u{feff}{body}");
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                text.as_bytes().to_vec()
            } else {
                physical_utf16(body, encoding)
            };
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            for maximum in 1..=text.len() + 1 {
                let expected_end = text
                    .char_indices()
                    .map(|(start, character)| start + character.len_utf8())
                    .take_while(|end| *end <= maximum)
                    .last()
                    .unwrap_or(0);
                let expected = &text[..expected_end];
                let result = read_prefix(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    maximum,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
                let chunk = &result.chunks()[0];
                let encoded = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                    expected.as_bytes().to_vec()
                } else {
                    expected
                        .encode_utf16()
                        .flat_map(|unit| match encoding {
                            rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                            rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                            rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
                        })
                        .collect()
                };
                assert_eq!(chunk.text(), expected);
                assert_eq!(chunk.raw().reference, fixture.reference);
                assert_eq!(chunk.raw().bytes, encoded);
                assert_eq!(chunk.raw().bytes, original[..encoded.len()]);
                assert_eq!(chunk.raw().start_byte, 0);
                assert_eq!(chunk.raw().end_byte, u64::try_from(encoded.len()).unwrap());
                assert_eq!(chunk.raw().content_hash, content_hash(&original));
                assert_eq!((chunk.raw().start_line, chunk.raw().end_line), (None, None));
                assert_eq!(chunk.physical_encoding(), encoding);
                assert_eq!(result.total_source_bytes(), encoded.len());
                assert_eq!(result.total_text_bytes(), expected.len());
                assert_eq!(result.truncated(), expected.len() != text.len());
                assert_eq!(result.generation(), fixture.reference.generation());
            }
        }
    }

    #[test]
    fn lossless_prefix_midfile_admits_only_actual_returned_bytes() {
        let body = "head\u{2603}\u{1f600}\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                body.as_bytes().to_vec()
            } else {
                physical_utf16(body, encoding)
            };
            let start = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                4
            } else {
                10
            };
            let width = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                3
            } else {
                2
            };
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let reference = selected_reference(&fixture, start, original.len());
            let budget = SourceBudget::new().with_max_source_bytes(width);
            let prefix = read_prefix(
                &fixture,
                std::slice::from_ref(&reference),
                3,
                budget,
                &Cancellation::new(),
            )
            .expect("actual scalar prefix fits physical admission");
            let chunk = &prefix.chunks()[0];
            assert_eq!(chunk.text(), "\u{2603}");
            assert_eq!(chunk.raw().reference, reference);
            assert_eq!(chunk.raw().bytes, original[start..start + width]);
            assert_eq!(chunk.raw().start_byte, u64::try_from(start).unwrap());
            assert_eq!(chunk.raw().end_byte, u64::try_from(start + width).unwrap());
            assert_eq!(prefix.total_source_bytes(), width);
            assert_eq!(prefix.total_text_bytes(), 3);
            assert!(prefix.truncated());
            assert_eq!(
                read_lossless(
                    &fixture,
                    std::slice::from_ref(&reference),
                    no_text_context(),
                    budget,
                    &Cancellation::new()
                ),
                Err(SourceError::SourceBudgetExceeded)
            );
        }
    }

    #[test]
    fn lossless_prefix_empty_and_too_small_selections_preserve_valid_endpoints() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let body = "\u{1f600}\r\n";
            let original = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                format!("\u{feff}{body}").into_bytes()
            } else {
                physical_utf16(body, encoding)
            };
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            let bom_end = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                3
            } else {
                2
            };
            for start in [0, bom_end, original.len()] {
                let reference = selected_reference(&fixture, start, start);
                let prefix = read_prefix(
                    &fixture,
                    std::slice::from_ref(&reference),
                    1,
                    SourceBudget::new().with_max_source_bytes(1),
                    &Cancellation::new(),
                )
                .unwrap();
                let chunk = &prefix.chunks()[0];
                assert_eq!(chunk.raw().reference, reference);
                assert_eq!(chunk.raw().start_byte, u64::try_from(start).unwrap());
                assert_eq!(chunk.raw().end_byte, u64::try_from(start).unwrap());
                assert_eq!(chunk.text(), "");
                assert!(chunk.raw().bytes.is_empty());
                assert_eq!(prefix.total_source_bytes(), 0);
                assert_eq!(prefix.total_text_bytes(), 0);
                assert!(!prefix.truncated());
            }
            for start in [0, bom_end] {
                let reference = selected_reference(&fixture, start, original.len());
                let prefix = read_prefix(
                    &fixture,
                    std::slice::from_ref(&reference),
                    1,
                    SourceBudget::new().with_max_source_bytes(1),
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(prefix.chunks()[0].raw().reference, reference);
                assert_eq!(prefix.chunks()[0].text(), "");
                assert_eq!(
                    prefix.chunks()[0].raw().start_byte,
                    prefix.chunks()[0].raw().end_byte
                );
                assert!(prefix.truncated());
            }
        }
    }

    #[test]
    fn lossless_prefix_retains_utf8_nul_and_borrowed_presentation() {
        let original = b"a\0b\r\ntail";
        let fixture = fixture(
            original,
            "utf-8",
            (0, u64::try_from(original.len()).unwrap()),
        );
        let result = read_prefix(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            3,
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
        let chunk = &result.chunks()[0];
        assert_eq!(chunk.text(), "a\0b");
        assert_eq!(chunk.raw().bytes, original[..3]);
        assert_eq!(chunk.text().as_ptr(), chunk.raw().bytes.as_ptr());
        assert_eq!(chunk.owned_presentation_bytes(), 0);
        assert_eq!(chunk.raw().reference, fixture.reference);
        assert_eq!(result.total_source_bytes(), 3);
        assert_eq!(result.total_text_bytes(), 3);
        assert!(result.truncated());
    }

    #[test]
    fn lossless_prefix_rejects_invalid_ceiling_budget_and_selector_count() {
        let fixture = fixture(b"source", "utf-8", (0, 6));
        for maximum in [0, HARD_MAX_SOURCE_BYTES + 1] {
            assert_eq!(
                read_prefix(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    maximum,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::InvalidBudget)
            );
        }
        assert_eq!(
            read_prefix(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                1,
                SourceBudget::new().with_max_source_bytes(0),
                &Cancellation::new()
            ),
            Err(SourceError::InvalidBudget)
        );
        assert_eq!(
            read_prefix(&fixture, &[], 1, SourceBudget::new(), &Cancellation::new()),
            Err(SourceError::SelectorLimit)
        );
        let references = [fixture.reference.clone(), fixture.reference.clone()];
        assert_eq!(
            read_prefix(
                &fixture,
                &references,
                1,
                SourceBudget::new().with_max_selectors(1),
                &Cancellation::new()
            ),
            Err(SourceError::SelectorLimit)
        );
    }

    #[test]
    fn lossless_prefix_validates_approved_endpoints_beyond_clipped_output() {
        let original = "a\u{1f600}z";
        let utf8_fixture = fixture(
            original.as_bytes(),
            "utf-8",
            (0, u64::try_from(original.len()).unwrap()),
        );
        for (start, end) in [(0, 2), (0, 3), (0, 4), (2, 6), (2, 2)] {
            let reference = selected_reference(&utf8_fixture, start, end);
            assert_eq!(
                read_prefix(
                    &utf8_fixture,
                    std::slice::from_ref(&reference),
                    1,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::InvalidSourceSpan)
            );
        }
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = physical_utf16("a\u{1f600}z", encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (0, u64::try_from(original.len()).unwrap()),
            );
            for (start, end) in [(0, 1), (0, 3), (0, 6), (1, 10), (6, 10), (6, 6)] {
                let reference = selected_reference(&fixture, start, end);
                assert_eq!(
                    read_prefix(
                        &fixture,
                        std::slice::from_ref(&reference),
                        3,
                        SourceBudget::new(),
                        &Cancellation::new()
                    ),
                    Err(SourceError::InvalidSourceSpan)
                );
            }
        }
    }

    #[test]
    fn lossless_prefix_verifies_malformed_unselected_tails_and_physical_encoding() {
        let invalid_utf8 = fixture(b"a\xff", "utf-8", (0, 1));
        assert_eq!(
            read_prefix(
                &invalid_utf8,
                std::slice::from_ref(&invalid_utf8.reference),
                1,
                SourceBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        );
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let valid = physical_utf16("abc", encoding);
            let mut malformed = Vec::new();
            let mut odd = valid.clone();
            odd.push(1);
            malformed.push(odd);
            for tail in [vec![0xd800], vec![0xdc00], vec![0xd800, 0x0058], vec![0]] {
                let mut bytes = valid.clone();
                for unit in tail {
                    bytes.extend_from_slice(&match encoding {
                        rootlight_vfs::TextEncoding::Utf16Le => u16::to_le_bytes(unit),
                        rootlight_vfs::TextEncoding::Utf16Be => u16::to_be_bytes(unit),
                        rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
                    });
                }
                malformed.push(bytes);
            }
            let opposite = match encoding {
                rootlight_vfs::TextEncoding::Utf16Le => rootlight_vfs::TextEncoding::Utf16Be,
                rootlight_vfs::TextEncoding::Utf16Be => rootlight_vfs::TextEncoding::Utf16Le,
                rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
            };
            malformed.push(physical_utf16("abc", opposite));
            malformed.push(valid[2..].to_vec());
            for bytes in malformed {
                let fixture = fixture(&bytes, encoding.as_str(), (0, 2));
                assert_eq!(
                    read_prefix(
                        &fixture,
                        std::slice::from_ref(&fixture.reference),
                        1,
                        SourceBudget::new(),
                        &Cancellation::new()
                    ),
                    Err(SourceError::EncodingUnsupported)
                );
            }
        }
        let unsupported = fixture(b"text", "windows-1252", (0, 4));
        assert_eq!(
            read_prefix(
                &unsupported,
                std::slice::from_ref(&unsupported.reference),
                1,
                SourceBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        );
    }

    #[test]
    fn lossless_prefix_duplicate_chunks_conserve_physical_text_and_owned_costs() {
        let body = "\u{2603}\u{1f600}\r\ntail";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = if encoding == rootlight_vfs::TextEncoding::Utf8 {
                format!("\u{feff}{body}").into_bytes()
            } else {
                physical_utf16(body, encoding)
            };
            let length = u64::try_from(original.len()).unwrap();
            let fixture = fixture(&original, encoding.as_str(), (0, length));
            let references = [fixture.reference.clone(), fixture.reference.clone()];
            let result = read_prefix(
                &fixture,
                &references,
                7,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
            let physical: usize = result
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().bytes.len())
                .sum();
            let metadata: usize = result
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().path.len() + chunk.raw().language.len())
                .sum();
            let text: usize = result.chunks().iter().map(|chunk| chunk.text().len()).sum();
            let owned: usize = result
                .chunks()
                .iter()
                .map(|chunk| {
                    chunk.raw().path.capacity()
                        + chunk.raw().language.capacity()
                        + chunk.raw().bytes.capacity()
                        + chunk.owned_presentation_bytes()
                })
                .sum();
            assert_eq!(result.total_source_bytes(), physical);
            assert_eq!(result.total_metadata_bytes(), metadata);
            assert_eq!(result.total_text_bytes(), text);
            assert_eq!(result.total_response_memory_bytes(), owned);
            assert_eq!(text, 12);
            assert!(result.truncated());
            assert_eq!(result.chunks().len(), 2);
            for chunk in result.chunks() {
                assert_eq!(chunk.text(), "\u{feff}\u{2603}");
                assert_eq!(chunk.raw().reference, fixture.reference);
                assert_eq!(chunk.raw().content_hash, content_hash(&original));
            }
            let exact = SourceBudget::new()
                .with_max_source_bytes(physical)
                .with_max_metadata_bytes(metadata)
                .with_max_response_memory_bytes(owned)
                .with_max_snapshot_bytes(length);
            assert_eq!(
                read_prefix(&fixture, &references, 7, exact, &Cancellation::new()).unwrap(),
                result
            );
            for (budget, expected) in [
                (
                    exact.with_max_source_bytes(physical - 1),
                    SourceError::SourceBudgetExceeded,
                ),
                (
                    exact.with_max_metadata_bytes(metadata - 1),
                    SourceError::MetadataBudgetExceeded,
                ),
                (
                    exact.with_max_response_memory_bytes(owned - 1),
                    SourceError::ResponseMemoryBudgetExceeded,
                ),
                (
                    exact.with_max_snapshot_bytes(length - 1),
                    SourceError::SnapshotBudgetExceeded,
                ),
            ] {
                assert_eq!(
                    read_prefix(&fixture, &references, 7, budget, &Cancellation::new()),
                    Err(expected)
                );
            }
        }
    }

    #[test]
    fn lossless_prefix_preserves_reference_preflight_before_vfs() {
        let fixture = fixture(b"source tail", "utf-8", (0, 11));
        let reference = &fixture.reference;
        let foreign_repository = SourceRef::new(
            derive_repository(b"foreign-source-test").id(),
            reference.generation(),
            reference.span(),
            reference.content_hash(),
            None,
        );
        let foreign_generation = SourceRef::new(
            reference.repository(),
            rootlight_ids::GenerationId::from_bytes([0x55; 20]),
            reference.span(),
            reference.content_hash(),
            None,
        );
        let stale_hash = SourceRef::new(
            reference.repository(),
            reference.generation(),
            reference.span(),
            content_hash(b"different full source"),
            None,
        );
        let stale_range = selected_reference(&fixture, 0, 12);
        fs::remove_file(fixture._temporary.path().join("src/sample.rs")).unwrap();
        for (invalid, expected) in [
            (foreign_repository, SourceError::RepositoryMismatch),
            (foreign_generation, SourceError::GenerationMismatch),
            (stale_hash, SourceError::StaleSource),
            (stale_range, SourceError::StaleSource),
        ] {
            let references = [fixture.reference.clone(), invalid];
            assert_eq!(
                read_prefix(
                    &fixture,
                    &references,
                    1,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(expected)
            );
        }
    }

    #[test]
    fn lossless_prefix_retained_identity_survives_live_changes_outside_prefix() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf8,
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let encode = |body: &str| {
                if encoding == rootlight_vfs::TextEncoding::Utf8 {
                    format!("\u{feff}{body}").into_bytes()
                } else {
                    physical_utf16(body, encoding)
                }
            };
            let original = encode("alpha\u{1f600}tail");
            let changed = encode("alpha\u{1f600}fail");
            let length = u64::try_from(original.len()).unwrap();
            let fixture = fixture(&original, encoding.as_str(), (0, length));
            let path = RelativePath::parse(Path::new("src/sample.rs")).unwrap();
            let retained = [Arc::new(fixture.root.snapshot(&path, length).unwrap())];
            fs::write(fixture._temporary.path().join("src/sample.rs"), changed).unwrap();
            let service = SourceService::from_snapshots(&retained, &fixture.generation).unwrap();
            let prefix = service
                .read_lossless_text_prefix(
                    std::slice::from_ref(&fixture.reference),
                    4,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
            let chunk = &prefix.chunks()[0];
            assert_eq!(chunk.text(), "\u{feff}a");
            assert_eq!(chunk.raw().reference, fixture.reference);
            assert_eq!(chunk.raw().content_hash, content_hash(&original));
            assert_eq!(chunk.raw().bytes, original[..4]);
            assert_eq!(prefix.total_source_bytes(), 4);
            assert_eq!(prefix.total_text_bytes(), 4);
            assert!(prefix.truncated());
            assert_eq!(
                read_prefix(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    4,
                    SourceBudget::new(),
                    &Cancellation::new()
                ),
                Err(SourceError::StaleSource)
            );
        }
    }

    #[test]
    fn lossless_prefix_retains_whole_file_snapshot_caps_and_cancellation() {
        let mut fixture = fixture(b"first\n", "utf-8", (0, 6));
        let other = add_fixture_file(&mut fixture, "src/other.rs", b"second\n", (0, 7));
        let references = [fixture.reference.clone(), other];
        fs::remove_file(fixture._temporary.path().join("src/sample.rs")).unwrap();
        assert_eq!(
            read_prefix(
                &fixture,
                &references,
                1,
                SourceBudget::new()
                    .with_max_source_bytes(2)
                    .with_max_snapshot_bytes(12),
                &Cancellation::new()
            ),
            Err(SourceError::SnapshotBudgetExceeded)
        );
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            read_prefix(&fixture, &references, 1, SourceBudget::new(), &cancellation),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
    }

    #[test]
    fn lossless_prefix_boundary_calculation_is_allocation_free_and_cooperative() {
        let original = physical_utf16("\u{2603}\u{1f600}", rootlight_vfs::TextEncoding::Utf16Le);
        let fixture = fixture(&original, "utf-16le", (2, 8));
        let cancellation = Cancellation::new();
        let operations = Cell::new(0usize);
        let hook = || operations.set(operations.get() + 1);
        let control =
            SourceControl::new(&cancellation, Duration::from_secs(1)).with_after_operation(&hook);
        let prefix = lossless_text::bounded_text_prefix(
            &original,
            &fixture.reference,
            3,
            rootlight_vfs::TextEncoding::Utf16Le,
            &control,
        )
        .unwrap();
        assert_eq!(
            (prefix.range.start, prefix.range.end, prefix.text_bytes),
            (2, 4, 3)
        );
        assert_eq!(operations.get(), 0);
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            lossless_text::bounded_text_prefix(
                &original,
                &fixture.reference,
                3,
                rootlight_vfs::TextEncoding::Utf16Le,
                &control
            )
            .err(),
            Some(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        let uncancelled = Cancellation::new();
        let expired = SourceControl {
            cancellation: &uncancelled,
            deadline: Instant::now(),
            after_operation: None,
        };
        assert_eq!(
            lossless_text::bounded_text_prefix(
                &original,
                &fixture.reference,
                3,
                rootlight_vfs::TextEncoding::Utf16Le,
                &expired
            )
            .err(),
            Some(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
        );
    }

    #[test]
    fn lossless_utf8_preserves_bom_and_legacy_nul_policy() {
        for original in ["\u{feff}alpha\r\n", "alpha\0beta\n"] {
            let fixture = fixture(
                original.as_bytes(),
                "utf-8",
                (
                    0,
                    u64::try_from(original.len()).expect("fixture length fits u64"),
                ),
            );
            let result = read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                no_text_context(),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .expect("lossless UTF-8 keeps legacy scalar acceptance");
            let chunk = &result.chunks()[0];
            assert_eq!(chunk.text(), original);
            assert_eq!(chunk.raw().bytes, original.as_bytes());
            assert_eq!(chunk.text().as_ptr(), chunk.raw().bytes.as_ptr());
            assert_eq!(chunk.owned_presentation_bytes(), 0);
        }
    }

    #[test]
    fn lossless_utf16_preserves_selected_bom_and_original_identity() {
        let body = "first\r\n\u{00e9}\u{1f600} last\n";
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = physical_utf16(body, encoding);
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (
                    0,
                    u64::try_from(original.len()).expect("fixture length fits u64"),
                ),
            );
            for (start, end, expected) in [
                (0, original.len(), format!("\u{feff}{body}")),
                (0, 2, "\u{feff}".to_owned()),
                (2, original.len(), body.to_owned()),
                (2, 2, String::new()),
            ] {
                let reference = selected_reference(&fixture, start, end);
                let result = read_lossless(
                    &fixture,
                    std::slice::from_ref(&reference),
                    no_text_context(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .expect("exact UTF-16 scalar selection is readable");
                let chunk = &result.chunks()[0];
                assert_eq!(chunk.text(), expected);
                assert_eq!(chunk.raw().bytes, original[start..end]);
                assert_eq!(chunk.raw().reference, reference);
                assert_eq!(chunk.raw().content_hash, content_hash(&original));
                assert_eq!(chunk.raw().trust, SourceTrust::UntrustedRepositoryData);
                assert_eq!(chunk.raw().encoding, SourceEncoding::Bytes);
                assert_eq!(chunk.physical_encoding(), encoding);
                assert_eq!(result.generation(), fixture.reference.generation());
                assert_eq!(result.total_source_bytes(), end - start);
                assert_eq!(result.total_text_bytes(), expected.len());
                assert!(chunk.owned_presentation_bytes() >= expected.len());
                let encoded: Vec<u8> = chunk
                    .text()
                    .encode_utf16()
                    .flat_map(|unit| match encoding {
                        rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                        rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                        rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
                    })
                    .collect();
                assert_eq!(encoded, chunk.raw().bytes);
            }
            assert_eq!(
                read(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    SourceReadOptions::new(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                ),
                Err(SourceError::EncodingUnsupported)
            );
        }
    }

    #[test]
    fn lossless_utf16_rejects_odd_and_surrogate_interior_spans() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = physical_utf16("A\u{00e9}\u{1f600}Z", encoding);
            let fixture = fixture(&original, encoding.as_str(), (0, 12));
            for offset in [0, 2, 4, 6, 10, 12] {
                let reference = selected_reference(&fixture, offset, offset);
                let result = read_lossless(
                    &fixture,
                    std::slice::from_ref(&reference),
                    no_text_context(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .expect("scalar boundary admits an empty selection");
                assert!(result.chunks()[0].text().is_empty());
                assert_eq!(
                    result.chunks()[0].raw().start_byte,
                    u64::try_from(offset).expect("fixture offset fits u64")
                );
                assert_eq!(
                    result.chunks()[0].raw().end_byte,
                    u64::try_from(offset).expect("fixture offset fits u64")
                );
            }
            for offset in [1, 3, 5, 7, 8, 9, 11] {
                for (start, end) in [(offset, offset), (0, offset), (offset, original.len())] {
                    let reference = selected_reference(&fixture, start, end);
                    assert_eq!(
                        read_lossless(
                            &fixture,
                            std::slice::from_ref(&reference),
                            no_text_context(),
                            SourceBudget::new(),
                            &Cancellation::new(),
                        ),
                        Err(SourceError::InvalidSourceSpan)
                    );
                }
            }
            let interior = selected_reference(&fixture, 7, 8);
            let bytes = read(
                &fixture,
                std::slice::from_ref(&interior),
                SourceReadOptions::new()
                    .with_encoding(SourceEncoding::Bytes)
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .expect("legacy bytes remain uninterpreted");
            assert_eq!(bytes.chunks[0].bytes, original[7..8]);
            assert_eq!(
                (bytes.chunks[0].start_line, bytes.chunks[0].end_line),
                (None, None)
            );
        }
    }

    #[test]
    fn lossless_utf16_context_counts_decoded_lf_not_matching_raw_bytes() {
        let body = "zero\u{010a}\u{0a00}\r\none\u{00e9}\n\u{1f600}TWO\nthree\r\nfour";
        let logical = format!("\u{feff}{body}");
        let start = logical
            .find("\u{1f600}TWO")
            .expect("neutral selection exists");
        let end = start + "\u{1f600}TWO".len();
        let utf8 = fixture(
            logical.as_bytes(),
            "utf-8",
            (
                u64::try_from(start).expect("fixture start fits u64"),
                u64::try_from(end).expect("fixture end fits u64"),
            ),
        );
        for (before, after) in [(0, 0), (1, 1), (2, 2), (0, 2), (2, 0)] {
            let options = LosslessTextReadOptions::new()
                .with_context_lines_before(before)
                .with_context_lines_after(after);
            let control = read_lossless(
                &utf8,
                std::slice::from_ref(&utf8.reference),
                options,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .expect("UTF-8 control context is readable");
            let expected = &control.chunks()[0];
            for encoding in [
                rootlight_vfs::TextEncoding::Utf16Le,
                rootlight_vfs::TextEncoding::Utf16Be,
            ] {
                let original = physical_utf16(body, encoding);
                let physical_start = logical[..start].encode_utf16().count() * 2;
                let physical_end = logical[..end].encode_utf16().count() * 2;
                let fixture = fixture(
                    &original,
                    encoding.as_str(),
                    (
                        u64::try_from(physical_start).expect("fixture start fits u64"),
                        u64::try_from(physical_end).expect("fixture end fits u64"),
                    ),
                );
                let result = read_lossless(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    options,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .expect("encoded context follows decoded line endings");
                let actual = &result.chunks()[0];
                assert_eq!(actual.text(), expected.text());
                assert_eq!(
                    (actual.raw().start_line, actual.raw().end_line),
                    (expected.raw().start_line, expected.raw().end_line)
                );
                let expanded_start = logical[..usize::try_from(expected.raw().start_byte)
                    .expect("fixture start fits usize")]
                    .encode_utf16()
                    .count()
                    * 2;
                let expanded_end = logical
                    [..usize::try_from(expected.raw().end_byte).expect("fixture end fits usize")]
                    .encode_utf16()
                    .count()
                    * 2;
                assert_eq!(
                    (actual.raw().start_byte, actual.raw().end_byte),
                    (
                        u64::try_from(expanded_start).expect("fixture start fits u64"),
                        u64::try_from(expanded_end).expect("fixture end fits u64")
                    )
                );
                assert_eq!(actual.raw().bytes, original[expanded_start..expanded_end]);
            }
        }
    }

    #[test]
    fn lossless_utf16_rejects_malformed_unselected_tails_and_encoding_mismatch() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let valid = physical_utf16("stable", encoding);
            let encode_unit = |unit: u16| match encoding {
                rootlight_vfs::TextEncoding::Utf16Le => unit.to_le_bytes(),
                rootlight_vfs::TextEncoding::Utf16Be => unit.to_be_bytes(),
                rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
            };
            let mut malformed = Vec::new();
            let mut odd = valid.clone();
            odd.push(1);
            malformed.push(odd);
            for units in [vec![0xd800], vec![0xdc00], vec![0xd800, 0x0058], vec![0]] {
                let mut bytes = valid.clone();
                for unit in units {
                    bytes.extend_from_slice(&encode_unit(unit));
                }
                malformed.push(bytes);
            }
            let opposite = match encoding {
                rootlight_vfs::TextEncoding::Utf16Le => rootlight_vfs::TextEncoding::Utf16Be,
                rootlight_vfs::TextEncoding::Utf16Be => rootlight_vfs::TextEncoding::Utf16Le,
                rootlight_vfs::TextEncoding::Utf8 => unreachable!(),
            };
            malformed.push(physical_utf16("stable", opposite));
            malformed.push(valid[2..].to_vec());
            for original in malformed {
                let fixture = fixture(&original, encoding.as_str(), (0, 2));
                assert_eq!(
                    read_lossless(
                        &fixture,
                        std::slice::from_ref(&fixture.reference),
                        no_text_context(),
                        SourceBudget::new(),
                        &Cancellation::new(),
                    ),
                    Err(SourceError::EncodingUnsupported)
                );
                let bytes = read(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    SourceReadOptions::new()
                        .with_encoding(SourceEncoding::Bytes)
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .expect("legacy bytes do not repair or reinterpret malformed source");
                assert_eq!(bytes.chunks[0].bytes, original[..2]);
            }
        }
        let fixture = fixture(b"ordinary ASCII", "windows-1252", (0, 2));
        assert_eq!(
            read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                no_text_context(),
                SourceBudget::new(),
                &Cancellation::new(),
            ),
            Err(SourceError::EncodingUnsupported)
        );
    }

    #[test]
    fn lossless_duplicate_chunks_conserve_all_buffer_capacities() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = physical_utf16("\u{00e9}\u{1f600}\r\n", encoding);
            let original_len = u64::try_from(original.len()).expect("fixture length fits u64");
            let fixture = fixture(&original, encoding.as_str(), (0, original_len));
            let references = [fixture.reference.clone(), fixture.reference.clone()];
            let baseline = read_lossless(
                &fixture,
                &references,
                no_text_context(),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .expect("duplicate chunks are independently owned");
            let physical_bytes: usize = baseline
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().bytes.len())
                .sum();
            let metadata_bytes: usize = baseline
                .chunks()
                .iter()
                .map(|chunk| chunk.raw().path.len() + chunk.raw().language.len())
                .sum();
            let text_bytes: usize = baseline
                .chunks()
                .iter()
                .map(|chunk| chunk.text().len())
                .sum();
            let owned_bytes: usize = baseline
                .chunks()
                .iter()
                .map(|chunk| {
                    chunk.raw().path.capacity()
                        + chunk.raw().language.capacity()
                        + chunk.raw().bytes.capacity()
                        + chunk.owned_presentation_bytes()
                })
                .sum();
            assert_eq!(baseline.total_source_bytes(), physical_bytes);
            assert_eq!(baseline.total_metadata_bytes(), metadata_bytes);
            assert_eq!(baseline.total_text_bytes(), text_bytes);
            assert_eq!(baseline.total_response_memory_bytes(), owned_bytes);
            assert_eq!(physical_bytes, original.len() * references.len());
            assert_eq!(baseline.chunks().len(), references.len());
            assert_ne!(
                baseline.chunks()[0].raw().bytes.as_ptr(),
                baseline.chunks()[1].raw().bytes.as_ptr()
            );
            assert_ne!(
                baseline.chunks()[0].text().as_ptr(),
                baseline.chunks()[1].text().as_ptr()
            );
            let exact = SourceBudget::new()
                .with_max_source_bytes(physical_bytes)
                .with_max_metadata_bytes(metadata_bytes)
                .with_max_response_memory_bytes(owned_bytes)
                .with_max_snapshot_bytes(original_len);
            assert_eq!(
                read_lossless(
                    &fixture,
                    &references,
                    no_text_context(),
                    exact,
                    &Cancellation::new(),
                )
                .expect("exact physical, snapshot and response budgets are admitted"),
                baseline
            );
            for (budget, expected) in [
                (
                    exact.with_max_source_bytes(physical_bytes - 1),
                    SourceError::SourceBudgetExceeded,
                ),
                (
                    exact.with_max_metadata_bytes(metadata_bytes - 1),
                    SourceError::MetadataBudgetExceeded,
                ),
                (
                    exact.with_max_response_memory_bytes(owned_bytes - 1),
                    SourceError::ResponseMemoryBudgetExceeded,
                ),
                (
                    exact.with_max_snapshot_bytes(original_len - 1),
                    SourceError::SnapshotBudgetExceeded,
                ),
            ] {
                assert_eq!(
                    read_lossless(
                        &fixture,
                        &references,
                        no_text_context(),
                        budget,
                        &Cancellation::new(),
                    ),
                    Err(expected)
                );
            }
        }
    }

    #[test]
    fn lossless_retained_utf16_pins_bytes_while_live_changes_fail_closed() {
        for encoding in [
            rootlight_vfs::TextEncoding::Utf16Le,
            rootlight_vfs::TextEncoding::Utf16Be,
        ] {
            let original = physical_utf16("first\u{1f600}\r\n", encoding);
            let changed = physical_utf16("other\u{1f600}\r\n", encoding);
            let end = u64::try_from(original.len()).expect("fixture length fits u64");
            let fixture = fixture(&original, encoding.as_str(), (0, end));
            let path = RelativePath::parse(Path::new("src/sample.rs"))
                .expect("fixture source path is canonical");
            let retained = vec![Arc::new(
                fixture
                    .root
                    .snapshot(&path, end)
                    .expect("pinned snapshot is stable"),
            )];
            fs::write(fixture._temporary.path().join("src/sample.rs"), changed)
                .expect("live source changes");
            let service = SourceService::from_snapshots(&retained, &fixture.generation)
                .expect("retained snapshots are ordered");
            let result = service
                .read_lossless_text(
                    std::slice::from_ref(&fixture.reference),
                    no_text_context(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .expect("retained physical bytes are readable without live resnapshot");
            assert_eq!(result.chunks()[0].raw().bytes, original);
            assert_eq!(result.chunks()[0].text(), "\u{feff}first\u{1f600}\r\n");
            assert_eq!(
                result.chunks()[0].raw().content_hash,
                fixture.reference.content_hash()
            );
            assert_eq!(
                read_lossless(
                    &fixture,
                    std::slice::from_ref(&fixture.reference),
                    no_text_context(),
                    SourceBudget::new(),
                    &Cancellation::new(),
                ),
                Err(SourceError::StaleSource)
            );
            let missing = Vec::<Arc<SourceSnapshot>>::new();
            assert_eq!(
                SourceService::from_snapshots(&missing, &fixture.generation)
                    .expect("empty retained set is ordered")
                    .read_lossless_text(
                        std::slice::from_ref(&fixture.reference),
                        no_text_context(),
                        SourceBudget::new(),
                        &Cancellation::new(),
                    ),
                Err(SourceError::FileNotFound)
            );
        }
    }

    #[test]
    fn lossless_metadata_preflight_precedes_every_vfs_operation() {
        let mut fixture = fixture(b"ordinary source", "utf-8", (0, 2));
        let other = add_fixture_file(&mut fixture, "src/other.rs", b"other source", (0, 2));
        let metadata = fixture.generation.metadata();
        let mut document = fixture.generation.clone().into_document();
        document
            .files
            .iter_mut()
            .find(|file| file.id == other.span().file())
            .expect("additional file exists")
            .encoding = "unknown".to_owned();
        fixture.generation = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("unknown encoding metadata remains a valid indexed record");
        fs::remove_file(fixture._temporary.path().join("src/sample.rs"))
            .expect("first physical source is removed");
        assert_eq!(
            read_lossless(
                &fixture,
                &[fixture.reference.clone(), other],
                no_text_context(),
                SourceBudget::new(),
                &Cancellation::new(),
            ),
            Err(SourceError::EncodingUnsupported)
        );
    }

    #[test]
    fn lossless_context_limits_cancellation_and_redacted_debug_remain_exact() {
        let original = physical_utf16(
            "neutral-source-marker",
            rootlight_vfs::TextEncoding::Utf16Le,
        );
        let fixture = fixture(&original, "utf-16le", (0, 2));
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                no_text_context(),
                SourceBudget::new(),
                &cancellation,
            ),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
        assert_eq!(
            read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                no_text_context(),
                SourceBudget::new().with_max_duration(Duration::from_nanos(1)),
                &Cancellation::new(),
            ),
            Err(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
        );
        assert_eq!(
            read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                LosslessTextReadOptions::new()
                    .with_context_lines_before(HARD_MAX_CONTEXT_LINES + 1),
                SourceBudget::new(),
                &Cancellation::new(),
            ),
            Err(SourceError::ContextLimit)
        );
        let full = selected_reference(&fixture, 0, original.len());
        let result = read_lossless(
            &fixture,
            std::slice::from_ref(&full),
            no_text_context(),
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .expect("neutral source marker is readable");
        let debug = format!("{result:?} {:?}", result.chunks()[0]);
        assert!(!debug.contains("neutral-source-marker"));
        assert!(!debug.contains("src/sample.rs"));
        assert_eq!(
            LosslessTextReadOptions::default(),
            LosslessTextReadOptions::new()
        );
    }

    #[test]
    fn lossless_empty_utf8_and_bom_only_utf16_are_not_rewritten() {
        for (original, encoding, expected) in [
            (Vec::new(), rootlight_vfs::TextEncoding::Utf8, ""),
            (
                vec![0xff, 0xfe],
                rootlight_vfs::TextEncoding::Utf16Le,
                "\u{feff}",
            ),
            (
                vec![0xfe, 0xff],
                rootlight_vfs::TextEncoding::Utf16Be,
                "\u{feff}",
            ),
        ] {
            let fixture = fixture(
                &original,
                encoding.as_str(),
                (
                    0,
                    u64::try_from(original.len()).expect("fixture length fits u64"),
                ),
            );
            let result = read_lossless(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                no_text_context(),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .expect("strict empty or BOM-only source is readable");
            assert_eq!(result.chunks()[0].text(), expected);
            assert_eq!(result.chunks()[0].raw().bytes, original);
            assert_eq!(result.chunks()[0].physical_encoding(), encoding);
        }
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
                encoding: SourceEncoding::Utf8,
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
        assert_eq!((chunk.start_line, chunk.end_line), (Some(2), Some(4)));
        assert_eq!(chunk.bytes, b"one\nTWO\nthree\n");
        assert_eq!(chunk.encoding, SourceEncoding::Utf8);
        assert_eq!(chunk.trust, SourceTrust::UntrustedRepositoryData);
    }

    #[test]
    fn retained_snapshots_preserve_pinned_bytes_while_live_reads_fail_closed() {
        let original = b"pub fn answer() -> u32 {\n    42\n}\n";
        let changed = b"pub fn answer() -> u32 {\n    43\n}\n";
        let end = u64::try_from(original.len()).expect("fixture length fits u64");
        let fixture = fixture(original, "utf-8", (0, end));
        let path =
            RelativePath::parse(Path::new("src/sample.rs")).expect("fixture path is canonical");
        let retained = vec![Arc::new(
            fixture
                .root
                .snapshot(&path, end)
                .expect("pinned source snapshot is stable"),
        )];
        fs::write(fixture._temporary.path().join("src/sample.rs"), changed)
            .expect("live source changes");
        let service = SourceService::from_snapshots(&retained, &fixture.generation)
            .expect("retained snapshots are ordered");
        let result = service
            .read(
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new(),
            )
            .expect("retained source remains readable");
        assert_eq!(result.chunks[0].bytes, original);
        assert_eq!(
            result.chunks[0].content_hash,
            fixture.reference.content_hash()
        );

        assert_eq!(
            read(
                &fixture,
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new(),
            ),
            Err(SourceError::StaleSource)
        );

        let missing = Vec::<Arc<SourceSnapshot>>::new();
        let missing_service = SourceService::from_snapshots(&missing, &fixture.generation)
            .expect("an empty retained set is ordered");
        assert_eq!(
            missing_service.read(
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &Cancellation::new(),
            ),
            Err(SourceError::FileNotFound)
        );

        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ParentCancelled));
        assert_eq!(
            service.read(
                std::slice::from_ref(&fixture.reference),
                SourceReadOptions::default(),
                SourceBudget::default(),
                &cancellation,
            ),
            Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
        );
    }

    // Apple filesystem APIs reject ill-formed UTF-8 names before Rootlight can open them.
    #[cfg(all(unix, not(target_vendor = "apple")))]
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
                encoding: SourceEncoding::Utf8,
            },
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("split UTF-8 sequence is validated");

        assert_eq!(result.chunks[0].bytes, "🦀".as_bytes());
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
    fn byte_reads_preserve_ranges_inside_utf8_scalars() {
        let content = "a🦀b".as_bytes();
        let fixture = fixture(content, "utf-8", (2, 4));
        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("raw byte ranges do not require UTF-8 scalar boundaries");

        let chunk = &result.chunks[0];
        assert_eq!(chunk.bytes, &content[2..4]);
        assert_eq!((chunk.start_byte, chunk.end_byte), (2, 4));
        assert_eq!((chunk.start_line, chunk.end_line), (None, None));
        assert_eq!(chunk.encoding, SourceEncoding::Bytes);
    }

    #[test]
    fn zero_width_spans_remain_exact_without_context() {
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
        assert_eq!(chunk.bytes, b"");
        assert_eq!((chunk.start_byte, chunk.end_byte), (2, 2));
        assert_eq!((chunk.start_line, chunk.end_line), (Some(2), Some(2)));
    }

    #[test]
    fn zero_context_preserves_an_exact_partial_utf8_span() {
        let fixture = fixture(b"alpha\nbeta\n", "utf-8", (7, 9));
        let result = read(
            &fixture,
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::default(),
            &Cancellation::new(),
        )
        .expect("exact partial UTF-8 selection resolves");

        let chunk = &result.chunks[0];
        assert_eq!(chunk.bytes, b"et");
        assert_eq!((chunk.start_byte, chunk.end_byte), (7, 9));
        assert_eq!((chunk.start_line, chunk.end_line), (Some(2), Some(2)));
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

        let response_budget = SourceBudget::new().with_max_source_bytes(2);
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
                    .with_max_response_memory_bytes(all_metadata + 5),
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
                .with_max_response_memory_bytes(all_metadata + 6),
            &Cancellation::new(),
        )
        .expect("exact duplicate response memory is admitted");
        assert_eq!(result.total_metadata_bytes, all_metadata);
        assert_eq!(result.total_source_bytes, 6);
        assert_eq!(result.total_response_memory_bytes, all_metadata + 6);
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
            (Some(1), Some(101))
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
