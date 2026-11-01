//! Operation-owned packed index and one verified pack, outside global caches.
//! The service reserves every live representation before opening this owner and
//! rechecks current publication separately before and after each response.

use super::*;
use crate::source_read_operation::OperationWork;
use crate::{
    AnalysisProfileGuard, AnalysisProfilePhase, AnalysisProfileScopeExit, analysis_profile_enabled,
    current_cancellation, profile_analysis_phase, record_analysis_profile_elapsed,
};
use rootlight_vfs::platform::{PlatformFileIdentity, PrivateRangeReadPhase};
#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::io::Write as _;
use std::time::Duration;

pub(crate) struct ReadOperationDescriptor {
    manifest: ContentHash,
    storage: DurableSourceStorage,
    repository_identity: PlatformFileIdentity,
    generation_identity: PlatformFileIdentity,
    sources_identity: PlatformFileIdentity,
}

pub(crate) struct PackedReadOperation {
    reader: PersistedSourceReader,
    descriptor: ReadOperationDescriptor,
    #[cfg(test)]
    index_reads: Cell<u64>,
    #[cfg(test)]
    pack_reads: Cell<u64>,
    #[cfg(test)]
    range_reads: Cell<u64>,
}

impl DurableCatalog {
    pub(crate) fn read_operation_descriptor(
        &self,
        receipt: &FirstSliceIndexReceipt,
        work: &OperationWork,
        cancellation: &Cancellation,
    ) -> Result<Option<ReadOperationDescriptor>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let repository = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let mut published = false;
        let entries = repository
            .capability()
            .entries()
            .map_err(|_| FirstSliceError::Catalog)?;
        for (index, entry) in entries.enumerate() {
            work.charge(1, cancellation)?;
            if index >= MAX_DURABLE_ENTRIES {
                return Err(FirstSliceError::Retention);
            }
            let name = entry.map_err(|_| FirstSliceError::Catalog)?.file_name();
            let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            if let Some((sequence, generation)) = parse_activation_name(text)
                && generation == receipt.generation
            {
                // A matching directory name alone is not publication authority.
                work.charge(MAX_ACTIVATION_MANIFEST_BYTES, cancellation)?;
                matching_marker(&repository, &name, sequence, generation, cancellation)?;
                published = true;
            }
        }
        check_cancellation(cancellation)?;
        if !published {
            return Err(FirstSliceError::GenerationNotFound);
        }
        let generation = PrivateDirectory::open(
            repository.capability(),
            OsStr::new(&receipt.generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        work.charge(MAX_MANIFEST_BYTES, cancellation)?;
        let encoded = generation
            .read_file_bounded_cancellable(
                OsStr::new(MANIFEST_FILENAME),
                MAX_MANIFEST_BYTES,
                cancellation,
            )
            .map_err(map_private_read_error)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&encoded).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        check_cancellation(cancellation)?;
        // Logical identities are restored from a sidecar and omitted from JSON.
        let receipt_matches_manifest = {
            let mut persisted = receipt.clone();
            persisted.logical_snapshot = None;
            manifest.receipt == persisted
        };
        if !receipt_matches_manifest {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let DurableSourceLayout::Packed(storage) =
            source_authority::manifest_source_layout(&manifest)?
        else {
            return Ok(None);
        };
        let sources =
            PrivateDirectory::open(generation.capability(), OsStr::new(SOURCES_DIRECTORY))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        Ok(Some(ReadOperationDescriptor {
            manifest: content_hash_bytes(&encoded),
            storage,
            repository_identity: repository.identity(),
            generation_identity: generation.identity(),
            sources_identity: sources.identity(),
        }))
    }

    pub(crate) fn open_read_operation(
        &self,
        receipt: &FirstSliceIndexReceipt,
        descriptor: ReadOperationDescriptor,
        work: &OperationWork,
        cancellation: &Cancellation,
    ) -> Result<PackedReadOperation, FirstSliceError> {
        let repository = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let generation = PrivateDirectory::open(
            repository.capability(),
            OsStr::new(&receipt.generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let sources =
            PrivateDirectory::open(generation.capability(), OsStr::new(SOURCES_DIRECTORY))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if repository.identity() != descriptor.repository_identity
            || generation.identity() != descriptor.generation_identity
            || sources.identity() != descriptor.sources_identity
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let bytes = descriptor
            .storage
            .index_bytes
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        #[cfg(test)]
        let index_reads = Cell::new(0);
        #[cfg(test)]
        index_reads.set(index_reads.get() + 1);
        let encoded = collect_exact(
            &sources,
            OsStr::new(SOURCE_PACK_INDEX_FILENAME),
            bytes,
            work,
            cancellation,
            None,
        )?;
        work.charge(bytes, cancellation)?;
        if hash_cancellable(&encoded, cancellation)?
            != descriptor
                .storage
                .index_digest
                .ok_or(FirstSliceError::CatalogCorrupt)?
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        // The decoder reads through caller checkpoints; its owned decode/map
        // workspace was admitted with the descriptor's conservative quote.
        let mut input = CancellableInput {
            remaining: &encoded,
            cancellation,
        };
        let decoded: DurablePackedSourceIndex = rmp_serde::from_read(&mut input).map_err(|_| {
            current_cancellation(cancellation).unwrap_or(FirstSliceError::CatalogCorrupt)
        })?;
        work.charge(descriptor.storage.files.unwrap_or(0), cancellation)?;
        let index = validate_packed_source_index(decoded, descriptor.storage, cancellation)?;
        Ok(PackedReadOperation {
            reader: PersistedSourceReader {
                repository: receipt.repository,
                sources,
                layout: PersistedSourceLayout::Packed {
                    index,
                    cached_pack: Mutex::new(None),
                },
            },
            descriptor,
            #[cfg(test)]
            index_reads,
            #[cfg(test)]
            pack_reads: Cell::new(0),
            #[cfg(test)]
            range_reads: Cell::new(0),
        })
    }
}

impl ReadOperationDescriptor {
    pub(crate) fn memory_bytes(&self) -> Result<u64, FirstSliceError> {
        let encoded = self
            .storage
            .index_bytes
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let files = self.storage.files.ok_or(FirstSliceError::CatalogCorrupt)?;
        if encoded == 0
            || encoded > MAX_SOURCE_PACK_INDEX_BYTES
            || files
                > u64::try_from(MAX_SOURCE_BLOB_ENTRIES).map_err(|_| FirstSliceError::Limits)?
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        // Match recovery's 8x encoded/decoded staging envelope, adding explicit
        // map-node workspace and both old/new maximum-size pack owners.
        encoded
            .checked_mul(8)
            .and_then(|bytes| {
                files
                    .checked_mul(256)
                    .and_then(|nodes| bytes.checked_add(nodes))
            })
            .and_then(|bytes| bytes.checked_add(MAX_SOURCE_FILE_BYTES.checked_mul(2)?))
            .ok_or(FirstSliceError::Limits)
    }
}

impl PackedReadOperation {
    pub(crate) fn revalidate(
        &self,
        catalog: &DurableCatalog,
        receipt: &FirstSliceIndexReceipt,
        work: &OperationWork,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        let current = catalog
            .read_operation_descriptor(receipt, work, cancellation)?
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if current.manifest != self.descriptor.manifest
            || current.repository_identity != self.descriptor.repository_identity
            || current.generation_identity != self.descriptor.generation_identity
            || current.sources_identity != self.descriptor.sources_identity
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(())
    }

    pub(crate) fn read(
        &self,
        file: &FileRecord,
        work: &OperationWork,
        cancellation: &Cancellation,
    ) -> Result<SourceSnapshot, FirstSliceError> {
        check_cancellation(cancellation)?;
        if file.repository != self.reader.repository
            || file.byte_length > MAX_SOURCE_FILE_BYTES
            || file.byte_length > MAX_SNAPSHOT_BYTES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let PersistedSourceLayout::Packed { index, cached_pack } = &self.reader.layout else {
            return Err(FirstSliceError::CatalogCorrupt);
        };
        let entry = index
            .entries
            .get(&file.id)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if entry.digest != file.content_hash || entry.bytes != file.byte_length {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let pack_bytes = *index
            .pack_bytes
            .get(usize::try_from(entry.pack).map_err(|_| FirstSliceError::Limits)?)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let mut cached = cached_pack.lock().map_err(|_| FirstSliceError::Catalog)?;
        if cached.as_ref().map(|pack| pack.ordinal) != Some(entry.pack) {
            #[cfg(test)]
            self.pack_reads.set(self.pack_reads.get() + 1);
            let bytes = profile_analysis_phase!(
                SourceReadPackedLoad,
                Some(file.id),
                collect_exact(
                    &self.reader.sources,
                    OsStr::new(&source_pack_name(entry.pack)),
                    pack_bytes,
                    work,
                    cancellation,
                    Some(file.id),
                )
            )?;
            let profile = AnalysisProfileGuard::begin(
                AnalysisProfilePhase::SourceReadPackedVerification,
                Some(file.id),
            );
            work.charge(
                u64::try_from(index.entries.len()).map_err(|_| FirstSliceError::Limits)?,
                cancellation,
            )?;
            for candidate in index.entries.values() {
                check_cancellation(cancellation)?;
                if candidate.pack == entry.pack {
                    work.charge(candidate.bytes, cancellation)?;
                    let start =
                        usize::try_from(candidate.offset).map_err(|_| FirstSliceError::Limits)?;
                    let end = candidate
                        .offset
                        .checked_add(candidate.bytes)
                        .and_then(|end| usize::try_from(end).ok())
                        .ok_or(FirstSliceError::Limits)?;
                    if hash_cancellable(
                        bytes
                            .get(start..end)
                            .ok_or(FirstSliceError::CatalogCorrupt)?,
                        cancellation,
                    )? != candidate.digest
                    {
                        return Err(FirstSliceError::CatalogCorrupt);
                    }
                }
            }
            *cached = Some(CachedSourcePack {
                ordinal: entry.pack,
                bytes,
            });
            profile.finish();
        }
        let start = usize::try_from(entry.offset).map_err(|_| FirstSliceError::Limits)?;
        let end = entry
            .offset
            .checked_add(entry.bytes)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(FirstSliceError::Limits)?;
        let selected = cached
            .as_ref()
            .and_then(|pack| pack.bytes.get(start..end))
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(selected.len())
            .map_err(|_| FirstSliceError::Retention)?;
        bytes.extend_from_slice(selected);
        drop(cached);
        check_cancellation(cancellation)?;
        let path = RelativePath::from_locator(
            file.path_locator
                .as_ref()
                .ok_or(FirstSliceError::CatalogCorrupt)?,
        )
        .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))?;
        SourceSnapshot::from_persisted(file.repository, path, file.id, file.content_hash, bytes)
            .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))
    }

