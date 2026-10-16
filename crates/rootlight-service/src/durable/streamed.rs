//! Immutable source blob persistence and authenticated streamed restoration.
//!
//! Large payloads use the existing blob/pointer format; complete private-file
//! authentication replaces snapshot allocation, without changing parser limits.

use rootlight_vfs::{StreamedSourceBytesIdentity, visit_verified_source_bytes_cancellable};

use super::*;
use crate::{source_backing::VerifiedSourceVisitor, source_owner::CapturedGenerationSources};

pub(crate) enum SourceWriter<'a> {
    Packed(DurablePackedSourceWriter<'a>),
    Blobs(BlobWriter<'a>),
}

impl SourceWriter<'_> {
    pub(crate) fn push(&mut self, snapshot: &SourceSnapshot) -> Result<(), FirstSliceError> {
        match self {
            Self::Packed(writer) => writer.push(snapshot),
            Self::Blobs(writer) => writer.push_snapshot(snapshot),
        }
    }

    pub(crate) fn push_captured(
        &mut self,
        owner: &CapturedGenerationSources,
        file: &FileRecord,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        match self {
            Self::Packed(_) => Err(FirstSliceError::CatalogCorrupt),
            Self::Blobs(writer) => writer.push_captured(owner, file, cancellation),
        }
    }

    pub(crate) fn finish(self) -> Result<DurableSourceWrite, FirstSliceError> {
        match self {
            Self::Packed(writer) => writer.finish(),
            Self::Blobs(writer) => writer.finish(),
        }
    }
}

pub(crate) struct BlobWriter<'a> {
    prepared: &'a DurablePreparedGeneration,
    sources: PrivateDirectory<'a>,
    blobs: PrivateDirectory<'static>,
    written: u64,
    referenced: u64,
}

impl DurablePreparedGeneration {
    pub(crate) fn begin_source_write(
        &self,
        captured: Option<&CapturedGenerationSources>,
    ) -> Result<SourceWriter<'_>, FirstSliceError> {
        let Some(captured) = captured else {
            return self.begin_packed_source_write().map(SourceWriter::Packed);
        };
        *self
            .lexical_streamed_limit
            .lock()
            .map_err(|_| FirstSliceError::Catalog)? = Some(StreamedProjectionLimit::new(
            captured.snapshot_file_bytes(),
        )?);
        let sources = self
            .staging()
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        let blobs = ensure_private_directory(
            self.repository().capability(),
            OsStr::new(SOURCE_BLOBS_DIRECTORY),
        )?;
        self.set_source_storage(legacy_source_storage())?;
        Ok(SourceWriter::Blobs(BlobWriter {
            prepared: self,
            sources,
            blobs,
            written: 0,
            referenced: 0,
        }))
    }

    pub(crate) fn write_sources_with_streamed(
        &self,
        sources: &[RustSourceInput],
        captured: Option<&CapturedGenerationSources>,
        cancellation: &Cancellation,
    ) -> Result<DurableSourceWrite, FirstSliceError> {
        let Some(captured) = captured else {
            return self.write_sources(sources);
        };
        let mut writer = self.begin_source_write(Some(captured))?;
        for source in sources {
            check_cancellation(cancellation)?;
            writer.push(&source.snapshot)?;
        }
        if let SourceWriter::Blobs(writer) = &mut writer {
            for (file, source) in captured.files() {
                writer.push_streamed(
                    file,
                    source.capture.content_hash(),
                    source.capture.metadata().length,
                    cancellation,
                    |visitor| {
                        captured.visit(
                            file,
                            source.capture.metadata().length,
                            cancellation,
                            visitor,
                        )
                    },
                )?;
            }
        }
        writer.finish()
    }
}

