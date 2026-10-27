//! Durable sidecar ownership reserved before ordinary file-fact admission.
//! Bounds use the real sidecar codecs and unchanged writer limits; final sealed
//! descriptors and independent reopen admission remain authoritative backstops.

use super::*;
use crate::{FirstSliceResource, current_cancellation};
use std::io::Write;

pub(crate) struct Source<'a> {
    pub(crate) claim: &'a FileIdentityClaim,
    pub(crate) language: &'a str,
    pub(crate) encoding: &'a str,
    pub(crate) generated: bool,
}

pub(crate) struct Envelope {
    repository: RepositoryId,
    generation: GenerationId,
    files: u64,
    authority: u64,
    packed_index: u64,
    payload_bytes: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReservedSidecarInputs {
    incremental_encoded_bytes: u64,
    authority_encoded_bytes: u64,
    packed_index_encoded_bytes: u64,
    memory_multiplier: u64,
    fixed_overhead_bytes: u64,
}

pub(super) struct Counter<'a> {
    pub(super) bytes: u64,
    cancellation: &'a Cancellation,
}

impl Counter<'_> {
    pub(super) fn new(cancellation: &Cancellation) -> Counter<'_> {
        Counter {
            bytes: 0,
            cancellation,
        }
    }
}

impl Write for Counter<'_> {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        self.cancellation
            .check()
            .map_err(|_| std::io::Error::other("sidecar census cancelled"))?;
        self.bytes = self
            .bytes
            .checked_add(
                u64::try_from(value.len())
                    .map_err(|_| std::io::Error::other("sidecar census overflow"))?,
            )
            .ok_or_else(|| std::io::Error::other("sidecar census overflow"))?;
        Ok(value.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Envelope {
    /// Reserves ordinary source-authority and packed-index representations.
    ///
    /// This scope excludes origins/native project materialization and any source
    /// catalog: all physical file metadata stays in the ordinary Oracle document.
    /// Incremental evidence can grow after lowering, so every state that can
    /// seal is charged at its existing 256 MiB encoded hard maximum. This is an
    /// explicit utilization tradeoff, not a claim about actual sidecar size.
    pub(crate) fn prepare(
        metadata: GenerationMetadata,
        sources: &[Source<'_>],
        cancellation: &Cancellation,
        admission: &mut dyn FnMut(u64) -> Result<(), FirstSliceError>,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        if sources.is_empty() || sources.len() > MAX_SOURCE_BLOB_ENTRIES {
            return Err(FirstSliceError::Identity);
        }
        let expected = sources
            .len()
            .checked_mul(std::mem::size_of::<DurablePackedSourceEntry>())
            .and_then(|bytes| {
                bytes.checked_add(sources.len().checked_mul(std::mem::size_of::<u64>())?)
            })
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        admission(expected)?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(sources.len()).map_err(|_| {
            FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            }
        })?;
        let mut pack_bytes = Vec::new();
        pack_bytes.try_reserve_exact(sources.len()).map_err(|_| {
            FirstSliceError::ResourceUnavailable {
                resource: FirstSliceResource::MemoryBytes,
            }
        })?;
        let actual = entries
            .capacity()
            .checked_mul(std::mem::size_of::<DurablePackedSourceEntry>())
            .and_then(|bytes| {
                bytes.checked_add(
                    pack_bytes
                        .capacity()
                        .checked_mul(std::mem::size_of::<u64>())?,
                )
            })
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(FirstSliceError::Limits)?;
        if actual > expected {
            admission(actual - expected)?;
        }
        let mut payload_bytes = 0_u64;
        for source in sources {
            check_cancellation(cancellation)?;
            if source.claim.repository != metadata.repository() {
                return Err(FirstSliceError::Identity);
            }
            payload_bytes = payload_bytes
                .checked_add(source.claim.byte_length)
                .ok_or(FirstSliceError::Limits)?;
            // A pack owns at least one complete file entry; a single source is
            // never split into multiple index entries. Thus at most N packs can
            // seal for N physical files. Maximum-width scalar values bound the
            // exact MessagePack encoding without depending on chosen packing.
            entries.push(DurablePackedSourceEntry {
                file: source.claim.file,
                digest: source.claim.content_hash,
                pack: u32::MAX,
                offset: u64::MAX,
                bytes: source.claim.byte_length,
            });
            pack_bytes.push(u64::MAX);
        }
        let index = DurablePackedSourceIndex {
            version: SOURCE_PACK_INDEX_VERSION,
            payload_bytes,
            pack_bytes,
            entries,
        };
        let mut packed = Counter::new(cancellation);
        if rmp_serde::encode::write_named(&mut packed, &index).is_err() {
            return Err(current_cancellation(cancellation).unwrap_or(FirstSliceError::Limits));
        }
        let authority =
            source_authority::prospective_ordinary_bytes(metadata, sources, cancellation)?;
        let envelope = Self {
            repository: metadata.repository(),
            generation: metadata.generation(),
            files: u64::try_from(sources.len()).map_err(|_| FirstSliceError::Limits)?,
            authority: authority.min(source_authority::maximum_bytes()),
            packed_index: packed.bytes.min(MAX_SOURCE_PACK_INDEX_BYTES),
            payload_bytes,
        };
        // The restore envelope is a logical future ownership charge in every
        // restore comparison. Only the actual codec workspace above belongs in
        // the current producer's heap reservation.
        Ok(envelope)
    }

    pub(crate) fn restore_bytes(&self) -> Result<u64, FirstSliceError> {
        recovery_sidecar_memory_bytes_from_parts(
            Some(DurableSidecarDescriptor {
                bytes: MAX_INCREMENTAL_STATE_BYTES,
                digest: ContentHash::from_bytes([0; 32]),
            }),
            None,
            Some(DurableSidecarDescriptor {
                bytes: self.authority,
                digest: ContentHash::from_bytes([0; 32]),
            }),
            Some(DurableSourceStorage {
                version: SOURCE_STORAGE_VERSION,
                files: Some(self.files),
                packs: Some(self.files),
                index_bytes: Some(self.packed_index),
                index_digest: None,
                payload_bytes: None,
            }),
            self.files,
        )
    }

    /// Checks that the actual ordinary descriptor owners satisfy the reserved envelope.
    pub(crate) fn verify_written(
        &self,
        prepared: &DurablePreparedGeneration,
    ) -> Result<(), FirstSliceError> {
        if prepared.repository_id != self.repository || prepared.generation != self.generation {
            return Err(FirstSliceError::Identity);
        }
        let incremental = *prepared
            .incremental_state
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        let authority = *prepared
            .source_authority
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        let catalog = *prepared
            .source_file_catalog
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        let storage = *prepared
            .source_storage
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if catalog.is_some()
            || incremental
                .is_none_or(|value| value.bytes == 0 || value.bytes > MAX_INCREMENTAL_STATE_BYTES)
            || authority.is_none_or(|value| value.bytes == 0 || value.bytes > self.authority)
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        match source_storage_layout(GENERATION_MANIFEST_VERSION, storage)? {
            DurableSourceLayout::Blobs => {}
            DurableSourceLayout::Packed(storage)
                if storage.files == Some(self.files)
                    && storage
                        .packs
                        .is_some_and(|packs| packs > 0 && packs <= self.files)
                    && storage
                        .index_bytes
                        .is_some_and(|bytes| bytes > 0 && bytes <= self.packed_index)
                    && storage.payload_bytes == Some(self.payload_bytes) => {}
            _ => return Err(FirstSliceError::CatalogCorrupt),
        }
        Ok(())
    }

    pub(crate) fn observed(
        &self,
        plan: rootlight_catalog::OracleReadMemoryPlan,
    ) -> Result<u64, FirstSliceError> {
        Ok(self.breakdown(plan)?.total_bytes)
    }

    pub(crate) fn breakdown(
        &self,
        plan: rootlight_catalog::OracleReadMemoryPlan,
    ) -> Result<OracleRestoreMemoryBreakdown, FirstSliceError> {
        oracle_restore_memory_breakdown(plan, false, self.restore_bytes()?)
    }

    pub(crate) fn reserved_sidecar_inputs(&self) -> ReservedSidecarInputs {
        ReservedSidecarInputs {
            incremental_encoded_bytes: MAX_INCREMENTAL_STATE_BYTES,
            authority_encoded_bytes: self.authority,
            packed_index_encoded_bytes: self.packed_index,
            memory_multiplier: RECOVERY_SIDECAR_MEMORY_MULTIPLIER,
            fixed_overhead_bytes: crate::GENERATION_MEMORY_FIXED_OVERHEAD_BYTES,
        }
    }
}

#[cfg(test)]
#[path = "restore_envelope/tests.rs"]
mod tests;