    pub(crate) fn visit(
        &self,
        file: &FileRecord,
        maximum: u64,
        work: &OperationWork,
        cancellation: &Cancellation,
        visitor: &mut crate::source_backing::VerifiedSourceVisitor<'_>,
    ) -> Result<u64, FirstSliceError> {
        if file.byte_length > maximum {
            return Err(FirstSliceError::BudgetExceeded);
        }
        let PersistedSourceLayout::Packed { index, .. } = &self.reader.layout else {
            return Err(FirstSliceError::CatalogCorrupt);
        };
        let entry = index
            .entries
            .get(&file.id)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if file.repository != self.reader.repository
            || entry.digest != file.content_hash
            || entry.bytes != file.byte_length
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let pack_bytes = *index
            .pack_bytes
            .get(usize::try_from(entry.pack).map_err(|_| FirstSliceError::Limits)?)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let end = entry
            .offset
            .checked_add(entry.bytes)
            .filter(|end| *end <= pack_bytes)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        work.charge(file.byte_length, cancellation)?;
        let mut hash = blake3::Hasher::new();
        let mut consumed = 0u64;
        #[cfg(test)]
        self.range_reads.set(self.range_reads.get() + 1);
        let actual = self
            .reader
            .sources
            .read_file_range_streamed_cancellable(
                OsStr::new(&source_pack_name(entry.pack)),
                pack_bytes,
                entry.offset..end,
                cancellation,
                |offset, bytes| {
                    if entry.offset.checked_add(consumed) != Some(offset) {
                        return Err(PlatformError::SecurityPolicy);
                    }
                    hash.update(bytes);
                    visitor(consumed, bytes)?;
                    consumed = consumed
                        .checked_add(
                            u64::try_from(bytes.len()).map_err(|_| PlatformError::ResourceLimit)?,
                        )
                        .ok_or(PlatformError::ResourceLimit)?;
                    Ok(())
                },
            )
            .map_err(map_private_read_error)?;
        check_cancellation(cancellation)?;
        if actual != file.byte_length
            || consumed != file.byte_length
            || ContentHash::from_bytes(*hash.finalize().as_bytes()) != file.content_hash
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(consumed)
    }

