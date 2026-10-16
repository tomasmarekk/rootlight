//! Private immutable source ownership independent of parser snapshots.
//!
//! Capture sessions own unpublished children; generation owners retain both the
//! child and its instance. Failed removal remains charged for an explicit retry.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Write},
    sync::{Arc, Mutex},
};

use cap_std::fs::Dir;
use rootlight_discovery::{
    DiscoveryError, ManifestInput, StreamedDiscoverySource, StreamedSourceReceiver,
};
use rootlight_ids::{FileId, GenerationId, RepositoryId};
use rootlight_ir::FileRecord;
use rootlight_vfs::{
    RelativePath, RepositoryRoot, SnapshotMetadata, StreamedSourceBytesIdentity,
    StreamedSourceCapture, StreamingSourceTextValidator, TextEncoding,
    platform::{PlatformFileIdentity, PrivateDirectory},
    visit_verified_source_bytes_cancellable,
};

use super::{
    Cancellation, FirstSliceError, check_cancellation, source_backing::VerifiedSourceVisitor,
};

const CLASSIFICATION_PREFIX_BYTES: usize = rootlight_discovery::MAX_STREAMED_CLASSIFICATION_BYTES;

/// Explicit payload and metadata bounds for private streamed source ownership.
///
/// Payload bytes are separate from parser/snapshot memory and request response or
/// verification limits. Physical filesystem admission remains the storage owner's
/// responsibility; these values do not claim actual allocated disk bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceStreamedStorageLimits {
    maximum_file_bytes: u64,
    maximum_payload_bytes: u64,
    maximum_files: usize,
}

impl FirstSliceStreamedStorageLimits {
    /// Creates checked explicit file, retained-payload and metadata-entry bounds.
    ///
    /// # Errors
    /// Returns [`FirstSliceError::Limits`] for zero or contradictory bounds.
    pub fn new(
        maximum_file_bytes: u64,
        maximum_payload_bytes: u64,
        maximum_files: usize,
    ) -> Result<Self, FirstSliceError> {
        if maximum_file_bytes == 0
            || maximum_payload_bytes < maximum_file_bytes
            || maximum_files == 0
        {
            return Err(FirstSliceError::Limits);
        }
        Ok(Self {
            maximum_file_bytes,
            maximum_payload_bytes,
            maximum_files,
        })
    }
}

/// Logical payload reservations, including unresolved cleanup debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstSliceStreamedStorageUsage {
    /// Complete payload bytes charged until confirmed private removal.
    pub reserved_payload_bytes: u64,
    /// Source file entries charged independently from their payload size.
    pub reserved_files: usize,
    /// Owned child directories whose prior removal did not complete.
    pub pending_cleanup_directories: usize,
}

/// A fresh source instance under a caller-provided account-private parent.
///
/// Clones retain the same owner. The instance survives every capture and source
/// lease; no generation is retained by the instance, avoiding reference cycles.
/// Final drop attempts cleanup without reporting success. Callers needing a
/// confirmed result must retain this owner and use [`Self::finish_if_idle`]. A
/// failed last drop can leave private files; later filesystem admission and
/// durable inventory still charge those bytes without adopting that instance.
#[derive(Clone)]
pub struct FirstSliceEphemeralSources {
    instance: Arc<SourceInstance>,
}

impl fmt::Debug for FirstSliceEphemeralSources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FirstSliceEphemeralSources")
            .finish_non_exhaustive()
    }
}

