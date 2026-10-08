//! Indexed source metadata validation without a source backing capability.
//! Returned borrows certify resident identity and path metadata only;
//! freshness, encoding bytes and scalar endpoints remain read-time checks.

use std::mem;

use rootlight_vfs::TextEncoding;

use super::*;

/// Borrowed indexed metadata for one unchanged editing selection.
#[derive(Clone, Copy)]
pub struct SourceMetadata<'a> {
    reference: &'a SourceRef,
    file: &'a FileRecord,
    extracted_from: Option<&'a SourceRef>,
    path_memory_upper: usize,
}

/// Validates ordered unique selectors using generation-owned metadata only.
///
/// No VFS, retained snapshot or durable reader is accepted. Complete bodies
/// and scalar endpoints must still be validated at read time.
///
/// # Errors
/// Returns [`SourceError`] for foreign, stale, duplicate or invalid selectors,
/// invalid paths, unsupported codecs, bounded metadata or cancellation.
pub fn validate_source_metadata<'a>(
    generation: &'a GenerationSnapshot,
    references: &'a [SourceRef],
    options: LosslessTextReadOptions,
    budget: SourceBudget,
    cancellation: &Cancellation,
) -> Result<Vec<SourceMetadata<'a>>, SourceError> {
    budget.validate()?;
    if references.is_empty() || references.len() > budget.max_selectors {
        return Err(SourceError::SelectorLimit);
    }
    if options.context_lines_before > budget.max_context_lines
        || options.context_lines_after > budget.max_context_lines
    {
        return Err(SourceError::ContextLimit);
    }
    let control = SourceControl::new(cancellation, budget.max_duration);
    let mut entries = Vec::new();
    try_reserve_vec(&mut entries, references.len(), &control)?;
    let mut metadata_bytes = 0usize;
    for (index, reference) in references.iter().enumerate() {
        control.check()?;
        if references[..index].contains(reference) {
            return Err(SourceError::SelectorLimit);
        }
        validate_reference(
            reference,
            generation.metadata().repository(),
            generation.metadata().generation(),
        )?;
        let file = generation
            .find_file(reference.span().file())
            .ok_or(SourceError::FileNotFound)?;
        validate_file(file, reference)?;
        lossless_text::physical_encoding(file)?;
        metadata_bytes = metadata_bytes
            .checked_add(chunk_metadata_bytes(file)?)
            .ok_or(SourceError::MetadataBudgetExceeded)?;
        if metadata_bytes > budget.max_metadata_bytes {
            return Err(SourceError::MetadataBudgetExceeded);
        }
        if metadata_bytes > budget.max_response_memory_bytes {
            return Err(SourceError::ResponseMemoryBudgetExceeded);
        }
        let path_memory_upper = source_metadata_memory_upper(file)?;
        let path = control.controlled(|| validated_path(file))?;
        drop(path);
        entries.push(SourceMetadata {
            reference,
            file,
            extracted_from: None,
            path_memory_upper,
        });
    }
    for mapping in &generation.document().source_mappings {
        control.check()?;
        if mapping.kind != SourceMappingKind::ExtractedFromContainer {
            continue;
        }
        for entry in &mut entries {
            if entry.file.id == mapping.from.span().file() {
                validate_file(entry.file, &mapping.from)?;
                if entry.extracted_from.is_some() {
                    return Err(SourceError::StaleSource);
                }
                entry.extracted_from = Some(&mapping.to);
            }
        }
    }
    control.check()?;
    Ok(entries)
}

impl std::fmt::Debug for SourceMetadata<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceMetadata")
            .field("reference", self.reference)
            .field("path_bytes", &self.file.path.len())
            .finish_non_exhaustive()
    }
}

/// Bounds metadata-only path reconstruction before its temporary allocation.
///
/// # Errors
/// Returns [`SourceError`] for a missing locator or arithmetic overflow.
pub fn source_metadata_memory_upper(file: &FileRecord) -> Result<usize, SourceError> {
    let locator = file
        .path_locator
        .as_ref()
        .ok_or(SourceError::InvalidIndexedPath)?;
    // Reconstruction owns decoded components, display/identity buffers and
    // vector slots. Actual reads still admit allocator capacities separately.
    let locator_bytes = locator.components().iter().try_fold(0usize, |sum, part| {
        sum.checked_add(part.len())
            .ok_or(SourceError::ResponseMemoryBudgetExceeded)
    })?;
    locator_bytes
        .checked_add(file.path.len())
        .and_then(|bytes| bytes.checked_mul(16))
        .and_then(|bytes| {
            bytes.checked_add(locator.components().len().checked_mul(
                16 * (mem::size_of::<std::ffi::OsString>()
                    + mem::size_of::<String>()
                    + mem::size_of::<usize>()),
            )?)
        })
        .ok_or(SourceError::ResponseMemoryBudgetExceeded)
}

impl<'a> SourceMetadata<'a> {
    /// Returns the complete approved selection, never a prefix surrogate.
    #[must_use]
    pub const fn reference(&self) -> &'a SourceRef {
        self.reference
    }

    /// Returns the resident indexed file record, without checking a source body.
    #[must_use]
    pub const fn file(&self) -> &'a FileRecord {
        self.file
    }

    /// Returns the indexed physical extraction origin, if present.
    #[must_use]
    pub const fn extracted_from(&self) -> Option<&'a SourceRef> {
        self.extracted_from
    }

    /// Returns a conservative path-reconstruction working-memory allowance.
    #[must_use]
    pub const fn path_memory_upper(&self) -> usize {
        self.path_memory_upper
    }

    /// Returns the indexed codec label, not current encoded-byte validity.
    ///
    /// # Errors
    /// Returns [`SourceError::EncodingUnsupported`] for an unsupported label.
    pub fn physical_encoding(&self) -> Result<TextEncoding, SourceError> {
        lossless_text::physical_encoding(self.file)
    }
}