    #[cfg(test)]
    pub(crate) fn counters(&self) -> (u64, u64, u64) {
        (
            self.index_reads.get(),
            self.pack_reads.get(),
            self.range_reads.get(),
        )
    }

    #[cfg(test)]
    pub(crate) fn cached_pack_bytes(&self) -> Option<usize> {
        let PersistedSourceLayout::Packed { cached_pack, .. } = &self.reader.layout else {
            panic!("operation requires packed storage");
        };
        cached_pack
            .lock()
            .unwrap()
            .as_ref()
            .map(|pack| pack.bytes.len())
    }
}

fn collect_exact(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    expected: u64,
    work: &OperationWork,
    cancellation: &Cancellation,
    profile_file: Option<FileId>,
) -> Result<Vec<u8>, FirstSliceError> {
    work.charge(expected, cancellation)?;
    let count = usize::try_from(expected).map_err(|_| FirstSliceError::Limits)?;
    let mut bytes = Vec::new();
    let allocation = profile_file.map(|file| {
        AnalysisProfileGuard::begin(AnalysisProfilePhase::SourceReadPackedAllocation, Some(file))
    });
    let reserved = bytes
        .try_reserve_exact(count)
        .map_err(|_| FirstSliceError::Retention);
    if let Some(allocation) = allocation {
        allocation.finish();
    }
    reserved?;
    let mut observe = |phase, elapsed| {
        record_packed_read_phase(phase, profile_file, elapsed);
    };
    let observer = (profile_file.is_some() && analysis_profile_enabled())
        .then_some(&mut observe as &mut dyn FnMut(PrivateRangeReadPhase, Duration));
    let actual = directory
        .read_file_range_streamed_cancellable_with_observer(
            name,
            expected,
            0..expected,
            cancellation,
            |offset, part| {
                let start = u64::try_from(bytes.len()).map_err(|_| PlatformError::ResourceLimit)?;
                let end = bytes
                    .len()
                    .checked_add(part.len())
                    .filter(|end| *end <= count)
                    .ok_or(PlatformError::ResourceLimit)?;
                if offset != start {
                    return Err(PlatformError::SecurityPolicy);
                }
                bytes.extend_from_slice(part);
                if bytes.len() != end {
                    return Err(PlatformError::SecurityPolicy);
                }
                Ok(())
            },
            observer,
        )
        .map_err(map_private_read_error)?;
    if actual != expected || bytes.len() != count {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    Ok(bytes)
}

fn record_packed_read_phase(phase: PrivateRangeReadPhase, file: Option<FileId>, elapsed: Duration) {
    let phase = match phase {
        PrivateRangeReadPhase::ParentValidation => {
            AnalysisProfilePhase::SourceReadPackedParentValidation
        }
        PrivateRangeReadPhase::OpenFile => AnalysisProfilePhase::SourceReadPackedOpenFile,
        PrivateRangeReadPhase::InitialAccessPolicy => {
            AnalysisProfilePhase::SourceReadPackedInitialAccessPolicy
        }
        PrivateRangeReadPhase::InitialMetadata => {
            AnalysisProfilePhase::SourceReadPackedInitialMetadata
        }
        PrivateRangeReadPhase::Seek => AnalysisProfilePhase::SourceReadPackedSeek,
        PrivateRangeReadPhase::PayloadRead => AnalysisProfilePhase::SourceReadPackedPayloadRead,
        PrivateRangeReadPhase::Visitor => AnalysisProfilePhase::SourceReadPackedVisitor,
        PrivateRangeReadPhase::FinalAccessPolicy => {
            AnalysisProfilePhase::SourceReadPackedFinalAccessPolicy
        }
        PrivateRangeReadPhase::FinalMetadata => AnalysisProfilePhase::SourceReadPackedFinalMetadata,
    };
    record_analysis_profile_elapsed(
        phase,
        file,
        elapsed,
        AnalysisProfileScopeExit::ExpressionReturned,
    );
}

fn hash_cancellable(
    bytes: &[u8],
    cancellation: &Cancellation,
) -> Result<ContentHash, FirstSliceError> {
    let mut hash = blake3::Hasher::new();
    for part in bytes.chunks(64 * 1024) {
        check_cancellation(cancellation)?;
        hash.update(part);
    }
    check_cancellation(cancellation)?;
    Ok(ContentHash::from_bytes(*hash.finalize().as_bytes()))
}

struct CancellableInput<'a> {
    remaining: &'a [u8],
    cancellation: &'a Cancellation,
}
impl std::io::Read for CancellableInput<'_> {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
        self.cancellation
            .check()
            .map_err(|_| std::io::Error::other("source operation cancelled"))?;
        let count = destination.len().min(self.remaining.len()).min(64 * 1024);
        let (part, rest) = self.remaining.split_at(count);
        destination
            .get_mut(..count)
            .ok_or_else(|| std::io::Error::other("invalid decode buffer"))?
            .copy_from_slice(part);
        self.remaining = rest;
        Ok(count)
    }
}