impl BlobWriter<'_> {
    fn push_snapshot(&mut self, source: &SourceSnapshot) -> Result<(), FirstSliceError> {
        let digest = source.content_hash();
        let length = source.metadata().length;
        let created = persist_source_blob(&self.blobs, digest, source.content())?;
        self.record(source.file(), digest, length, created)
    }

    fn push_captured(
        &mut self,
        owner: &CapturedGenerationSources,
        file: &FileRecord,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        owner.source(file)?;
        self.push_streamed(
            file.id,
            file.content_hash,
            file.byte_length,
            cancellation,
            |visitor| owner.visit(file.id, file.byte_length, cancellation, visitor),
        )
    }

    fn push_streamed(
        &mut self,
        file: FileId,
        digest: ContentHash,
        length: u64,
        cancellation: &Cancellation,
        visit: impl FnOnce(
            &mut dyn FnMut(u64, &[u8]) -> Result<(), PlatformError>,
        ) -> Result<u64, FirstSliceError>,
    ) -> Result<(), FirstSliceError> {
        let name = digest.to_string();
        match self.blobs.capability().symlink_metadata(&name) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let blob = PrivateDirectory::open(self.blobs.capability(), OsStr::new(&name))
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                visit_verified_source_bytes_cancellable(
                    &blob,
                    OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME),
                    StreamedSourceBytesIdentity {
                        byte_length: length,
                        content_hash: digest,
                    },
                    length,
                    cancellation,
                    |_, _| Ok(()),
                )
                .map_err(|_| {
                    check_cancellation(cancellation)
                        .err()
                        .unwrap_or(FirstSliceError::CatalogCorrupt)
                })?;
                self.record(file, digest, length, false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let staging_name = random_source_blob_staging_name(digest)?;
                let staging =
                    PrivateDirectory::create(self.blobs.capability(), OsStr::new(&staging_name))
                        .map_err(|_| FirstSliceError::Catalog)?;
                let result = (|| {
                    let mut payload = staging
                        .create_file(OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME))
                        .map_err(|_| FirstSliceError::Catalog)?;
                    let count = visit(&mut |_, bytes| {
                        payload
                            .write_all(bytes)
                            .map_err(|source| PlatformError::Io {
                                operation: "write streamed source blob",
                                source,
                            })
                    })?;
                    if count != length {
                        return Err(FirstSliceError::CatalogCorrupt);
                    }
                    payload.sync_all().map_err(|_| FirstSliceError::Catalog)?;
                    drop(payload);
                    staging.sync_all().map_err(|_| FirstSliceError::Catalog)
                })();
                if let Err(error) = result {
                    let _ = staging.remove();
                    return Err(error);
                }
                match staging.publish_noreplace(self.blobs.capability(), OsStr::new(&name)) {
                    Ok(published) => {
                        // Record ownership before a fallible durability barrier so
                        // preparation rollback can reclaim a committed new blob.
                        self.prepared
                            .created_source_blobs
                            .lock()
                            .map_err(|_| FirstSliceError::Catalog)?
                            .insert(digest);
                        published.sync_all().map_err(|_| FirstSliceError::Catalog)?;
                        self.record(file, digest, length, true)
                    }
                    Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
                        directory.remove().map_err(|_| FirstSliceError::Catalog)?;
                        Err(FirstSliceError::Catalog)
                    }
                    Err(_) => Err(FirstSliceError::Catalog),
                }
            }
            Err(_) => Err(FirstSliceError::Catalog),
        }
    }

    fn record(
        &mut self,
        file: FileId,
        digest: ContentHash,
        length: u64,
        created: bool,
    ) -> Result<(), FirstSliceError> {
        if created {
            self.prepared
                .created_source_blobs
                .lock()
                .map_err(|_| FirstSliceError::Catalog)?
                .insert(digest);
            self.written = self
                .written
                .checked_add(length)
                .ok_or(FirstSliceError::Limits)?;
        } else if !self
            .prepared
            .created_source_blobs
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?
            .contains(&digest)
        {
            self.referenced = self
                .referenced
                .checked_add(length)
                .ok_or(FirstSliceError::Limits)?;
        }
        let pointer = encode_source_pointer(digest, length)?;
        let mut output = self
            .sources
            .create_file(OsStr::new(&file.to_string()))
            .map_err(|_| FirstSliceError::Catalog)?;
        output
            .write_all(&pointer)
            .map_err(|_| FirstSliceError::Catalog)?;
        output.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        self.written = self
            .written
            .checked_add(u64::try_from(pointer.len()).map_err(|_| FirstSliceError::Limits)?)
            .ok_or(FirstSliceError::Limits)?;
        Ok(())
    }

    fn finish(self) -> Result<DurableSourceWrite, FirstSliceError> {
        self.sources
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.blobs
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.prepared.account_staging_bytes(self.written)?;
        Ok(DurableSourceWrite {
            newly_written_bytes: self.written,
            referenced_bytes: self.referenced,
        })
    }
}

