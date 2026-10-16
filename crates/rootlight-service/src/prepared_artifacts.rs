//! Transfers bounded parser work between preparation phases without enlarging the committed cache.

use super::{
    Arc, BTreeMap, Cancellation, FileId, FirstSliceError, FirstSliceGenerationCache,
    GenerationMemoryReservation, Mutex, NormalizedIrDocument, PendingGenerationMemory,
    TreeSitterStructuralArtifact, check_cancellation, generation_admission::GenerationAdmission,
};

pub(super) struct PreparedArtifacts {
    cache: Arc<Mutex<FirstSliceGenerationCache>>,
    entries: BTreeMap<FileId, PreparedArtifact>,
}

pub(super) struct PreparedArtifact {
    // Drop the payload before releasing the allowance that kept it alive.
    artifact: Arc<TreeSitterStructuralArtifact>,
    _reservation: ArtifactReservation,
}

impl PreparedArtifact {
    pub(super) fn artifact(&self) -> &Arc<TreeSitterStructuralArtifact> {
        &self.artifact
    }
}

impl PreparedArtifacts {
    pub(super) fn new(cache: Arc<Mutex<FirstSliceGenerationCache>>) -> Self {
        Self {
            cache,
            entries: BTreeMap::new(),
        }
    }

    pub(super) fn retain(
        &mut self,
        artifact: Arc<TreeSitterStructuralArtifact>,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        check_cancellation(cancellation)?;
        if !artifact.syntax_partition_complete() {
            return Ok(());
        }
        let file = artifact.file();
        if self.entries.contains_key(&file) {
            return Err(FirstSliceError::Retention);
        }
        let bytes = artifact
            .accounted_bytes()
            .checked_add(std::mem::size_of::<(FileId, PreparedArtifact)>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        let reservation = match ArtifactReservation::new(Arc::clone(&self.cache), bytes) {
            Ok(reservation) => reservation,
            // This optional transfer cannot make a viable uncached preparation fail.
            Err(FirstSliceError::GenerationMemoryLimit { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        check_cancellation(cancellation)?;
        self.entries.insert(
            file,
            PreparedArtifact {
                artifact,
                _reservation: reservation,
            },
        );
        Ok(())
    }

    pub(super) fn take(&mut self, file: FileId) -> Option<PreparedArtifact> {
        self.entries.remove(&file)
    }

    pub(super) fn admit_document(
        &mut self,
        admission: &mut GenerationAdmission,
        document: &NormalizedIrDocument,
        reservation: &mut GenerationMemoryReservation,
        durable_cache: bool,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        let allow_unload = durable_cache && self.entries.is_empty();
        let result = admission.admit_document(document, reservation, allow_unload, cancellation);
        if matches!(result, Err(FirstSliceError::GenerationMemoryLimit { .. }))
            && !self.entries.is_empty()
        {
            // Future files can repeat parsing before resident generations need unloading.
            self.entries.clear();
            admission.admit_document(document, reservation, durable_cache, cancellation)
        } else {
            result
        }
    }
}

struct ArtifactReservation {
    cache: Arc<Mutex<FirstSliceGenerationCache>>,
    bytes: u64,
}

impl ArtifactReservation {
    fn new(
        cache: Arc<Mutex<FirstSliceGenerationCache>>,
        bytes: u64,
    ) -> Result<Self, FirstSliceError> {
        {
            let mut ledger = cache.lock().map_err(|_| FirstSliceError::Retention)?;
            // Optional preparation work does not evict already committed generations.
            ledger.ensure_capacity(bytes, PendingGenerationMemory::Reserved, false)?;
            ledger.prepared_artifact_reservation_bytes = ledger
                .prepared_artifact_reservation_bytes
                .checked_add(bytes)
                .ok_or(FirstSliceError::Limits)?;
        }
        Ok(Self { cache, bytes })
    }
}

impl Drop for ArtifactReservation {
    fn drop(&mut self) {
        if let Ok(mut ledger) = self.cache.lock()
            && let Some(remaining) = ledger
                .prepared_artifact_reservation_bytes
                .checked_sub(self.bytes)
        {
            ledger.prepared_artifact_reservation_bytes = remaining;
        }
    }
}