fn matching_marker(
    repository: &PrivateDirectory<'_>,
    name: &OsStr,
    sequence: u64,
    generation: GenerationId,
    cancellation: &Cancellation,
) -> Result<(), FirstSliceError> {
    if sequence == 0 {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let marker = PrivateDirectory::open(repository.capability(), name)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let bytes = marker
        .read_file_bounded_cancellable(
            OsStr::new(ACTIVATION_MANIFEST_FILENAME),
            MAX_ACTIVATION_MANIFEST_BYTES,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    let manifest: DurableActivationManifest =
        serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let version_is_valid = match manifest.version {
        LEGACY_ACTIVATION_MANIFEST_VERSION => {
            manifest.global_activation_sequence.is_none()
                && manifest.published_generation_count.is_none()
        }
        ACTIVATION_MANIFEST_VERSION => {
            manifest
                .global_activation_sequence
                .is_some_and(|value| value > 0)
                && manifest
                    .published_generation_count
                    .is_some_and(|value| value > 0)
        }
        _ => false,
    };
    check_cancellation(cancellation)?;
    if !version_is_valid || manifest.generation != generation {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn assert_one_slot_for_records(files: &[FileRecord], content: &[Vec<u8>]) {
    use std::time::{Duration, Instant};
    assert_eq!(files.len(), 2);
    assert_eq!(content.len(), files.len());
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        root.path().join("state"),
        root.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let parent = Dir::open_ambient_dir(paths.state_dir(), ambient_authority()).unwrap();
    let sources = PrivateDirectory::create(&parent, OsStr::new("sources")).unwrap();
    let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
    let mut entries = Vec::new();
    let mut pack_bytes = Vec::new();
    for (index, (file, bytes)) in files.iter().zip(content).enumerate() {
        let ordinal = u32::try_from(index).unwrap();
        let mut output = sources
            .create_file(OsStr::new(&source_pack_name(ordinal)))
            .unwrap();
        output.write_all(bytes).unwrap();
        output.sync_all().unwrap();
        pack_bytes.push(u64::try_from(bytes.len()).unwrap());
        entries.push(DurablePackedSourceEntry {
            file: file.id,
            digest: file.content_hash,
            pack: ordinal,
            offset: 0,
            bytes: file.byte_length,
        });
    }
    let payload_bytes = pack_bytes.iter().sum();
    let wire = DurablePackedSourceIndex {
        version: SOURCE_PACK_INDEX_VERSION,
        payload_bytes,
        pack_bytes,
        entries,
    };
    let encoded = rmp_serde::to_vec(&wire).unwrap();
    let storage = DurableSourceStorage {
        version: SOURCE_STORAGE_VERSION,
        files: Some(2),
        packs: Some(2),
        index_bytes: Some(u64::try_from(encoded.len()).unwrap()),
        index_digest: Some(content_hash_bytes(&encoded)),
        payload_bytes: Some(payload_bytes),
    };
    let index = validate_packed_source_index(wire, storage, &cancellation).unwrap();
    let identity = sources.identity();
    let operation = PackedReadOperation {
        reader: PersistedSourceReader {
            repository: files[0].repository,
            sources,
            layout: PersistedSourceLayout::Packed {
                index,
                cached_pack: Mutex::new(None),
            },
        },
        descriptor: ReadOperationDescriptor {
            manifest: content_hash_bytes(b"private two-pack control"),
            storage,
            repository_identity: identity,
            generation_identity: identity,
            sources_identity: identity,
        },
        index_reads: Cell::new(0),
        pack_reads: Cell::new(0),
        range_reads: Cell::new(0),
    };
    let work = OperationWork::for_test(32 * 1024 * 1024);
    for index in [0, 1, 0] {
        let actual = operation.read(&files[index], &work, &cancellation).unwrap();
        assert_eq!(actual.content(), content[index]);
    }
    assert_eq!(operation.counters(), (0, 3, 0));
    // A failed replacement must not install the provisional second pack.
    std::fs::write(
        paths.state_dir().join("sources").join(source_pack_name(1)),
        b"short",
    )
    .unwrap();
    assert!(operation.read(&files[1], &work, &cancellation).is_err());
    let PersistedSourceLayout::Packed { cached_pack, .. } = &operation.reader.layout else {
        panic!("packed control");
    };
    assert_eq!(cached_pack.lock().unwrap().as_ref().unwrap().ordinal, 0);
    assert_eq!(operation.counters(), (0, 4, 0));
}

#[cfg(test)]
pub(crate) fn coalesce_legacy_source_pack_for_test(state: &Path, receipt: &FirstSliceIndexReceipt) {
    let directory = state
        .join(DURABLE_DIRECTORY)
        .join(REPOSITORIES_DIRECTORY)
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let sources = directory.join(SOURCES_DIRECTORY);
    let mut manifest: DurableGenerationManifest =
        serde_json::from_slice(&std::fs::read(directory.join(MANIFEST_FILENAME)).unwrap()).unwrap();
    let mut index: DurablePackedSourceIndex =
        rmp_serde::from_slice(&std::fs::read(sources.join(SOURCE_PACK_INDEX_FILENAME)).unwrap())
            .unwrap();
    let mut bytes = Vec::new();
    for ordinal in 0..index.pack_bytes.len() {
        bytes.extend_from_slice(
            &std::fs::read(sources.join(source_pack_name(u32::try_from(ordinal).unwrap())))
                .unwrap(),
        );
    }
    assert_eq!(u64::try_from(bytes.len()).unwrap(), index.payload_bytes);
    assert!(index.payload_bytes > SOURCE_PACK_WRITE_TARGET_BYTES);
    assert!(index.payload_bytes <= SOURCE_PACK_TARGET_BYTES);
    for entry in &mut index.entries {
        let previous_packs = index.pack_bytes[..usize::try_from(entry.pack).unwrap()]
            .iter()
            .sum::<u64>();
        entry.offset += previous_packs;
        entry.pack = 0;
    }
    for ordinal in 1..index.pack_bytes.len() {
        std::fs::remove_file(sources.join(source_pack_name(u32::try_from(ordinal).unwrap())))
            .unwrap();
    }
    index.pack_bytes = vec![index.payload_bytes];
    let encoded = rmp_serde::to_vec_named(&index).unwrap();
    let storage = manifest.source_storage.as_mut().unwrap();
    storage.packs = Some(1);
    storage.index_bytes = Some(u64::try_from(encoded.len()).unwrap());
    storage.index_digest = Some(content_hash_bytes(&encoded));
    write_test_recovery_file(&sources, &source_pack_name(0), &bytes).unwrap();
    write_test_recovery_file(&sources, SOURCE_PACK_INDEX_FILENAME, &encoded).unwrap();
    write_test_recovery_file(
        &directory,
        MANIFEST_FILENAME,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[cfg(test)]
pub(crate) fn replace_index_with_matching_descriptor_for_test(
    state: &Path,
    receipt: &FirstSliceIndexReceipt,
    encoded: &[u8],
) {
    let directory = state
        .join(DURABLE_DIRECTORY)
        .join(REPOSITORIES_DIRECTORY)
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let bytes = std::fs::read(directory.join(MANIFEST_FILENAME)).unwrap();
    let mut manifest: DurableGenerationManifest = serde_json::from_slice(&bytes).unwrap();
    let mut persisted = receipt.clone();
    persisted.logical_snapshot = None;
    assert_eq!(manifest.receipt, persisted);
    let storage = manifest.source_storage.as_mut().unwrap();
    storage.index_bytes = Some(u64::try_from(encoded.len()).unwrap());
    storage.index_digest = Some(content_hash_bytes(encoded));
    write_test_recovery_file(
        &directory.join(SOURCES_DIRECTORY),
        SOURCE_PACK_INDEX_FILENAME,
        encoded,
    )
    .unwrap();
    write_test_recovery_file(
        &directory,
        MANIFEST_FILENAME,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let actual = std::fs::read(
        directory
            .join(SOURCES_DIRECTORY)
            .join(SOURCE_PACK_INDEX_FILENAME),
    )
    .unwrap();
    assert_eq!(actual, encoded);
    let observed: DurableGenerationManifest =
        serde_json::from_slice(&std::fs::read(directory.join(MANIFEST_FILENAME)).unwrap()).unwrap();
    let observed_storage = observed.source_storage.unwrap();
    assert_eq!(
        content_hash_bytes(&actual),
        observed_storage.index_digest.unwrap()
    );
    assert_eq!(
        u64::try_from(actual.len()).unwrap(),
        observed_storage.index_bytes.unwrap()
    );
}

#[cfg(test)]
mod index_decoder_cancellation_controls {
    use super::*;
    use std::io::Read;

    #[test]
    fn index_decoder_cancellation_between_reads_preserves_the_first_typed_reason() {
        struct CancelAfterRead<'a> {
            inner: CancellableInput<'a>,
            successful_reads: usize,
        }
        impl Read for CancelAfterRead<'_> {
            fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
                let count = self.inner.read(destination)?;
                self.successful_reads += 1;
                if self.successful_reads == 1 {
                    assert!(
                        self.inner
                            .cancellation
                            .cancel(CancellationReason::ClientRequest)
                    );
                    assert!(
                        !self
                            .inner
                            .cancellation
                            .cancel(CancellationReason::DeadlineExceeded)
                    );
                }
                Ok(count)
            }
        }
        let cancellation = Cancellation::new();
        let encoded = [0x94, 0x01, 0x00, 0x90, 0x90];
        let mut input = CancelAfterRead {
            inner: CancellableInput {
                remaining: &encoded,
                cancellation: &cancellation,
            },
            successful_reads: 0,
        };
        let decoded: Result<DurablePackedSourceIndex, FirstSliceError> =
            rmp_serde::from_read(&mut input).map_err(|_| {
                current_cancellation(&cancellation).unwrap_or(FirstSliceError::CatalogCorrupt)
            });
        assert_eq!(input.successful_reads, 1);
        assert!(matches!(
            decoded,
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
    }
}