impl DurableCatalog {
    pub(crate) fn visit_source(
        &self,
        file: &FileRecord,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut VerifiedSourceVisitor<'_>,
    ) -> Result<u64, FirstSliceError> {
        if file.byte_length > maximum {
            return Err(FirstSliceError::BudgetExceeded);
        }
        check_cancellation(cancellation)?;
        let repository = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&file.repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let generation = PrivateDirectory::open(
            repository.capability(),
            OsStr::new(&file.generation.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let manifest = generation
            .read_file_bounded_cancellable(
                OsStr::new(MANIFEST_FILENAME),
                MAX_MANIFEST_BYTES,
                cancellation,
            )
            .map_err(map_private_read_error)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&manifest).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let reader = PersistedSourceReader::open(
            &repository,
            &generation,
            file.repository,
            source_storage_layout(manifest.version, manifest.source_storage)?,
        )?;
        let identity = StreamedSourceBytesIdentity {
            byte_length: file.byte_length,
            content_hash: file.content_hash,
        };
        let result = match &reader.layout {
            PersistedSourceLayout::Inline => visit_verified_source_bytes_cancellable(
                &reader.sources,
                OsStr::new(&file.id.to_string()),
                identity,
                maximum,
                cancellation,
                visitor,
            ),
            PersistedSourceLayout::Blobs(blobs) => {
                let bytes = reader
                    .sources
                    .read_file_bounded_cancellable(
                        OsStr::new(&file.id.to_string()),
                        MAX_SOURCE_POINTER_BYTES,
                        cancellation,
                    )
                    .map_err(map_private_read_error)?;
                let pointer = decode_source_pointer(&bytes)?;
                if pointer.digest != file.content_hash || pointer.bytes != file.byte_length {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let blob = PrivateDirectory::open(
                    blobs.capability(),
                    OsStr::new(&pointer.digest.to_string()),
                )
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                visit_verified_source_bytes_cancellable(
                    &blob,
                    OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME),
                    identity,
                    maximum,
                    cancellation,
                    visitor,
                )
            }
            PersistedSourceLayout::Packed { index, .. } => {
                let entry = index
                    .entries
                    .get(&file.id)
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                if entry.digest != file.content_hash || entry.bytes != file.byte_length {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let pack_bytes = index
                    .pack_bytes
                    .get(usize::try_from(entry.pack).map_err(|_| FirstSliceError::Limits)?)
                    .copied()
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                let end = entry
                    .offset
                    .checked_add(entry.bytes)
                    .filter(|end| *end <= pack_bytes)
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                let mut consumed = 0u64;
                let mut hash = blake3::Hasher::new();
                let actual = reader
                    .sources
                    .read_file_streamed_cancellable(
                        OsStr::new(&source_pack_name(entry.pack)),
                        pack_bytes,
                        cancellation,
                        |offset, bytes| {
                            let chunk_end = offset
                                .checked_add(
                                    u64::try_from(bytes.len())
                                        .map_err(|_| PlatformError::ResourceLimit)?,
                                )
                                .ok_or(PlatformError::ResourceLimit)?;
                            let start = offset.max(entry.offset);
                            let stop = chunk_end.min(end);
                            if start < stop {
                                let from = usize::try_from(start - offset)
                                    .map_err(|_| PlatformError::ResourceLimit)?;
                                let to = usize::try_from(stop - offset)
                                    .map_err(|_| PlatformError::ResourceLimit)?;
                                let selected =
                                    bytes.get(from..to).ok_or(PlatformError::SecurityPolicy)?;
                                hash.update(selected);
                                visitor(consumed, selected)?;
                                consumed = consumed
                                    .checked_add(stop - start)
                                    .ok_or(PlatformError::ResourceLimit)?;
                            }
                            Ok(())
                        },
                    )
                    .map_err(map_private_read_error)?;
                if actual != pack_bytes
                    || consumed != file.byte_length
                    || ContentHash::from_bytes(*hash.finalize().as_bytes()) != file.content_hash
                {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                check_cancellation(cancellation)?;
                return Ok(consumed);
            }
        };
        result.map_err(|_| {
            check_cancellation(cancellation)
                .err()
                .unwrap_or(FirstSliceError::CatalogCorrupt)
        })
    }
}
