//! Immutable UTF-8 proof and sparse line positions for explicit read operations.
//! The snapshot owner keeps the validated bytes unchanged; ordinary live reads
//! never acquire or consult this view.

use super::*;

const CHECKPOINT_BYTES: usize = 4 * 1024;
const OWNER_OVERHEAD: usize = 256;

/// One generation-bound immutable snapshot with complete UTF-8 validation.
///
/// Sparse newline counts bound repeated prefix lookups to one 4 KiB block.
pub struct PreparedUtf8Source {
    snapshot: Arc<SourceSnapshot>,
    repository: RepositoryId,
    generation: GenerationId,
    checkpoints: Vec<u64>,
}

impl PreparedUtf8Source {
    /// Bounds ownership and checkpoint construction before allocating the view.
    #[must_use]
    pub fn memory_upper(snapshot: &SourceSnapshot) -> Option<usize> {
        snapshot
            .retained_memory_bytes()?
            .checked_add(std::mem::size_of::<Self>())?
            .checked_add(OWNER_OVERHEAD)?
            .checked_add(
                snapshot
                    .content()
                    .len()
                    .checked_div(CHECKPOINT_BYTES)?
                    .checked_add(1)?
                    .checked_mul(std::mem::size_of::<u64>())?
                    .checked_mul(2)?,
            )
    }

    /// Validates the entire immutable body, including unselected tails.
    ///
    /// # Errors
    /// Rejects foreign snapshot identity, non-UTF-8 content, cancellation or
    /// unavailable bounded checkpoint memory.
    pub fn new(
        snapshot: Arc<SourceSnapshot>,
        generation: &GenerationSnapshot,
        cancellation: &Cancellation,
        deadline: Option<Instant>,
    ) -> Result<Self, SourceError> {
        let metadata = generation.metadata();
        let file = generation
            .find_file(snapshot.file())
            .ok_or(SourceError::FileNotFound)?;
        let control = SourceControl {
            cancellation,
            deadline,
            #[cfg(test)]
            after_operation: None,
            #[cfg(test)]
            origin_record_visits: std::cell::Cell::new(0),
        };
        control.check()?;
        if file.repository != metadata.repository()
            || file.generation != metadata.generation()
            || snapshot.content_hash() != file.content_hash
            || snapshot.metadata().length != file.byte_length
            || snapshot.path().as_str() != file.path
        {
            return Err(SourceError::StaleSource);
        }
        ensure_utf8(file, snapshot.content(), &control)?;
        let capacity = snapshot
            .content()
            .len()
            .checked_div(CHECKPOINT_BYTES)
            .and_then(|slots| slots.checked_add(1))
            .ok_or(SourceError::MemoryUnavailable)?;
        let mut checkpoints = Vec::new();
        checkpoints
            .try_reserve_exact(capacity)
            .map_err(|_| SourceError::MemoryUnavailable)?;
        let mut lines = 0u64;
        for chunk in snapshot.content().chunks(CHECKPOINT_BYTES) {
            control.check()?;
            checkpoints.push(lines);
            lines = lines
                .checked_add(usize_to_u64(count_newlines(chunk, &control)?)?)
                .ok_or(SourceError::InvalidSourceSpan)?;
        }
        if snapshot.content().len().is_multiple_of(CHECKPOINT_BYTES) {
            checkpoints.push(lines);
        }
        control.check()?;
        Ok(Self {
            snapshot,
            repository: metadata.repository(),
            generation: metadata.generation(),
            checkpoints,
        })
    }

    /// Borrows the immutable authenticated snapshot retained by this owner.
    #[must_use]
    pub fn snapshot(&self) -> &Arc<SourceSnapshot> {
        &self.snapshot
    }

    /// Charges owned capacities, including the snapshot and its shared buffers.
    #[must_use]
    pub fn retained_memory_bytes(&self) -> Option<usize> {
        self.snapshot
            .retained_memory_bytes()?
            .checked_add(std::mem::size_of::<Self>())?
            .checked_add(OWNER_OVERHEAD)?
            .checked_add(
                self.checkpoints
                    .capacity()
                    .checked_mul(std::mem::size_of::<u64>())?,
            )
    }

    pub(super) fn matches(&self, file: &FileRecord) -> bool {
        file.repository == self.repository
            && file.generation == self.generation
            && file.id == self.snapshot.file()
            && file.content_hash == self.snapshot.content_hash()
            && file.byte_length == self.snapshot.metadata().length
            && file.path == self.snapshot.path().as_str()
            && matches!(file.encoding.as_str(), "utf-8" | "utf8")
    }

    pub(super) fn line_before(
        &self,
        boundary: usize,
        control: &SourceControl<'_>,
    ) -> Result<u64, SourceError> {
        let slot = boundary / CHECKPOINT_BYTES;
        let base = self
            .checkpoints
            .get(slot)
            .ok_or(SourceError::InvalidSourceSpan)?;
        let start = slot
            .checked_mul(CHECKPOINT_BYTES)
            .ok_or(SourceError::InvalidSourceSpan)?;
        let tail = self
            .snapshot
            .content()
            .get(start..boundary)
            .ok_or(SourceError::InvalidSourceSpan)?;
        base.checked_add(usize_to_u64(count_newlines(tail, control)?)?)
            .and_then(|lines| lines.checked_add(1))
            .ok_or(SourceError::InvalidSourceSpan)
    }
}

impl std::fmt::Debug for PreparedUtf8Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedUtf8Source")
            .field("generation", &self.generation)
            .field("file", &self.snapshot.file())
            .finish_non_exhaustive()
    }
}