impl FirstSliceEphemeralSources {
    /// Exclusively creates one fresh private child without adopting existing data.
    ///
    /// # Errors
    /// Returns a typed service error for an invalid/private-parent policy, name
    /// collision, filesystem failure or inconsistent limits. The caller's parent
    /// is retained but is never removed or permission-adjusted by this owner.
    pub fn create(
        parent: &Dir,
        instance_name: &OsStr,
        limits: FirstSliceStreamedStorageLimits,
    ) -> Result<Self, FirstSliceError> {
        let retained_parent = parent.try_clone().map_err(|_| FirstSliceError::Catalog)?;
        let directory = PrivateDirectory::create(parent, instance_name)
            .map_err(|_| FirstSliceError::Catalog)?;
        let identity = directory.identity();
        Ok(Self {
            instance: Arc::new(SourceInstance {
                directory: Some(directory),
                parent: retained_parent,
                name: instance_name.to_owned(),
                identity,
                root_cleanup_failed: false,
                limits,
                charges: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Returns conservative logical usage, retaining failed cleanup charges.
    ///
    /// # Errors
    /// Returns [`FirstSliceError::Retention`] if bookkeeping is unavailable.
    pub fn usage(&self) -> Result<FirstSliceStreamedStorageUsage, FirstSliceError> {
        self.instance.usage()
    }

    /// Retries failed child cleanup after their last generation owner released it.
    ///
    /// # Errors
    /// Returns a storage or integrity error without reporting unconfirmed bytes
    /// as freed. A replaced child identity is never recursively removed.
    pub fn retry_cleanup(&self) -> Result<(), FirstSliceError> {
        self.instance.retry_cleanup()
    }

    /// Removes the instance only when no clone, capture or generation still retains it.
    ///
    /// `false` means an owner or lease remains. Failed unlink or parent durability
    /// stays observable through usage and can be retried on this same owner.
    /// A successfully finished instance cannot accept further captures.
    ///
    /// # Errors
    /// Returns identity, privacy, bookkeeping or filesystem cleanup failures.
    pub fn finish_if_idle(&mut self) -> Result<bool, FirstSliceError> {
        let Some(instance) = Arc::get_mut(&mut self.instance) else {
            return Ok(false);
        };
        instance.retry_cleanup()?;
        if !instance
            .charges
            .get_mut()
            .map_err(|_| FirstSliceError::Retention)?
            .is_empty()
        {
            return Ok(false);
        }
        instance.finish_root(PrivateDirectory::sync_verified_parent)?;
        Ok(true)
    }

    pub(super) fn begin(&self, repository: RepositoryId) -> SourceCaptureSession {
        SourceCaptureSession {
            instance: Arc::clone(&self.instance),
            repository,
            directory: None,
            name: None,
            files: BTreeMap::new(),
        }
    }
}

struct SourceInstance {
    directory: Option<PrivateDirectory<'static>>,
    parent: Dir,
    name: OsString,
    identity: PlatformFileIdentity,
    root_cleanup_failed: bool,
    limits: FirstSliceStreamedStorageLimits,
    charges: Mutex<BTreeMap<OsString, DirectoryCharge>>,
}

#[derive(Clone)]
struct DirectoryCharge {
    identity: PlatformFileIdentity,
    payload_bytes: u64,
    files: usize,
    cleanup_failed: bool,
    removed: bool,
}

impl SourceInstance {
    fn finish_root(
        &mut self,
        synchronize: impl FnOnce(&Dir) -> Result<(), rootlight_vfs::platform::PlatformError>,
    ) -> Result<(), FirstSliceError> {
        self.root_cleanup_failed = true;
        if let Some(directory) = self.directory.take() {
            if directory.identity() != self.identity {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            directory.remove().map_err(|_| FirstSliceError::Catalog)?;
        } else {
            match self.parent.symlink_metadata(&self.name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Ok(_) => {
                    let directory = PrivateDirectory::open(&self.parent, &self.name)
                        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                    if directory.identity() != self.identity {
                        return Err(FirstSliceError::CatalogCorrupt);
                    }
                    directory.remove().map_err(|_| FirstSliceError::Catalog)?;
                }
                Err(_) => return Err(FirstSliceError::Catalog),
            }
        }
        match self.parent.symlink_metadata(&self.name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(FirstSliceError::CatalogCorrupt),
        }
        synchronize(&self.parent).map_err(|_| FirstSliceError::Catalog)?;
        self.root_cleanup_failed = false;
        Ok(())
    }

    fn directory(&self) -> Result<&PrivateDirectory<'static>, FirstSliceError> {
        self.directory.as_ref().ok_or(FirstSliceError::Retention)
    }

    fn usage(&self) -> Result<FirstSliceStreamedStorageUsage, FirstSliceError> {
        let charges = self
            .charges
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        charges.values().try_fold(
            FirstSliceStreamedStorageUsage {
                reserved_payload_bytes: 0,
                reserved_files: 0,
                pending_cleanup_directories: usize::from(self.root_cleanup_failed),
            },
            |mut usage, charge| {
                usage.reserved_payload_bytes = usage
                    .reserved_payload_bytes
                    .checked_add(charge.payload_bytes)
                    .ok_or(FirstSliceError::Limits)?;
                usage.reserved_files = usage
                    .reserved_files
                    .checked_add(charge.files)
                    .ok_or(FirstSliceError::Limits)?;
                usage.pending_cleanup_directories += usize::from(charge.cleanup_failed);
                Ok(usage)
            },
        )
    }

    fn reserve(&self, name: &OsStr, bytes: u64) -> Result<(), FirstSliceError> {
        let mut charges = self
            .charges
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let (reserved, files) =
            charges
                .values()
                .try_fold((0_u64, 0_usize), |(bytes, files), row| {
                    Ok::<_, FirstSliceError>((
                        bytes
                            .checked_add(row.payload_bytes)
                            .ok_or(FirstSliceError::Limits)?,
                        files
                            .checked_add(row.files)
                            .ok_or(FirstSliceError::Limits)?,
                    ))
                })?;
        if bytes > self.limits.maximum_file_bytes
            || reserved
                .checked_add(bytes)
                .is_none_or(|sum| sum > self.limits.maximum_payload_bytes)
            || files >= self.limits.maximum_files
        {
            return Err(FirstSliceError::Limits);
        }
        let charge = charges.get_mut(name).ok_or(FirstSliceError::Retention)?;
        charge.payload_bytes = charge
            .payload_bytes
            .checked_add(bytes)
            .ok_or(FirstSliceError::Limits)?;
        charge.files = charge.files.checked_add(1).ok_or(FirstSliceError::Limits)?;
        Ok(())
    }

    fn cleanup(
        &self,
        name: &OsStr,
        directory: PrivateDirectory<'static>,
    ) -> Result<(), FirstSliceError> {
        self.cleanup_with(
            name,
            directory,
            |directory| directory.remove().is_ok(),
            |directory| directory.sync_all(),
        )
    }

    fn cleanup_with(
        &self,
        name: &OsStr,
        directory: PrivateDirectory<'static>,
        remove: impl FnOnce(PrivateDirectory<'static>) -> bool,
        synchronize: impl FnOnce(
            &PrivateDirectory<'static>,
        ) -> Result<(), rootlight_vfs::platform::PlatformError>,
    ) -> Result<(), FirstSliceError> {
        let identity = directory.identity();
        {
            let charges = self
                .charges
                .lock()
                .map_err(|_| FirstSliceError::Retention)?;
            if charges
                .get(name)
                .is_none_or(|charge| charge.identity != identity || charge.removed)
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let removed = remove(directory);
        let mut charges = self
            .charges
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let charge = charges.get_mut(name).ok_or(FirstSliceError::Retention)?;
        charge.removed = removed;
        charge.cleanup_failed = true;
        if !removed {
            return Err(FirstSliceError::Catalog);
        }
        synchronize(self.directory()?).map_err(|_| FirstSliceError::Catalog)?;
        charges.remove(name);
        Ok(())
    }

    fn retry_cleanup(&self) -> Result<(), FirstSliceError> {
        let pending = self
            .charges
            .lock()
            .map_err(|_| FirstSliceError::Retention)?
            .iter()
            .filter(|(_, charge)| charge.cleanup_failed)
            .map(|(name, charge)| (name.clone(), charge.clone()))
            .collect::<Vec<_>>();
        for (name, charge) in pending {
            let absent = match self.directory()?.capability().symlink_metadata(&name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                Ok(_) => false,
                Err(_) => return Err(FirstSliceError::Catalog),
            };
            if charge.removed || absent {
                match self.directory()?.capability().symlink_metadata(&name) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    _ => return Err(FirstSliceError::CatalogCorrupt),
                }
                self.directory()?
                    .sync_all()
                    .map_err(|_| FirstSliceError::Catalog)?;
                self.charges
                    .lock()
                    .map_err(|_| FirstSliceError::Retention)?
                    .remove(&name);
            } else {
                let directory = PrivateDirectory::open(self.directory()?.capability(), &name)
                    .map_err(|_| FirstSliceError::Catalog)?;
                if directory.identity() != charge.identity {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                self.cleanup(&name, directory)?;
            }
        }
        Ok(())
    }
}

impl Drop for SourceInstance {
    fn drop(&mut self) {
        // Generation and read leases retain this instance. Only its final owner
        // can attempt removal; explicit cleanup remains the observable contract.
        if self
            .charges
            .get_mut()
            .is_ok_and(|charges| charges.is_empty())
            && let Some(directory) = self.directory.take()
        {
            let _ = directory.remove();
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CapturedSourceFile {
    pub(super) capture: StreamedSourceCapture,
    pub(super) encoding: Option<TextEncoding>,
    pub(super) contains_nul: bool,
    pub(super) private_name: OsString,
}

pub(super) struct SourceCaptureSession {
    instance: Arc<SourceInstance>,
    repository: RepositoryId,
    directory: Option<PrivateDirectory<'static>>,
    name: Option<OsString>,
    files: BTreeMap<FileId, CapturedSourceFile>,
}

pub(super) enum SourceCaptureReceiver<'a> {
    Explicit(SourceCaptureSession),
    Durable {
        catalog: &'a crate::durable::DurableCatalog,
        repository: RepositoryId,
        session: Option<SourceCaptureSession>,
    },
}

impl<'a> SourceCaptureReceiver<'a> {
    pub(super) fn new(
        owner: Option<&FirstSliceEphemeralSources>,
        catalog: Option<&'a crate::durable::DurableCatalog>,
        repository: RepositoryId,
    ) -> Option<Self> {
        owner
            .map(|owner| Self::Explicit(owner.begin(repository)))
            .or_else(|| {
                catalog.map(|catalog| Self::Durable {
                    catalog,
                    repository,
                    session: None,
                })
            })
    }

    pub(super) fn maximum_file_bytes(&self) -> u64 {
        match self {
            Self::Explicit(session) => session.instance.limits.maximum_file_bytes,
            Self::Durable { .. } => u64::MAX,
        }
    }

    pub(super) fn into_generation(
        self,
        generation: GenerationId,
        inputs: &[ManifestInput],
        snapshot_file_bytes: u64,
    ) -> Result<Option<Arc<CapturedGenerationSources>>, FirstSliceError> {
        match self {
            Self::Explicit(session)
            | Self::Durable {
                session: Some(session),
                ..
            } => session.into_generation(generation, inputs, snapshot_file_bytes),
            Self::Durable { session: None, .. } => Ok(None),
        }
    }
}

impl StreamedSourceReceiver for SourceCaptureReceiver<'_> {
    fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError> {
        let session = match self {
            Self::Explicit(session) => session,
            Self::Durable {
                catalog,
                repository,
                session,
            } => {
                if session.is_none() {
                    *session = Some(
                        catalog
                            .create_source_owner()
                            .map_err(|_| DiscoveryError::StreamedCaptureUnavailable)?
                            .begin(*repository),
                    );
                }
                session
                    .as_mut()
                    .ok_or(DiscoveryError::StreamedCaptureUnavailable)?
            }
        };
        StreamedSourceReceiver::capture(session, root, path, observed, cancellation)
    }
}

impl SourceCaptureSession {
    pub(super) fn into_generation(
        mut self,
        generation: GenerationId,
        inputs: &[ManifestInput],
        snapshot_file_bytes: u64,
    ) -> Result<Option<Arc<CapturedGenerationSources>>, FirstSliceError> {
        let mut admitted = false;
        for input in inputs {
            let Some(source) = self.files.get(&input.file) else {
                if input.bytes > snapshot_file_bytes {
                    return Err(FirstSliceError::DiscoveryDrift);
                }
                continue;
            };
            admitted = true;
            if source.capture.path().as_str() != input.path
                || source.capture.content_hash() != input.content_hash
                || source.capture.metadata().length != input.bytes
                || source.encoding_label() != input.encoding
                || input.bytes <= snapshot_file_bytes
            {
                return Err(FirstSliceError::DiscoveryDrift);
            }
        }
        if !admitted {
            return Ok(None);
        }
        let admitted = inputs
            .iter()
            .map(|input| input.file)
            .collect::<std::collections::BTreeSet<_>>();
        self.files.retain(|file, _| admitted.contains(file));
        Ok(Some(Arc::new(CapturedGenerationSources {
            instance: Arc::clone(&self.instance),
            repository: self.repository,
            generation,
            snapshot_file_bytes,
            directory: self.directory.take(),
            name: self.name.take().ok_or(FirstSliceError::Retention)?,
            files: std::mem::take(&mut self.files),
        })))
    }

    pub(super) fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<(CapturedSourceFile, Vec<u8>), FirstSliceError> {
        check_cancellation(cancellation)?;
        if root.repository() != self.repository {
            return Err(FirstSliceError::GenerationMismatch);
        }
        if let Some(source) = self.files.get(&root.file_id(path)) {
            if source.capture.metadata() != observed || source.capture.path() != path {
                return Err(FirstSliceError::DiscoveryDrift);
            }
            let mut prefix = Vec::new();
            visit_verified_source_bytes_cancellable(
                self.directory.as_ref().ok_or(FirstSliceError::Retention)?,
                &source.private_name,
                StreamedSourceBytesIdentity {
                    byte_length: observed.length,
                    content_hash: source.capture.content_hash(),
                },
                observed.length,
                cancellation,
                |_, bytes| {
                    let count = bytes
                        .len()
                        .min(CLASSIFICATION_PREFIX_BYTES.saturating_sub(prefix.len()));
                    prefix
                        .try_reserve(count)
                        .map_err(|_| rootlight_vfs::platform::PlatformError::ResourceLimit)?;
                    prefix.extend_from_slice(&bytes[..count]);
                    Ok(())
                },
            )
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            return Ok((source.clone(), prefix));
        }
        self.ensure_directory()?;
        let name = self.name.as_ref().ok_or(FirstSliceError::Retention)?;
        self.instance.reserve(name, observed.length)?;
        let directory = self.directory.as_ref().ok_or(FirstSliceError::Retention)?;
        let private_name = OsString::from(root.file_id(path).to_string());
        let mut writer = directory
            .create_file(&private_name)
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut sink = CaptureSink {
            output: &mut writer,
            validator: StreamingSourceTextValidator::new(),
            prefix: Vec::new(),
            cancellation,
        };
        let captured = root
            .capture_streamed_cancellable(path, observed.length.max(1), cancellation, &mut sink)
            .map_err(|_| {
                check_cancellation(cancellation)
                    .err()
                    .unwrap_or(FirstSliceError::DiscoveryDrift)
            })?;
        if captured.metadata() != observed {
            return Err(FirstSliceError::DiscoveryDrift);
        }
        let contains_nul = sink.validator.contains_nul();
        let encoding = sink.validator.finish();
        let prefix = sink.prefix;
        writer.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        drop(writer);
        directory.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        let source = CapturedSourceFile {
            capture: captured,
            encoding,
            contains_nul,
            private_name,
        };
        self.files.insert(source.capture.file(), source.clone());
        check_cancellation(cancellation)?;
        Ok((source, prefix))
    }

    fn ensure_directory(&mut self) -> Result<(), FirstSliceError> {
        if self.directory.is_some() {
            return Ok(());
        }
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
        let name = OsString::from(format!("capture-{:032x}", u128::from_le_bytes(nonce)));
        let mut charges = self
            .instance
            .charges
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        if charges.len() >= self.instance.limits.maximum_files {
            return Err(FirstSliceError::Limits);
        }
        let directory = PrivateDirectory::create(self.instance.directory()?.capability(), &name)
            .map_err(|_| FirstSliceError::Catalog)?;
        charges.insert(
            name.clone(),
            DirectoryCharge {
                identity: directory.identity(),
                payload_bytes: 0,
                files: 0,
                cleanup_failed: false,
                removed: false,
            },
        );
        self.name = Some(name);
        self.directory = Some(directory);
        Ok(())
    }
}

impl StreamedSourceReceiver for SourceCaptureSession {
    fn capture(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        cancellation: &Cancellation,
    ) -> Result<StreamedDiscoverySource, DiscoveryError> {
        let (source, prefix) =
            SourceCaptureSession::capture(self, root, path, observed, cancellation).map_err(
                |_| {
                    cancellation.check().err().map_or(
                        DiscoveryError::StreamedCaptureUnavailable,
                        DiscoveryError::Cancelled,
                    )
                },
            )?;
        Ok(StreamedDiscoverySource {
            capture: source.capture,
            encoding: source.encoding,
            contains_nul: source.contains_nul,
            prefix,
        })
    }
}

impl CapturedSourceFile {
    pub(super) const fn encoding_label(&self) -> &'static str {
        match self.encoding {
            Some(TextEncoding::Utf8) => "utf-8",
            Some(TextEncoding::Utf16Le) => "utf-16le",
            Some(TextEncoding::Utf16Be) => "utf-16be",
            None => "bytes",
        }
    }
}

pub(super) struct CapturedGenerationSources {
    instance: Arc<SourceInstance>,
    repository: RepositoryId,
    generation: GenerationId,
    snapshot_file_bytes: u64,
    directory: Option<PrivateDirectory<'static>>,
    name: OsString,
    files: BTreeMap<FileId, CapturedSourceFile>,
}

impl fmt::Debug for CapturedGenerationSources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedGenerationSources")
            .field("repository", &self.repository)
            .field("generation", &self.generation)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl CapturedGenerationSources {
    pub(super) const fn snapshot_file_bytes(&self) -> u64 {
        self.snapshot_file_bytes
    }
    pub(super) fn files(&self) -> impl Iterator<Item = (FileId, &CapturedSourceFile)> {
        self.files.iter().map(|(file, source)| (*file, source))
    }

    pub(super) fn visit(
        &self,
        file: FileId,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut VerifiedSourceVisitor<'_>,
    ) -> Result<u64, FirstSliceError> {
        let source = self.files.get(&file).ok_or(FirstSliceError::Query)?;
        visit_verified_source_bytes_cancellable(
            self.directory()?,
            &source.private_name,
            StreamedSourceBytesIdentity {
                byte_length: source.capture.metadata().length,
                content_hash: source.capture.content_hash(),
            },
            maximum,
            cancellation,
            visitor,
        )
        .map_err(|_| {
            check_cancellation(cancellation)
                .err()
                .unwrap_or(FirstSliceError::CatalogCorrupt)
        })
    }

    pub(super) fn source(&self, file: &FileRecord) -> Result<&CapturedSourceFile, FirstSliceError> {
        if file.repository != self.repository || file.generation != self.generation {
            return Err(FirstSliceError::GenerationMismatch);
        }
        let source = self.files.get(&file.id).ok_or(FirstSliceError::Query)?;
        if source.capture.content_hash() != file.content_hash
            || source.capture.metadata().length != file.byte_length
            || source.capture.path().as_str() != file.path
            || source.encoding_label() != file.encoding
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(source)
    }

    pub(super) fn directory(&self) -> Result<&PrivateDirectory<'static>, FirstSliceError> {
        self.directory.as_ref().ok_or(FirstSliceError::Retention)
    }

    pub(super) fn contains(&self, file: FileId) -> bool {
        self.files.contains_key(&file)
    }
}

impl Drop for CapturedGenerationSources {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            let _ = self.instance.cleanup(&self.name, directory);
        }
    }
}

impl Drop for SourceCaptureSession {
    fn drop(&mut self) {
        if let (Some(name), Some(directory)) = (self.name.take(), self.directory.take()) {
            let _ = self.instance.cleanup(&name, directory);
        }
    }
}

struct CaptureSink<'a, W> {
    output: &'a mut W,
    validator: StreamingSourceTextValidator,
    prefix: Vec<u8>,
    cancellation: &'a Cancellation,
}

impl<W: Write> Write for CaptureSink<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        let written = self.output.write(bytes)?;
        let bytes = &bytes[..written];
        self.validator
            .push(bytes, self.cancellation)
            .map_err(io::Error::other)?;
        let prefix_bytes = bytes
            .len()
            .min(CLASSIFICATION_PREFIX_BYTES.saturating_sub(self.prefix.len()));
        self.prefix
            .try_reserve(prefix_bytes)
            .map_err(io::Error::other)?;
        self.prefix.extend_from_slice(&bytes[..prefix_bytes]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_discovery::InputClass;
    use rootlight_ids::{content_hash, derive_repository};

    fn parent() -> (tempfile::TempDir, Dir) {
        let temporary = tempfile::tempdir().unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            temporary.path().join("private"),
            temporary.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let directory =
            Dir::open_ambient_dir(paths.state_dir(), cap_std::ambient_authority()).unwrap();
        (temporary, directory)
    }

    #[test]
    fn capture_rollback_and_generation_leases_keep_private_payload_charges_exact() {
        let (temporary, parent) = parent();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(1024, 2048, 8).unwrap(),
        )
        .unwrap();
        let source = tempfile::tempdir().unwrap();
        let bytes = b"// exact original\n";
        std::fs::write(source.path().join("source.rs"), bytes).unwrap();
        let repository = derive_repository(b"source-owner-capture-lifetime").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let path = RelativePath::parse(std::path::Path::new("source.rs")).unwrap();
        let cancellation = Cancellation::new();
        let observed = root
            .read_directory(None, 10, &cancellation)
            .unwrap()
            .into_iter()
            .find(|entry| entry.name == "source.rs")
            .unwrap()
            .metadata;
        let mut provisional = owner.begin(repository);
        provisional
            .capture(&root, &path, observed, &cancellation)
            .unwrap();
        assert_eq!(
            owner.usage().unwrap().reserved_payload_bytes,
            u64::try_from(bytes.len()).unwrap()
        );
        assert!(!owner.finish_if_idle().unwrap());
        drop(provisional);
        assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 0);
        let mut provisional = owner.begin(repository);
        let (captured, _) = provisional
            .capture(&root, &path, observed, &cancellation)
            .unwrap();
        let input = ManifestInput {
            file: captured.capture.file(),
            path: "source.rs".to_owned(),
            content_hash: content_hash(bytes),
            bytes: u64::try_from(bytes.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: Vec::new(),
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        let generation = provisional
            .into_generation(GenerationId::from_bytes([7; 20]), &[input], 1)
            .unwrap()
            .unwrap();
        let lease = Arc::clone(&generation);
        drop(generation);
        std::fs::write(source.path().join("source.rs"), b"changed live source").unwrap();
        let mut read = Vec::new();
        lease
            .visit(
                captured.capture.file(),
                1024,
                &cancellation,
                &mut |_, bytes| {
                    read.extend_from_slice(bytes);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(read, bytes);
        assert!(!owner.finish_if_idle().unwrap());
        drop(lease);
        assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 0);
        assert!(owner.finish_if_idle().unwrap());
        assert!(!temporary.path().join("private/owned").exists());
    }

    #[test]
    fn instance_unlink_requires_parent_durability_before_cleanup_debt_clears() {
        let (temporary, parent) = parent();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(1024, 1024, 1).unwrap(),
        )
        .unwrap();
        let instance = Arc::get_mut(&mut owner.instance).unwrap();
        assert!(
            instance
                .finish_root(|_| Err(rootlight_vfs::platform::PlatformError::ResourceLimit))
                .is_err()
        );
        assert!(!temporary.path().join("private/owned").exists());
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
        assert!(owner.finish_if_idle().unwrap());
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 0);
    }

    #[test]
    fn cleanup_retry_rejects_a_replacement_instance_identity() {
        let (temporary, parent) = parent();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(1024, 1024, 1).unwrap(),
        )
        .unwrap();
        Arc::get_mut(&mut owner.instance)
            .unwrap()
            .finish_root(|_| Err(rootlight_vfs::platform::PlatformError::ResourceLimit))
            .unwrap_err();
        let replacement = PrivateDirectory::create(&parent, OsStr::new("owned")).unwrap();
        replacement
            .create_file(OsStr::new("keep"))
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        assert!(owner.finish_if_idle().is_err());
        assert_eq!(
            std::fs::read(temporary.path().join("private/owned/keep")).unwrap(),
            b"replacement"
        );
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
    }

    #[test]
    fn child_removal_and_parent_sync_failures_retain_charges_until_confirmed_retry() {
        for fail_unlink in [true, false] {
            let (temporary, parent) = parent();
            let owner = FirstSliceEphemeralSources::create(
                &parent,
                OsStr::new("owned"),
                FirstSliceStreamedStorageLimits::new(1024, 1024, 1).unwrap(),
            )
            .unwrap();
            let mut provisional = owner.begin(derive_repository(b"child-cleanup-debt").id());
            provisional.ensure_directory().unwrap();
            let name = provisional.name.take().unwrap();
            let directory = provisional.directory.take().unwrap();
            let mut file = directory.create_file(OsStr::new("payload")).unwrap();
            file.write_all(b"exact debt").unwrap();
            file.sync_all().unwrap();
            drop(file);
            owner.instance.reserve(&name, 10).unwrap();
            let result = owner.instance.cleanup_with(
                &name,
                directory,
                |directory| {
                    if fail_unlink {
                        drop(directory);
                        false
                    } else {
                        directory.remove().is_ok()
                    }
                },
                |_| Err(rootlight_vfs::platform::PlatformError::ResourceLimit),
            );
            assert!(result.is_err());
            assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 10);
            assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
            assert_eq!(
                temporary.path().join("private/owned").join(&name).exists(),
                fail_unlink
            );
            owner.retry_cleanup().unwrap();
            assert_eq!(owner.usage().unwrap().reserved_payload_bytes, 0);
            assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 0);
            assert!(!temporary.path().join("private/owned").join(&name).exists());
        }
    }
}
