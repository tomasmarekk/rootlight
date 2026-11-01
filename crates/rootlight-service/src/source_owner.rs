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
    DiscoveryError, ManifestInput, NTriplesNulProbe, NTriplesNulProof, NativeNulCapture,
    NativeNulGrammar, NativeNulProbe, NativeNulProof, StreamedDiscoverySource,
    StreamedSourceReceiver,
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
                root_identity_guard: None,
                root_removed: false,
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
    root_identity_guard: Option<std::fs::File>,
    root_removed: bool,
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
        self.finish_root_with(|directory| directory.remove(), synchronize)
    }

    fn finish_root_with(
        &mut self,
        remove: impl FnOnce(
            PrivateDirectory<'static>,
        ) -> Result<(), rootlight_vfs::platform::PlatformError>,
        synchronize: impl FnOnce(&Dir) -> Result<(), rootlight_vfs::platform::PlatformError>,
    ) -> Result<(), FirstSliceError> {
        self.root_cleanup_failed = true;
        if !self.root_removed {
            if let Some(directory) = self.directory.as_ref() {
                if directory.identity() != self.identity {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                if self.root_identity_guard.is_none() {
                    self.root_identity_guard = Some(
                        directory
                            .retain_identity_handle()
                            .map_err(|_| FirstSliceError::Catalog)?,
                    );
                }
            }
            if let Some(directory) = self.directory.take() {
                remove(directory).map_err(|_| FirstSliceError::Catalog)?;
            } else {
                match self.parent.symlink_metadata(&self.name) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        if self.root_identity_guard.is_none() {
                            return Err(FirstSliceError::CatalogCorrupt);
                        }
                        let directory = PrivateDirectory::open(&self.parent, &self.name)
                            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                        if directory.identity() != self.identity {
                            return Err(FirstSliceError::CatalogCorrupt);
                        }
                        remove(directory).map_err(|_| FirstSliceError::Catalog)?;
                    }
                    Err(_) => return Err(FirstSliceError::Catalog),
                }
            }
            // An uncertain unlink keeps the original identity pinned. Once
            // unlink is confirmed, a reused filesystem ID cannot authorize a
            // newly created directory at this name.
            self.root_removed = true;
            self.root_identity_guard = None;
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
    pub(super) ntriples_nul_proof: Option<NTriplesNulProof>,
    native_nul_proof: Option<NativeNulProof>,
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
        self.capture_with_native_nul(root, path, observed, false, cancellation)
    }

    fn capture_with_native_nul(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: bool,
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
        StreamedSourceReceiver::capture_with_native_nul(
            session,
            root,
            path,
            observed,
            requested,
            cancellation,
        )
    }

    fn capture_with_nul_grammar(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: Option<NativeNulGrammar>,
        cancellation: &Cancellation,
    ) -> Result<NativeNulCapture, DiscoveryError> {
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
        session.capture_with_nul_grammar(root, path, observed, requested, cancellation)
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
            ntriples_nul_proof: None,
            native_nul_proof: None,
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
        self.capture_with_native_nul(root, path, observed, false, cancellation)
    }

    fn capture_with_native_nul(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: bool,
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
        let ntriples_nul_proof =
            if requested && source.encoding == Some(TextEncoding::Utf8) && source.contains_nul {
                if let Some(proof) = source.ntriples_nul_proof.as_ref() {
                    if !proof.matches_source(source.capture.content_hash(), observed.length) {
                        return Err(DiscoveryError::IncrementalDrift);
                    }
                    Some(proof.clone())
                } else {
                    let proof = captured_ntriples_nul_proof(
                        self.directory
                            .as_ref()
                            .ok_or(DiscoveryError::StreamedCaptureUnavailable)?,
                        &source.private_name,
                        &source.capture,
                        cancellation,
                    )
                    .map_err(|_| {
                        cancellation.check().err().map_or(
                            DiscoveryError::StreamedCaptureUnavailable,
                            DiscoveryError::Cancelled,
                        )
                    })?;
                    self.files
                        .get_mut(&source.capture.file())
                        .ok_or(DiscoveryError::IncrementalDrift)?
                        .ntriples_nul_proof = Some(proof.clone());
                    Some(proof)
                }
            } else {
                None
            };
        Ok(StreamedDiscoverySource {
            capture: source.capture,
            encoding: source.encoding,
            contains_nul: source.contains_nul,
            ntriples_nul_proof,
            prefix,
        })
    }

    fn capture_with_nul_grammar(
        &mut self,
        root: &RepositoryRoot,
        path: &RelativePath,
        observed: SnapshotMetadata,
        requested: Option<NativeNulGrammar>,
        cancellation: &Cancellation,
    ) -> Result<NativeNulCapture, DiscoveryError> {
        let Some(
            grammar
            @ (NativeNulGrammar::NQuads | NativeNulGrammar::Turtle | NativeNulGrammar::Trig),
        ) = requested
        else {
            let source = self.capture_with_native_nul(
                root,
                path,
                observed,
                requested == Some(NativeNulGrammar::NTriples),
                cancellation,
            )?;
            let proof = source.ntriples_nul_proof.clone().map(Into::into);
            return Ok(NativeNulCapture { source, proof });
        };
        let (source, prefix) =
            SourceCaptureSession::capture(self, root, path, observed, cancellation).map_err(
                |_| {
                    cancellation.check().err().map_or(
                        DiscoveryError::StreamedCaptureUnavailable,
                        DiscoveryError::Cancelled,
                    )
                },
            )?;
        let proof = if source.encoding == Some(TextEncoding::Utf8) && source.contains_nul {
            if source.native_nul_proof.as_ref().is_some_and(|proof| {
                !proof.matches_source(source.capture.content_hash(), observed.length)
            }) {
                return Err(DiscoveryError::IncrementalDrift);
            }
            if let Some(proof) = source
                .native_nul_proof
                .as_ref()
                .filter(|proof| proof.grammar() == grammar)
            {
                Some(proof.clone())
            } else {
                let proof = captured_native_nul_proof(
                    self.directory
                        .as_ref()
                        .ok_or(DiscoveryError::StreamedCaptureUnavailable)?,
                    &source.private_name,
                    &source.capture,
                    grammar,
                    cancellation,
                )
                .map_err(|_| {
                    cancellation.check().err().map_or(
                        DiscoveryError::StreamedCaptureUnavailable,
                        DiscoveryError::Cancelled,
                    )
                })?;
                self.files
                    .get_mut(&source.capture.file())
                    .ok_or(DiscoveryError::IncrementalDrift)?
                    .native_nul_proof = Some(proof.clone());
                Some(proof)
            }
        } else {
            None
        };
        Ok(NativeNulCapture {
            source: StreamedDiscoverySource {
                capture: source.capture,
                encoding: source.encoding,
                contains_nul: source.contains_nul,
                ntriples_nul_proof: None,
                prefix,
            },
            proof,
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

fn captured_ntriples_nul_proof(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    captured: &StreamedSourceCapture,
    cancellation: &Cancellation,
) -> Result<NTriplesNulProof, FirstSliceError> {
    // Only NUL-bearing UTF-8 captures need this extra pass over private bytes.
    let mut probe = NTriplesNulProbe::new();
    visit_captured_nul_bytes(directory, name, captured, cancellation, |bytes| {
        probe.push(bytes, cancellation)
    })?;
    let proof = probe
        .finish(cancellation)
        .map_err(|error| super::map_discovery_error(error, cancellation))?;
    if !proof.matches_source(captured.content_hash(), captured.metadata().length) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(proof)
}

fn captured_native_nul_proof(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    captured: &StreamedSourceCapture,
    grammar: NativeNulGrammar,
    cancellation: &Cancellation,
) -> Result<NativeNulProof, FirstSliceError> {
    let mut probe = NativeNulProbe::new(grammar);
    visit_captured_nul_bytes(directory, name, captured, cancellation, |bytes| {
        probe.push(bytes, cancellation)
    })?;
    let proof = probe
        .finish(cancellation)
        .map_err(|error| super::map_discovery_error(error, cancellation))?;
    if !proof.matches_request(grammar, captured.content_hash(), captured.metadata().length) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(proof)
}

fn visit_captured_nul_bytes(
    directory: &PrivateDirectory<'_>,
    name: &OsStr,
    captured: &StreamedSourceCapture,
    cancellation: &Cancellation,
    mut consume: impl FnMut(&[u8]) -> Result<(), DiscoveryError>,
) -> Result<(), FirstSliceError> {
    let mut probe_error = None;
    let read = visit_verified_source_bytes_cancellable(
        directory,
        name,
        StreamedSourceBytesIdentity {
            byte_length: captured.metadata().length,
            content_hash: captured.content_hash(),
        },
        captured.metadata().length,
        cancellation,
        |_, bytes| {
            consume(bytes).map_err(|error| {
                probe_error = Some(error);
                rootlight_vfs::platform::PlatformError::ResourceLimit
            })
        },
    );
    if let Some(error) = probe_error {
        return Err(super::map_discovery_error(error, cancellation));
    }
    read.map_err(|_| {
        check_cancellation(cancellation)
            .err()
            .unwrap_or(FirstSliceError::CatalogCorrupt)
    })?;
    Ok(())
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
    fn native_nul_proof_covers_private_bytes_beyond_prefix_and_survives_cache() {
        let (_temporary, parent) = parent();
        let source = tempfile::tempdir().unwrap();
        let mut bytes = b"# padding\n".repeat(CLASSIFICATION_PREFIX_BYTES / 10 + 8);
        bytes.extend_from_slice(b"<s:><p:>\"a\0b\".\n");
        std::fs::write(source.path().join("source.nt"), &bytes).unwrap();
        std::fs::write(source.path().join("plain.nt"), b"<s:><p:>\"plain\".\n").unwrap();
        let maximum = u64::try_from(bytes.len()).unwrap() + 64;
        let owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(maximum, maximum * 2, 2).unwrap(),
        )
        .unwrap();
        let repository = derive_repository(b"source-owner-native-nul-proof").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let cancellation = Cancellation::new();
        let observed = root.read_directory(None, 10, &cancellation).unwrap();
        let path = RelativePath::parse(std::path::Path::new("source.nt")).unwrap();
        let metadata = observed
            .iter()
            .find(|entry| entry.name == "source.nt")
            .unwrap()
            .metadata;
        let mut session = owner.begin(repository);
        let generic =
            StreamedSourceReceiver::capture(&mut session, &root, &path, metadata, &cancellation)
                .unwrap();
        assert!(generic.ntriples_nul_proof.is_none());
        for _ in 0..2 {
            let captured = StreamedSourceReceiver::capture_with_native_nul(
                &mut session,
                &root,
                &path,
                metadata,
                true,
                &cancellation,
            )
            .unwrap();
            assert_eq!(captured.encoding, Some(TextEncoding::Utf8));
            assert!(captured.contains_nul);
            assert_eq!(captured.prefix.len(), CLASSIFICATION_PREFIX_BYTES);
            assert!(!captured.prefix.contains(&0));
            let proof = captured.ntriples_nul_proof.as_ref().unwrap();
            assert!(proof.permits_nul());
            assert!(!proof.was_refused());
            assert!(
                proof.matches_source(content_hash(&bytes), u64::try_from(bytes.len()).unwrap())
            );
            assert!(!proof.matches_source(
                content_hash(b"other bytes"),
                u64::try_from(bytes.len()).unwrap()
            ));
            assert!(!proof.matches_source(
                content_hash(&bytes),
                u64::try_from(bytes.len()).unwrap() + 1
            ));
        }
        let generic =
            StreamedSourceReceiver::capture(&mut session, &root, &path, metadata, &cancellation)
                .unwrap();
        assert!(generic.ntriples_nul_proof.is_none());
        let plain_path = RelativePath::parse(std::path::Path::new("plain.nt")).unwrap();
        let plain = StreamedSourceReceiver::capture(
            &mut session,
            &root,
            &plain_path,
            observed
                .iter()
                .find(|entry| entry.name == "plain.nt")
                .unwrap()
                .metadata,
            &cancellation,
        )
        .unwrap();
        assert!(!plain.contains_nul);
        assert!(plain.ntriples_nul_proof.is_none());
    }

    #[test]
    fn contextual_nul_proofs_cover_private_tails_and_cache_the_selected_grammar() {
        let (_temporary, parent) = parent();
        let source = tempfile::tempdir().unwrap();
        let mut bytes = b"# padding\n".repeat(CLASSIFICATION_PREFIX_BYTES / 10 + 8);
        bytes.extend_from_slice(b"<s> <p> '''first\n#\0last''' .\n<tail> <p> <o> .\n");
        std::fs::write(source.path().join("source.ttl"), &bytes).unwrap();
        let maximum = u64::try_from(bytes.len()).unwrap();
        let owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(maximum, maximum, 1).unwrap(),
        )
        .unwrap();
        let repository = derive_repository(b"source-owner-contextual-nul").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let cancellation = Cancellation::new();
        let metadata = root.read_directory(None, 10, &cancellation).unwrap()[0].metadata;
        let path = RelativePath::parse(std::path::Path::new("source.ttl")).unwrap();
        let mut session = owner.begin(repository);
        for grammar in [
            NativeNulGrammar::Turtle,
            NativeNulGrammar::Turtle,
            NativeNulGrammar::Trig,
            NativeNulGrammar::Trig,
            NativeNulGrammar::Turtle,
        ] {
            let captured = session
                .capture_with_nul_grammar(&root, &path, metadata, Some(grammar), &cancellation)
                .unwrap();
            assert_eq!(captured.source.encoding, Some(TextEncoding::Utf8));
            assert!(captured.source.contains_nul);
            assert!(!captured.source.prefix.contains(&0));
            assert!(captured.source.ntriples_nul_proof.is_none());
            let proof = captured.proof.unwrap();
            assert!(proof.permits_nul());
            assert!(proof.matches_request(grammar, content_hash(&bytes), maximum));
            assert_eq!(
                session.files[&captured.source.capture.file()]
                    .native_nul_proof
                    .as_ref()
                    .unwrap()
                    .grammar(),
                grammar
            );
        }
        let generic = session
            .capture_with_nul_grammar(&root, &path, metadata, None, &cancellation)
            .unwrap();
        assert!(generic.proof.is_none());
        assert!(generic.source.ntriples_nul_proof.is_none());
        let mut receiver = SourceCaptureReceiver::Explicit(session);
        let captured = receiver
            .capture_with_nul_grammar(
                &root,
                &path,
                metadata,
                Some(NativeNulGrammar::Turtle),
                &cancellation,
            )
            .unwrap();
        assert!(captured.proof.unwrap().permits_nul());
    }

    #[test]
    fn nquads_native_nul_cache_recomputes_when_selected_grammar_changes() {
        let (_temporary, parent) = parent();
        let source = tempfile::tempdir().unwrap();
        let mut bytes = b"# padding\n".repeat(CLASSIFICATION_PREFIX_BYTES / 10 + 8);
        bytes.extend_from_slice(b"_:s <urn:p> \"value\0tail\" <urn:g> .\n");
        std::fs::write(source.path().join("source.nq"), &bytes).unwrap();
        let maximum = u64::try_from(bytes.len()).unwrap();
        let owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(maximum, maximum, 1).unwrap(),
        )
        .unwrap();
        let repository = derive_repository(b"source-owner-nquads-nul").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let cancellation = Cancellation::new();
        let metadata = root.read_directory(None, 10, &cancellation).unwrap()[0].metadata;
        let path = RelativePath::parse(std::path::Path::new("source.nq")).unwrap();
        let mut session = owner.begin(repository);
        for (grammar, permits) in [
            (NativeNulGrammar::NQuads, true),
            (NativeNulGrammar::NQuads, true),
            (NativeNulGrammar::Turtle, false),
            (NativeNulGrammar::NQuads, true),
            (NativeNulGrammar::NTriples, false),
            (NativeNulGrammar::NQuads, true),
        ] {
            let captured = session
                .capture_with_nul_grammar(&root, &path, metadata, Some(grammar), &cancellation)
                .unwrap();
            assert!(captured.source.contains_nul);
            assert!(!captured.source.prefix.contains(&0));
            let proof = captured.proof.unwrap();
            assert_eq!(proof.permits_nul(), permits, "{grammar:?}");
            assert!(proof.matches_request(grammar, content_hash(&bytes), maximum));
            let other = if grammar == NativeNulGrammar::NQuads {
                NativeNulGrammar::NTriples
            } else {
                NativeNulGrammar::NQuads
            };
            assert!(!proof.matches_request(other, content_hash(&bytes), maximum));
        }
    }

    #[test]
    fn contextual_nul_cached_proofs_cannot_authenticate_other_private_bytes() {
        let (_temporary, parent) = parent();
        let source = tempfile::tempdir().unwrap();
        let bytes = b"GRAPH <g> { <s> <p> '\0' . }\n<tail> <p> <o> .\n";
        std::fs::write(source.path().join("source.trig"), bytes).unwrap();
        let maximum = u64::try_from(bytes.len()).unwrap();
        let owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(maximum, maximum, 1).unwrap(),
        )
        .unwrap();
        let repository = derive_repository(b"source-owner-contextual-stale").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let cancellation = Cancellation::new();
        let metadata = root.read_directory(None, 10, &cancellation).unwrap()[0].metadata;
        let path = RelativePath::parse(std::path::Path::new("source.trig")).unwrap();
        let mut session = owner.begin(repository);
        let captured = session
            .capture_with_nul_grammar(
                &root,
                &path,
                metadata,
                Some(NativeNulGrammar::Trig),
                &cancellation,
            )
            .unwrap();
        assert!(captured.proof.unwrap().permits_nul());
        let mut stale = NativeNulProbe::new(NativeNulGrammar::Trig);
        stale.push(b"<other> <p> '\0' .", &cancellation).unwrap();
        session
            .files
            .get_mut(&captured.source.capture.file())
            .unwrap()
            .native_nul_proof = Some(stale.finish(&cancellation).unwrap());
        assert!(matches!(
            session.capture_with_nul_grammar(
                &root,
                &path,
                metadata,
                Some(NativeNulGrammar::Trig),
                &cancellation
            ),
            Err(DiscoveryError::IncrementalDrift)
        ));
    }

    #[test]
    fn non_native_capture_never_mints_a_nul_proof() {
        let (_temporary, parent) = parent();
        let source = tempfile::tempdir().unwrap();
        let mut bytes = vec![b'a'; CLASSIFICATION_PREFIX_BYTES * 2];
        bytes.push(0);
        std::fs::write(source.path().join("payload.data"), &bytes).unwrap();
        let maximum = u64::try_from(bytes.len()).unwrap();
        let owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(maximum, maximum, 1).unwrap(),
        )
        .unwrap();
        let repository = derive_repository(b"source-owner-non-native-nul").id();
        let root = RepositoryRoot::open(repository, source.path()).unwrap();
        let cancellation = Cancellation::new();
        let observed = root.read_directory(None, 10, &cancellation).unwrap()[0].metadata;
        let path = RelativePath::parse(std::path::Path::new("payload.data")).unwrap();
        let mut session = owner.begin(repository);
        let captured = StreamedSourceReceiver::capture_with_native_nul(
            &mut session,
            &root,
            &path,
            observed,
            false,
            &cancellation,
        )
        .unwrap();
        assert_eq!(captured.encoding, Some(TextEncoding::Utf8));
        assert!(captured.contains_nul);
        assert!(!captured.prefix.contains(&0));
        assert!(captured.ntriples_nul_proof.is_none());
        assert!(
            session
                .files
                .values()
                .all(|source| source.ntriples_nul_proof.is_none())
        );
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
    fn confirmed_root_unlink_rejects_replacement_even_with_reused_identity() {
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
        let replacement_identity = replacement.identity();
        replacement
            .create_file(OsStr::new("keep"))
            .unwrap()
            .write_all(b"replacement")
            .unwrap();
        drop(replacement);
        // Reproduce filesystem-ID reuse without relying on allocator timing.
        Arc::get_mut(&mut owner.instance).unwrap().identity = replacement_identity;
        assert!(owner.finish_if_idle().is_err());
        assert_eq!(
            std::fs::read(temporary.path().join("private/owned/keep")).unwrap(),
            b"replacement"
        );
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
    }

    #[test]
    fn uncertain_root_removal_before_unlink_retries_with_original_identity_pinned() {
        let (temporary, parent) = parent();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(1024, 1024, 1).unwrap(),
        )
        .unwrap();
        let instance = Arc::get_mut(&mut owner.instance).unwrap();
        instance
            .finish_root_with(
                |directory| {
                    drop(directory);
                    Err(rootlight_vfs::platform::PlatformError::ResourceLimit)
                },
                |_| panic!("failed unlink must not report parent durability"),
            )
            .unwrap_err();
        assert!(instance.root_identity_guard.is_some());
        assert!(!instance.root_removed);
        assert!(temporary.path().join("private/owned").exists());
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
        assert!(owner.finish_if_idle().unwrap());
        assert!(!temporary.path().join("private/owned").exists());
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 0);
        let instance = Arc::get_mut(&mut owner.instance).unwrap();
        assert!(instance.root_removed);
        assert!(instance.root_identity_guard.is_none());
    }

    #[test]
    fn uncertain_root_removal_after_unlink_retains_identity_until_absence_is_confirmed() {
        let (temporary, parent) = parent();
        let mut owner = FirstSliceEphemeralSources::create(
            &parent,
            OsStr::new("owned"),
            FirstSliceStreamedStorageLimits::new(1024, 1024, 1).unwrap(),
        )
        .unwrap();
        let instance = Arc::get_mut(&mut owner.instance).unwrap();
        instance
            .finish_root_with(
                |directory| {
                    directory.remove()?;
                    Err(rootlight_vfs::platform::PlatformError::ResourceLimit)
                },
                |_| panic!("uncertain unlink must not report parent durability"),
            )
            .unwrap_err();
        assert!(instance.root_identity_guard.is_some());
        assert!(!instance.root_removed);
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 1);
        assert!(owner.finish_if_idle().unwrap());
        assert!(!temporary.path().join("private/owned").exists());
        assert_eq!(owner.usage().unwrap().pending_cleanup_directories, 0);
        let instance = Arc::get_mut(&mut owner.instance).unwrap();
        assert!(instance.root_removed);
        assert!(instance.root_identity_guard.is_none());
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
