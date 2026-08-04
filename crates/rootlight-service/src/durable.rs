//! Crash-safe generation publication and restoration for the first-slice service.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use cap_std::{ambient_authority, fs::Dir};
use rootlight_cancel::Cancellation;
use rootlight_catalog::OracleReader;
use rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_ir::{ExtensionSupport, FileRecord, IrLimits};
use rootlight_search::{BuildBudget, LexicalIndex};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationContractVersion,
    GenerationMetadata, GenerationSnapshot, IdentityVerificationError, IdentityVerifiedGeneration,
};
use rootlight_vfs::{
    MAX_SNAPSHOT_BYTES, RelativePath, SourceSnapshot,
    platform::{PlatformError, PrivateDirectory, PublishError, PublishedPrivateDirectory},
};
use serde::{Deserialize, Serialize};

use super::{
    FirstSliceError, FirstSliceIndexReceipt, FirstSliceOperationContext, RustSourceInput,
    check_cancellation, map_catalog_error, map_identity_error, map_query_error, map_search_error,
    map_vfs_error, project_lexical_documents,
};

const DURABLE_DIRECTORY: &str = "first-slice";
const REPOSITORIES_DIRECTORY: &str = "repositories";
const QUARANTINE_DIRECTORY: &str = "quarantine";
const SOURCES_DIRECTORY: &str = "sources";
const MANIFEST_FILENAME: &str = "manifest.json";
const RECOVERY_SNAPSHOT_FILENAME: &str = "recovery.json";
const RECOVERY_MANIFEST_FILENAME: &str = "recovery-manifest.json";
const ACTIVATION_MANIFEST_FILENAME: &str = "activation.json";
const REPOSITORY_METADATA_FILENAME: &str = "metadata.json";
const GENERATION_MANIFEST_VERSION: u16 = 1;
pub(super) const REPOSITORY_METADATA_VERSION: u16 = 1;
const RECOVERY_SNAPSHOT_VERSION: u16 = 1;
const LEGACY_ACTIVATION_MANIFEST_VERSION: u16 = 1;
const ACTIVATION_MANIFEST_VERSION: u16 = 2;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ACTIVATION_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_RECOVERY_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_RECOVERY_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RECOVERY_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_DURABLE_ENTRIES: usize = 65_536;
const MAX_RESTORED_OPERATIONS: usize = 256;
const MAX_QUARANTINED_GENERATIONS: usize = 256;
const STAGING_PREFIX: &str = "stage-";
const ACTIVATION_PREFIX: &str = "activation-";
const METADATA_PREFIX: &str = "metadata-";
const QUARANTINE_PREFIX: &str = "generation-";

pub(super) struct DurableCatalog {
    repositories: PrivateDirectory<'static>,
    quarantine: PrivateDirectory<'static>,
    repositories_path: PathBuf,
    maximum_generations_per_repository: usize,
    staging_bytes: Arc<AtomicU64>,
}

pub(super) struct DurablePreparedGeneration {
    staging: Option<PrivateDirectory<'static>>,
    staging_path: PathBuf,
    repository: Option<PrivateDirectory<'static>>,
    generation: GenerationId,
    staging_bytes: Arc<AtomicU64>,
    accounted_bytes: AtomicU64,
}

pub(super) struct DurablePublishedGeneration {
    directory: Option<PublishedPrivateDirectory>,
    repository: PrivateDirectory<'static>,
    generation: GenerationId,
}

pub(super) struct RestoredGeneration {
    pub(super) root_identity: ContentHash,
    pub(super) display_name: String,
    pub(super) root_path: Option<String>,
    pub(super) alias: Option<String>,
    pub(super) metadata_sequence: u64,
    pub(super) receipt: FirstSliceIndexReceipt,
    pub(super) activation_sequence: u64,
    pub(super) global_activation_sequence: Option<u64>,
    pub(super) published_generation_count: Option<u64>,
    pub(super) verified: IdentityVerifiedGeneration,
    pub(super) search: LexicalIndex,
    pub(super) sources: Vec<RustSourceInput>,
    pub(super) operations: Vec<FirstSliceOperationContext>,
}

struct RestorePolicy<'a> {
    maximum_generations: usize,
    excluded: &'a BTreeSet<GenerationId>,
    compact: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableGenerationManifest {
    version: u16,
    root_identity: ContentHash,
    display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_path: Option<String>,
    receipt: FirstSliceIndexReceipt,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DurableRepositoryMetadata {
    pub(super) version: u16,
    pub(super) sequence: u64,
    pub(super) repository: RepositoryId,
    pub(super) root_path: Option<String>,
    pub(super) alias: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableRecoverySnapshot {
    version: u16,
    bytes: u64,
    digest: ContentHash,
    contract_major: u16,
    contract_minor: u16,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableActivationManifest {
    version: u16,
    generation: GenerationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    global_activation_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    published_generation_count: Option<u64>,
    operation: Option<DurableOperationContextV2>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableOperationContextV2 {
    operation: rootlight_ids::OperationId,
    started_unix_ms: u64,
}

impl From<FirstSliceOperationContext> for DurableOperationContextV2 {
    fn from(context: FirstSliceOperationContext) -> Self {
        Self {
            operation: context.operation,
            started_unix_ms: context.started_unix_ms,
        }
    }
}

impl From<DurableOperationContextV2> for FirstSliceOperationContext {
    fn from(context: DurableOperationContextV2) -> Self {
        Self {
            operation: context.operation,
            started_unix_ms: context.started_unix_ms,
            provider: super::FirstSliceIndexProvider::Unknown,
        }
    }
}

struct ActivationMarker {
    name: OsString,
    sequence: u64,
    manifest: DurableActivationManifest,
}

struct GenerationRestoreRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    activation_sequence: u64,
    global_activation_sequence: Option<u64>,
    published_generation_count: Option<u64>,
    repository_directory: &'a PrivateDirectory<'a>,
    repository_path: &'a Path,
}

struct RecoverySnapshotWriter<W> {
    inner: W,
    hasher: blake3::Hasher,
    bytes: u64,
}

impl<W> RecoverySnapshotWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            bytes: 0,
        }
    }
}

fn buffered_recovery_writer<W: std::io::Write>(inner: W) -> RecoverySnapshotWriter<BufWriter<W>> {
    RecoverySnapshotWriter::new(BufWriter::with_capacity(RECOVERY_WRITE_BUFFER_BYTES, inner))
}

impl<W: std::io::Write> std::io::Write for RecoverySnapshotWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        let written_bytes =
            u64::try_from(written).map_err(|_| std::io::Error::other("size overflow"))?;
        self.bytes = self
            .bytes
            .checked_add(written_bytes)
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl DurableCatalog {
    pub(super) fn open(
        state_root: &Path,
        maximum_generations_per_repository: usize,
    ) -> Result<Self, FirstSliceError> {
        if maximum_generations_per_repository == 0 {
            return Err(FirstSliceError::Retention);
        }
        PrivateDirectory::require_supported().map_err(|_| FirstSliceError::Catalog)?;
        let root = Dir::open_ambient_dir(state_root, ambient_authority())
            .map_err(|_| FirstSliceError::Catalog)?;
        PrivateDirectory::verify_parent(&root).map_err(|_| FirstSliceError::Catalog)?;
        let durable = ensure_private_directory(&root, OsStr::new(DURABLE_DIRECTORY))?;
        let durable_path = state_root.join(DURABLE_DIRECTORY);
        let repositories =
            ensure_private_directory(durable.capability(), OsStr::new(REPOSITORIES_DIRECTORY))?;
        let quarantine =
            ensure_private_directory(durable.capability(), OsStr::new(QUARANTINE_DIRECTORY))?;
        let repositories_path = durable_path.join(REPOSITORIES_DIRECTORY);
        Ok(Self {
            repositories,
            quarantine,
            repositories_path,
            maximum_generations_per_repository,
            staging_bytes: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(super) fn begin_generation(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<DurablePreparedGeneration, FirstSliceError> {
        let repository_name = repository.to_string();
        let repository =
            ensure_private_directory(self.repositories.capability(), OsStr::new(&repository_name))?;
        let repository_path = self.repositories_path.join(&repository_name);
        let staging_name = random_staging_name(generation)?;
        let staging = PrivateDirectory::create(repository.capability(), OsStr::new(&staging_name))
            .map_err(|_| FirstSliceError::Catalog)?;
        let staging_path = repository_path.join(&staging_name);
        Ok(DurablePreparedGeneration {
            staging: Some(staging),
            staging_path,
            repository: Some(repository),
            generation,
            staging_bytes: Arc::clone(&self.staging_bytes),
            accounted_bytes: AtomicU64::new(0),
        })
    }

    pub(super) fn ensure_staging_capacity(
        &self,
        required_bytes: u64,
    ) -> Result<(), FirstSliceError> {
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        if available_bytes < required_bytes {
            return Err(FirstSliceError::InsufficientDiskSpace {
                required_bytes,
                available_bytes,
            });
        }
        Ok(())
    }

    pub(super) fn storage_health_snapshot(&self) -> Result<(u64, u64), FirstSliceError> {
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        Ok((self.staging_bytes.load(Ordering::Acquire), available_bytes))
    }

    pub(super) fn read_source(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        file: &FileRecord,
        cancellation: &Cancellation,
    ) -> Result<SourceSnapshot, FirstSliceError> {
        if file.repository != repository || file.generation != generation {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let repository = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let generation =
            PrivateDirectory::open(repository.capability(), OsStr::new(&generation.to_string()))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        read_persisted_source(&generation, file.repository, file, cancellation)
    }

    pub(super) fn restore(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        self.restore_with_policy(
            self.maximum_generations_per_repository,
            &BTreeSet::new(),
            true,
            cancellation,
        )
    }

    pub(super) fn restore_active(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        self.restore_with_policy(1, &BTreeSet::new(), false, cancellation)
    }

    pub(super) fn has_active_restore_work(&self) -> Result<bool, FirstSliceError> {
        let repository_names = private_entry_names(&self.repositories)?;
        if repository_names.len() > super::MAX_FIRST_SLICE_REPOSITORIES {
            return Err(FirstSliceError::Retention);
        }
        for repository_name in repository_names {
            let repository_text = repository_name
                .to_str()
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            RepositoryId::from_str(repository_text).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let repository =
                PrivateDirectory::open(self.repositories.capability(), &repository_name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            for entry_name in private_entry_names(&repository)? {
                let entry_text = entry_name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
                if parse_activation_name(entry_text).is_some() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn restore_excluding(
        &self,
        excluded: &BTreeSet<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        self.restore_with_policy(
            self.maximum_generations_per_repository,
            excluded,
            true,
            cancellation,
        )
    }

    fn restore_with_policy(
        &self,
        maximum_generations_per_repository: usize,
        excluded: &BTreeSet<GenerationId>,
        compact: bool,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        let policy = RestorePolicy {
            maximum_generations: maximum_generations_per_repository,
            excluded,
            compact,
        };
        check_cancellation(cancellation)?;
        let repository_names = private_entry_names(&self.repositories)?;
        if repository_names.len() > super::MAX_FIRST_SLICE_REPOSITORIES {
            return Err(FirstSliceError::Retention);
        }
        let mut restored = Vec::new();
        for repository_name in repository_names {
            check_cancellation(cancellation)?;
            let repository_text = repository_name
                .to_str()
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            let repository_id = RepositoryId::from_str(repository_text)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let repository =
                PrivateDirectory::open(self.repositories.capability(), &repository_name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let repository_path = self.repositories_path.join(&repository_name);
            let mut repository_generations = self.restore_repository(
                repository_id,
                &repository,
                &repository_path,
                &policy,
                cancellation,
            )?;
            restored
                .try_reserve(repository_generations.len())
                .map_err(|_| FirstSliceError::Retention)?;
            restored.append(&mut repository_generations);
        }
        let mut operation_order: Vec<_> = restored
            .iter()
            .flat_map(|generation| generation.operations.iter().copied())
            .collect();
        operation_order
            .sort_unstable_by_key(|operation| (operation.started_unix_ms, operation.operation));
        let keep_from = operation_order
            .len()
            .saturating_sub(MAX_RESTORED_OPERATIONS);
        let retained_operations: BTreeSet<_> = operation_order[keep_from..]
            .iter()
            .map(|operation| operation.operation)
            .collect();
        for generation in &mut restored {
            generation
                .operations
                .retain(|operation| retained_operations.contains(&operation.operation));
        }
        check_cancellation(cancellation)?;
        Ok(restored)
    }

    pub(super) fn activate_existing(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
        repository_activation_sequence: u64,
        global_activation_sequence: u64,
        published_generation_count: u64,
        operation: Option<FirstSliceOperationContext>,
    ) -> Result<u64, FirstSliceError> {
        let repository_name = repository.to_string();
        let repository =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let generation_name = generation.to_string();
        let _generation =
            PrivateDirectory::open(repository.capability(), OsStr::new(&generation_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        publish_activation_marker(
            &repository,
            generation,
            repository_activation_sequence,
            global_activation_sequence,
            published_generation_count,
            operation,
        )
    }

    pub(super) fn write_repository_metadata(
        &self,
        metadata: DurableRepositoryMetadata,
    ) -> Result<u64, FirstSliceError> {
        if metadata.version != REPOSITORY_METADATA_VERSION
            || metadata.sequence == 0
            || !valid_repository_metadata(&metadata)
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let repository = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&metadata.repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let staging_name = random_metadata_staging_name(metadata.sequence)?;
        let staging = PrivateDirectory::create(repository.capability(), OsStr::new(&staging_name))
            .map_err(|_| FirstSliceError::Catalog)?;
        let bytes = serde_json::to_vec(&metadata).map_err(|_| FirstSliceError::Catalog)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
            return Err(FirstSliceError::Limits);
        }
        {
            let mut file = staging
                .create_file(OsStr::new(REPOSITORY_METADATA_FILENAME))
                .map_err(|_| FirstSliceError::Catalog)?;
            file.write_all(&bytes)
                .map_err(|_| FirstSliceError::Catalog)?;
            file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        }
        staging.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        let name = metadata_name(metadata.sequence);
        match staging.publish_noreplace(repository.capability(), OsStr::new(&name)) {
            Ok(published) => published.sync_all().map_err(|_| FirstSliceError::Catalog)?,
            Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
                directory.remove().map_err(|_| FirstSliceError::Catalog)?;
                return Err(FirstSliceError::Catalog);
            }
            Err(_) => return Err(FirstSliceError::Catalog),
        }
        compact_repository_metadata(&repository, metadata.sequence)?;
        u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)
    }

    pub(super) fn remove_repository(
        &self,
        repository: RepositoryId,
    ) -> Result<(), FirstSliceError> {
        let directory = PrivateDirectory::open(
            self.repositories.capability(),
            OsStr::new(&repository.to_string()),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        directory.remove().map_err(|_| FirstSliceError::Catalog)?;
        self.repositories
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)
    }

    fn restore_repository(
        &self,
        repository_id: RepositoryId,
        repository: &PrivateDirectory<'_>,
        repository_path: &Path,
        policy: &RestorePolicy<'_>,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        let names = private_entry_names(repository)?;
        let mut markers = BTreeMap::<u64, ActivationMarker>::new();
        let mut metadata_names = BTreeMap::<u64, OsString>::new();
        let mut generation_names = BTreeSet::new();
        let mut staging_names = Vec::new();
        for name in names {
            check_cancellation(cancellation)?;
            let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            if text.starts_with(STAGING_PREFIX) {
                staging_names.push(name);
            } else if let Some((sequence, generation)) = parse_activation_name(text) {
                let marker = read_activation_marker(repository, name, sequence, generation)?;
                if markers.insert(sequence, marker).is_some() {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
            } else if let Some(sequence) = parse_metadata_name(text) {
                if metadata_names.insert(sequence, name).is_some() {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
            } else if let Ok(generation) = GenerationId::from_str(text) {
                generation_names.insert(generation);
            } else {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }

        for staging_name in staging_names {
            PrivateDirectory::open(repository.capability(), &staging_name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }

        if markers.is_empty() {
            remove_generation_directories(repository, &generation_names)?;
            remove_repository_metadata_directories(repository, metadata_names.values())?;
            return Ok(Vec::new());
        }

        let metadata = metadata_names
            .last_key_value()
            .map(|(sequence, name)| {
                read_repository_metadata(repository_id, repository, name, *sequence)
            })
            .transpose()?;

        for marker in markers.values() {
            if !generation_names.contains(&marker.manifest.generation) {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let mut latest_by_generation = BTreeMap::<GenerationId, u64>::new();
        for marker in markers.values() {
            latest_by_generation
                .entry(marker.manifest.generation)
                .and_modify(|sequence| *sequence = (*sequence).max(marker.sequence))
                .or_insert(marker.sequence);
        }
        let published_generation_count = markers
            .values()
            .filter_map(|marker| marker.manifest.published_generation_count)
            .max();
        let mut recency: Vec<_> = latest_by_generation
            .iter()
            .map(|(generation, sequence)| (*sequence, *generation))
            .collect();
        recency.sort_unstable_by(|left, right| right.cmp(left));
        let excluded_retained = policy
            .excluded
            .iter()
            .filter(|generation| generation_names.contains(generation))
            .copied()
            .collect::<BTreeSet<_>>();
        let maximum_restored = policy
            .maximum_generations
            .saturating_sub(excluded_retained.len());

        let mut restored = Vec::new();
        restored
            .try_reserve_exact(self.maximum_generations_per_repository)
            .map_err(|_| FirstSliceError::Retention)?;
        let mut corrupted = Vec::new();
        for (activation_sequence, generation) in recency {
            if restored.len() == maximum_restored {
                break;
            }
            if policy.excluded.contains(&generation) {
                continue;
            }
            check_cancellation(cancellation)?;
            let latest_marker = markers
                .get(&activation_sequence)
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            let restored_generation = restore_generation(
                GenerationRestoreRequest {
                    repository: repository_id,
                    generation,
                    activation_sequence,
                    global_activation_sequence: latest_marker.manifest.global_activation_sequence,
                    published_generation_count: latest_marker.manifest.published_generation_count,
                    repository_directory: repository,
                    repository_path,
                },
                cancellation,
            );
            let restored_generation = match restored_generation {
                Ok(restored) => restored,
                Err(FirstSliceError::CatalogCorrupt) => {
                    corrupted.push((activation_sequence, generation));
                    continue;
                }
                Err(error) => return Err(error),
            };
            restored.push(restored_generation);
        }
        for (activation_sequence, generation) in corrupted {
            self.quarantine_generation(
                repository_id,
                repository,
                generation,
                activation_sequence,
                &markers,
            )?;
            generation_names.remove(&generation);
            latest_by_generation.remove(&generation);
            markers.retain(|_, marker| marker.manifest.generation != generation);
        }
        let mut retained: BTreeSet<_> = restored
            .iter()
            .map(|generation| generation.receipt.generation)
            .collect();
        retained.extend(excluded_retained);
        let retained_marker_names = retained_activation_marker_names(&markers, &retained);
        for restored_generation in &mut restored {
            if let Some(metadata) = &metadata {
                restored_generation.root_path = metadata.root_path.clone();
                restored_generation.alias = metadata.alias.clone();
                restored_generation.metadata_sequence = metadata.sequence;
            }
            let generation = restored_generation.receipt.generation;
            restored_generation.operations = markers
                .values()
                .filter_map(|marker| {
                    (marker.manifest.generation == generation
                        && retained_marker_names.contains(&marker.name))
                    .then_some(marker.manifest.operation)
                    .flatten()
                    .map(FirstSliceOperationContext::from)
                })
                .collect();
        }
        if policy.compact {
            compact_repository_entries(
                repository,
                &markers,
                &generation_names,
                &retained,
                &retained_marker_names,
            )?;
            if let Some(metadata) = &metadata {
                compact_repository_metadata(repository, metadata.sequence)?;
            }
        }
        if let Some(published_generation_count) = published_generation_count
            && let Some(latest_valid) = restored
                .iter_mut()
                .max_by_key(|generation| generation.activation_sequence)
        {
            latest_valid.published_generation_count = Some(
                latest_valid
                    .published_generation_count
                    .unwrap_or(0)
                    .max(published_generation_count),
            );
        }
        Ok(restored)
    }

    fn quarantine_generation(
        &self,
        repository_id: RepositoryId,
        repository: &PrivateDirectory<'_>,
        generation: GenerationId,
        activation_sequence: u64,
        markers: &BTreeMap<u64, ActivationMarker>,
    ) -> Result<(), FirstSliceError> {
        for marker in markers
            .values()
            .filter(|marker| marker.manifest.generation == generation)
        {
            PrivateDirectory::open(repository.capability(), &marker.name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
        // Marker removal is the safety boundary: after this sync, a crash can
        // expose only an unreferenced corrupt tree, never reactivate it.
        repository
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        let generation_name = generation.to_string();
        let generation_directory =
            PrivateDirectory::open(repository.capability(), OsStr::new(&generation_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let quarantine_name =
            random_quarantine_name(repository_id, generation, activation_sequence)?;
        let quarantined = match generation_directory
            .publish_noreplace(self.quarantine.capability(), OsStr::new(&quarantine_name))
        {
            Ok(directory) => directory,
            Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => directory,
            Err(PublishError::NotCommitted { .. }) => return Err(FirstSliceError::Catalog),
            Err(_) => return Err(FirstSliceError::Catalog),
        };
        quarantined
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.quarantine
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        repository
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.compact_quarantine()
    }

    fn compact_quarantine(&self) -> Result<(), FirstSliceError> {
        let mut names = private_entry_names(&self.quarantine)?;
        if names.len() <= MAX_QUARANTINED_GENERATIONS {
            return Ok(());
        }
        names.sort_unstable();
        let remove_count = names.len() - MAX_QUARANTINED_GENERATIONS;
        for name in names.into_iter().take(remove_count) {
            PrivateDirectory::open(self.quarantine.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
        self.quarantine
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)
    }

    pub(super) fn compact_repository(
        &self,
        repository: RepositoryId,
        retained: &BTreeSet<GenerationId>,
    ) -> Result<(), FirstSliceError> {
        let repository_name = repository.to_string();
        let repository =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let names = private_entry_names(&repository)?;
        let mut markers = BTreeMap::<u64, ActivationMarker>::new();
        let mut generation_names = BTreeSet::new();
        for name in names {
            let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            if text.starts_with(STAGING_PREFIX) {
                continue;
            }
            if let Some((sequence, generation)) = parse_activation_name(text) {
                let marker = read_activation_marker(&repository, name, sequence, generation)?;
                if markers.insert(sequence, marker).is_some() {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
            } else if parse_metadata_name(text).is_some() {
                continue;
            } else if let Ok(generation) = GenerationId::from_str(text) {
                generation_names.insert(generation);
            } else {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        if !retained.is_subset(&generation_names)
            || markers
                .values()
                .any(|marker| !generation_names.contains(&marker.manifest.generation))
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let retained_marker_names = retained_activation_marker_names(&markers, retained);
        if retained.iter().any(|generation| {
            !markers
                .values()
                .any(|marker| marker.manifest.generation == *generation)
        }) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        compact_repository_entries(
            &repository,
            &markers,
            &generation_names,
            retained,
            &retained_marker_names,
        )
    }
}

impl DurablePreparedGeneration {
    pub(super) fn path(&self) -> &Path {
        &self.staging_path
    }

    pub(super) fn write_sources(
        &self,
        sources: &[RustSourceInput],
    ) -> Result<u64, FirstSliceError> {
        let staging = self.staging();
        let sources_directory = staging
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut written_bytes = 0_u64;
        for source in sources {
            let mut file = sources_directory
                .create_file(OsStr::new(&source.snapshot.file().to_string()))
                .map_err(|_| FirstSliceError::Catalog)?;
            file.write_all(source.snapshot.content())
                .map_err(|_| FirstSliceError::Catalog)?;
            file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
            let source_bytes = u64::try_from(source.snapshot.content().len())
                .map_err(|_| FirstSliceError::Limits)?;
            self.account_staging_bytes(source_bytes)?;
            written_bytes = written_bytes
                .checked_add(source_bytes)
                .ok_or(FirstSliceError::Limits)?;
        }
        sources_directory
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        Ok(written_bytes)
    }

    pub(super) fn write_recovery_snapshot(
        &self,
        snapshot: &GenerationSnapshot,
        expected_bytes: u64,
    ) -> Result<u64, FirstSliceError> {
        if snapshot.metadata().generation() != self.generation {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let staging = self.staging();
        let file = staging
            .create_file(OsStr::new(RECOVERY_SNAPSHOT_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut writer = buffered_recovery_writer(file);
        serde_json::to_writer(&mut writer, snapshot.document())
            .map_err(|_| FirstSliceError::Catalog)?;
        writer.flush().map_err(|_| FirstSliceError::Catalog)?;
        if writer.bytes != expected_bytes || writer.bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(writer.bytes)?;
        let metadata = snapshot.metadata();
        let contract = metadata.contract_version();
        let recovery = DurableRecoverySnapshot {
            version: RECOVERY_SNAPSHOT_VERSION,
            bytes: writer.bytes,
            digest: ContentHash::from_bytes(*writer.hasher.finalize().as_bytes()),
            contract_major: contract.major(),
            contract_minor: contract.minor(),
            manifest_hash: metadata.manifest_hash(),
            configuration_hash: metadata.configuration_hash(),
            provider_set_hash: metadata.provider_set_hash(),
        };
        let descriptor = serde_json::to_vec(&recovery).map_err(|_| FirstSliceError::Catalog)?;
        let descriptor_bytes =
            u64::try_from(descriptor.len()).map_err(|_| FirstSliceError::Limits)?;
        if descriptor_bytes > MAX_RECOVERY_MANIFEST_BYTES {
            return Err(FirstSliceError::Limits);
        }
        let mut descriptor_file = staging
            .create_file(OsStr::new(RECOVERY_MANIFEST_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        descriptor_file
            .write_all(&descriptor)
            .map_err(|_| FirstSliceError::Catalog)?;
        descriptor_file
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(descriptor_bytes)?;
        writer
            .bytes
            .checked_add(descriptor_bytes)
            .ok_or(FirstSliceError::Limits)
    }

    pub(super) fn finish(
        &self,
        root_identity: ContentHash,
        display_name: &str,
        root_path: &str,
        receipt: FirstSliceIndexReceipt,
    ) -> Result<u64, FirstSliceError> {
        if receipt.generation != self.generation || display_name.is_empty() || root_path.is_empty()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let manifest = DurableGenerationManifest {
            version: GENERATION_MANIFEST_VERSION,
            root_identity,
            display_name: display_name.to_owned(),
            root_path: Some(root_path.to_owned()),
            receipt,
        };
        let bytes = serde_json::to_vec(&manifest).map_err(|_| FirstSliceError::Catalog)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
            return Err(FirstSliceError::Limits);
        }
        let staging = self.staging();
        let mut file = staging
            .create_file(OsStr::new(MANIFEST_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        file.write_all(&bytes)
            .map_err(|_| FirstSliceError::Catalog)?;
        file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        drop(file);
        staging.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        let manifest_bytes = u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?;
        self.account_staging_bytes(manifest_bytes)?;
        Ok(manifest_bytes)
    }

    pub(super) fn publish(mut self) -> Result<DurablePublishedGeneration, FirstSliceError> {
        let staging = self.staging.take().ok_or(FirstSliceError::Catalog)?;
        let generation_name = self.generation.to_string();
        let directory = match staging
            .publish_noreplace(self.repository().capability(), OsStr::new(&generation_name))
        {
            Ok(directory) => directory,
            Err(PublishError::NotCommitted { .. }) => {
                self.release_staging_bytes();
                return Err(FirstSliceError::Catalog);
            }
            Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
                directory.remove().map_err(|_| FirstSliceError::Catalog)?;
                self.release_staging_bytes();
                return Err(FirstSliceError::Catalog);
            }
            Err(_) => {
                self.release_staging_bytes();
                return Err(FirstSliceError::Catalog);
            }
        };
        self.release_staging_bytes();
        let repository = self.repository.take().ok_or(FirstSliceError::Catalog)?;
        Ok(DurablePublishedGeneration {
            directory: Some(directory),
            repository,
            generation: self.generation,
        })
    }

    fn staging(&self) -> &PrivateDirectory<'static> {
        self.staging
            .as_ref()
            .expect("prepared durable generation retains staging ownership")
    }

    fn repository(&self) -> &PrivateDirectory<'static> {
        self.repository
            .as_ref()
            .expect("prepared durable generation retains repository ownership")
    }

    pub(super) fn account_external_staging_bytes(&self, bytes: u64) -> Result<(), FirstSliceError> {
        self.account_staging_bytes(bytes)
    }

    fn account_staging_bytes(&self, bytes: u64) -> Result<(), FirstSliceError> {
        self.accounted_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(bytes)
            })
            .map_err(|_| FirstSliceError::Limits)?;
        if self
            .staging_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(bytes)
            })
            .is_err()
        {
            self.accounted_bytes.fetch_sub(bytes, Ordering::AcqRel);
            return Err(FirstSliceError::Limits);
        }
        Ok(())
    }

    fn release_staging_bytes(&self) {
        let bytes = self.accounted_bytes.swap(0, Ordering::AcqRel);
        let _ = self
            .staging_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_sub(bytes))
            });
    }
}

impl DurablePublishedGeneration {
    pub(super) fn activate(
        &mut self,
        repository_activation_sequence: u64,
        global_activation_sequence: u64,
        published_generation_count: u64,
        operation: Option<FirstSliceOperationContext>,
    ) -> Result<u64, FirstSliceError> {
        publish_activation_marker(
            &self.repository,
            self.generation,
            repository_activation_sequence,
            global_activation_sequence,
            published_generation_count,
            operation,
        )
    }

    pub(super) fn disarm(mut self) {
        drop(self.directory.take());
    }

    pub(super) fn discard(mut self) -> Result<(), FirstSliceError> {
        self.directory
            .take()
            .ok_or(FirstSliceError::Catalog)?
            .remove()
            .map_err(|_| FirstSliceError::Catalog)
    }
}

impl Drop for DurablePreparedGeneration {
    fn drop(&mut self) {
        if let Some(staging) = self.staging.take() {
            // The primary preparation error remains authoritative; restart
            // recovery removes this validated staging tree if cleanup fails.
            if staging.remove().is_ok() {
                self.release_staging_bytes();
            }
        }
    }
}

fn ensure_private_directory(
    parent: &Dir,
    name: &OsStr,
) -> Result<PrivateDirectory<'static>, FirstSliceError> {
    match PrivateDirectory::create(parent, name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.is_already_exists() => {
            PrivateDirectory::open(parent, name).map_err(|_| FirstSliceError::Catalog)
        }
        Err(_) => Err(FirstSliceError::Catalog),
    }
}

fn private_entry_names(directory: &PrivateDirectory<'_>) -> Result<Vec<OsString>, FirstSliceError> {
    let entries = directory
        .capability()
        .entries()
        .map_err(|_| FirstSliceError::Catalog)?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| FirstSliceError::Catalog)?;
        if names.len() >= MAX_DURABLE_ENTRIES {
            return Err(FirstSliceError::Retention);
        }
        names
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Retention)?;
        names.push(entry.file_name());
    }
    names.sort();
    Ok(names)
}

fn read_activation_marker(
    repository: &PrivateDirectory<'_>,
    name: OsString,
    sequence: u64,
    generation: GenerationId,
) -> Result<ActivationMarker, FirstSliceError> {
    if sequence == 0 {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let marker = PrivateDirectory::open(repository.capability(), &name)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let bytes = marker
        .read_file_bounded(
            OsStr::new(ACTIVATION_MANIFEST_FILENAME),
            MAX_ACTIVATION_MANIFEST_BYTES,
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
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
    if !version_is_valid || manifest.generation != generation {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(ActivationMarker {
        name,
        sequence,
        manifest,
    })
}

fn read_repository_metadata(
    repository_id: RepositoryId,
    repository: &PrivateDirectory<'_>,
    name: &OsStr,
    sequence: u64,
) -> Result<DurableRepositoryMetadata, FirstSliceError> {
    let directory = PrivateDirectory::open(repository.capability(), name)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let bytes = directory
        .read_file_bounded(OsStr::new(REPOSITORY_METADATA_FILENAME), MAX_MANIFEST_BYTES)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let metadata: DurableRepositoryMetadata =
        serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if metadata.version != REPOSITORY_METADATA_VERSION
        || metadata.sequence != sequence
        || metadata.repository != repository_id
        || !valid_repository_metadata(&metadata)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(metadata)
}

fn valid_repository_metadata(metadata: &DurableRepositoryMetadata) -> bool {
    valid_repository_root_path(metadata.root_path.as_deref())
        && metadata.alias.as_deref().is_none_or(|alias| {
            !alias.is_empty()
                && alias.len() <= super::catalog::CATALOG_MAX_LABEL_BYTES
                && !alias
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        })
}

fn valid_repository_root_path(root_path: Option<&str>) -> bool {
    root_path.is_none_or(|path| {
        !path.is_empty()
            && path.len() <= super::catalog::CATALOG_MAX_ROOT_PATH_BYTES
            && !path.chars().any(char::is_control)
    })
}

fn retained_activation_marker_names(
    markers: &BTreeMap<u64, ActivationMarker>,
    retained_generations: &BTreeSet<GenerationId>,
) -> BTreeSet<OsString> {
    let mut latest_by_generation = BTreeMap::<GenerationId, &ActivationMarker>::new();
    for marker in markers.values() {
        if retained_generations.contains(&marker.manifest.generation) {
            latest_by_generation.insert(marker.manifest.generation, marker);
        }
    }
    let mut retained: BTreeSet<_> = latest_by_generation
        .values()
        .map(|marker| marker.name.clone())
        .collect();
    retained.extend(
        markers
            .values()
            .rev()
            .filter(|marker| {
                retained_generations.contains(&marker.manifest.generation)
                    && marker.manifest.operation.is_some()
            })
            .take(MAX_RESTORED_OPERATIONS)
            .map(|marker| marker.name.clone()),
    );
    retained
}

fn compact_repository_entries(
    repository: &PrivateDirectory<'_>,
    markers: &BTreeMap<u64, ActivationMarker>,
    generation_names: &BTreeSet<GenerationId>,
    retained_generations: &BTreeSet<GenerationId>,
    retained_marker_names: &BTreeSet<OsString>,
) -> Result<(), FirstSliceError> {
    for marker in markers.values() {
        if !retained_marker_names.contains(&marker.name) {
            PrivateDirectory::open(repository.capability(), &marker.name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
    }
    repository
        .sync_all()
        .map_err(|_| FirstSliceError::Catalog)?;
    let obsolete_generations: BTreeSet<_> = generation_names
        .difference(retained_generations)
        .copied()
        .collect();
    remove_generation_directories(repository, &obsolete_generations)?;
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn compact_repository_metadata(
    repository: &PrivateDirectory<'_>,
    retained_sequence: u64,
) -> Result<(), FirstSliceError> {
    let names = private_entry_names(repository)?;
    for name in names {
        let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
        if parse_metadata_name(text).is_some_and(|sequence| sequence != retained_sequence) {
            PrivateDirectory::open(repository.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
    }
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn remove_repository_metadata_directories<'a>(
    repository: &PrivateDirectory<'_>,
    names: impl IntoIterator<Item = &'a OsString>,
) -> Result<(), FirstSliceError> {
    for name in names {
        PrivateDirectory::open(repository.capability(), name)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?
            .remove()
            .map_err(|_| FirstSliceError::Catalog)?;
    }
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn remove_generation_directories(
    repository: &PrivateDirectory<'_>,
    generations: &BTreeSet<GenerationId>,
) -> Result<(), FirstSliceError> {
    for generation in generations {
        PrivateDirectory::open(repository.capability(), OsStr::new(&generation.to_string()))
            .map_err(|_| FirstSliceError::CatalogCorrupt)?
            .remove()
            .map_err(|_| FirstSliceError::Catalog)?;
    }
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn compact_activation_markers(repository: &PrivateDirectory<'_>) -> Result<(), FirstSliceError> {
    let names = private_entry_names(repository)?;
    let mut markers = BTreeMap::<u64, ActivationMarker>::new();
    let mut generations = BTreeSet::new();
    for name in names {
        let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
        if text.starts_with(STAGING_PREFIX) {
            continue;
        }
        if let Some((sequence, generation)) = parse_activation_name(text) {
            let marker = read_activation_marker(repository, name, sequence, generation)?;
            if markers.insert(sequence, marker).is_some() {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        } else if parse_metadata_name(text).is_some() {
            continue;
        } else if let Ok(generation) = GenerationId::from_str(text) {
            generations.insert(generation);
        } else {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    if markers
        .values()
        .any(|marker| !generations.contains(&marker.manifest.generation))
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let retained_marker_names = retained_activation_marker_names(&markers, &generations);
    for marker in markers.values() {
        if !retained_marker_names.contains(&marker.name) {
            PrivateDirectory::open(repository.capability(), &marker.name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
    }
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn restore_generation(
    request: GenerationRestoreRequest<'_>,
    cancellation: &Cancellation,
) -> Result<RestoredGeneration, FirstSliceError> {
    let GenerationRestoreRequest {
        repository,
        generation,
        activation_sequence,
        global_activation_sequence,
        published_generation_count,
        repository_directory,
        repository_path,
    } = request;
    let generation_name = generation.to_string();
    let generation_directory = PrivateDirectory::open(
        repository_directory.capability(),
        OsStr::new(&generation_name),
    )
    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let manifest_bytes = generation_directory
        .read_file_bounded(OsStr::new(MANIFEST_FILENAME), MAX_MANIFEST_BYTES)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let manifest: DurableGenerationManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if manifest.version != GENERATION_MANIFEST_VERSION
        || manifest.receipt.repository != repository
        || manifest.receipt.generation != generation
        || !valid_repository_root_path(manifest.root_path.as_deref())
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }

    let context = GenerationContext::new(cancellation, GenerationBudget::default());
    let generation_path = repository_path.join(&generation_name);
    let recovered = restore_recovery_generation(
        &generation_directory,
        repository,
        generation,
        manifest.receipt.parent,
        &context,
        cancellation,
    );
    let (verified, allocated_bytes, defer_sources) = match recovered {
        Ok(Some(verified)) => (verified, manifest.receipt.oracle_allocated_bytes, true),
        Ok(None) | Err(FirstSliceError::CatalogCorrupt) => {
            let (verified, allocated_bytes) = restore_oracle_generation(
                &generation_path,
                repository,
                generation,
                manifest.receipt.parent,
                manifest.receipt.oracle_allocated_bytes,
                &context,
                cancellation,
            )?;
            (verified, allocated_bytes, false)
        }
        Err(error) => return Err(error),
    };
    let documents =
        project_lexical_documents(verified.snapshot(), BuildBudget::default(), cancellation)
            .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
    if u64::try_from(verified.document().files.len()).ok() != Some(manifest.receipt.indexed_files)
        || u64::try_from(verified.document().entities.len()).ok() != Some(manifest.receipt.entities)
        || u64::try_from(documents.len()).ok() != Some(manifest.receipt.lexical_documents)
        || allocated_bytes != manifest.receipt.oracle_allocated_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let search =
        LexicalIndex::build_ephemeral(generation, documents, BuildBudget::default(), cancellation)
            .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    let mut sources = Vec::new();
    if !defer_sources {
        sources
            .try_reserve_exact(verified.document().files.len())
            .map_err(|_| FirstSliceError::Retention)?;
        for file in &verified.document().files {
            sources.push(RustSourceInput {
                snapshot: read_persisted_source(
                    &generation_directory,
                    repository,
                    file,
                    cancellation,
                )?,
                generated: file.generated,
                origins: Vec::new(),
            });
        }
    }
    Ok(RestoredGeneration {
        root_identity: manifest.root_identity,
        display_name: manifest.display_name,
        root_path: manifest.root_path,
        alias: None,
        metadata_sequence: 0,
        receipt: manifest.receipt,
        activation_sequence,
        global_activation_sequence,
        published_generation_count,
        verified,
        search,
        sources,
        operations: Vec::new(),
    })
}

fn restore_recovery_generation(
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    context: &GenerationContext<'_>,
    cancellation: &Cancellation,
) -> Result<Option<IdentityVerifiedGeneration>, FirstSliceError> {
    let names = private_entry_names(generation_directory)?;
    if !names
        .iter()
        .any(|name| name == OsStr::new(RECOVERY_MANIFEST_FILENAME))
    {
        return Ok(None);
    }
    check_cancellation(cancellation)?;
    let descriptor = generation_directory
        .read_file_bounded(
            OsStr::new(RECOVERY_MANIFEST_FILENAME),
            MAX_RECOVERY_MANIFEST_BYTES,
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let recovery: DurableRecoverySnapshot =
        serde_json::from_slice(&descriptor).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if recovery.version != RECOVERY_SNAPSHOT_VERSION
        || recovery.bytes == 0
        || recovery.bytes > MAX_RECOVERY_SNAPSHOT_BYTES
        || GenerationContractVersion::new(recovery.contract_major, recovery.contract_minor)
            != GENERATION_CONTRACT_VERSION
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let encoded = generation_directory
        .read_file_bounded_cancellable(
            OsStr::new(RECOVERY_SNAPSHOT_FILENAME),
            recovery.bytes,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    if u64::try_from(encoded.len()).ok() != Some(recovery.bytes) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let metadata = GenerationMetadata::new_for_contract(
        GenerationContractVersion::new(recovery.contract_major, recovery.contract_minor),
        repository,
        generation,
        parent,
        recovery.manifest_hash,
        recovery.configuration_hash,
        recovery.provider_set_hash,
    )
    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let mut recovery_limits = IrLimits::default();
    // Published in-memory documents may legitimately serialize beyond the
    // default import envelope. The sidecar supplies the exact checksummed
    // length, still capped by the recovery hard bound.
    recovery_limits.max_document_bytes =
        usize::try_from(recovery.bytes).map_err(|_| FirstSliceError::Limits)?;
    IdentityVerifiedGeneration::restore_published_json(
        metadata,
        &encoded,
        recovery.digest,
        &recovery_limits,
        &ExtensionSupport::default(),
        context,
    )
    .map(Some)
    .map_err(|error| match error {
        error @ IdentityVerificationError::Control(_) => map_identity_error(error, cancellation),
        _ => FirstSliceError::CatalogCorrupt,
    })
}

fn restore_oracle_generation(
    generation_path: &Path,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    expected_allocated_bytes: u64,
    context: &GenerationContext<'_>,
    cancellation: &Cancellation,
) -> Result<(IdentityVerifiedGeneration, u64), FirstSliceError> {
    let oracle = OracleReader::open_in(generation_path, context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let allocated_bytes = oracle
        .allocated_bytes(context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let persisted = oracle
        .read(context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let metadata = persisted.metadata();
    if metadata.repository() != repository
        || metadata.generation() != generation
        || metadata.parent() != parent
        || allocated_bytes != expected_allocated_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let verified = IdentityVerifiedGeneration::verify_snapshot(persisted, context)
        .map_err(|error| map_identity_error(error, cancellation))?;
    Ok((verified, allocated_bytes))
}

fn read_persisted_source(
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    file: &FileRecord,
    cancellation: &Cancellation,
) -> Result<SourceSnapshot, FirstSliceError> {
    check_cancellation(cancellation)?;
    if file.repository != repository
        || file.byte_length > DEFAULT_MAX_SOURCE_FILE_BYTES
        || file.byte_length > MAX_SNAPSHOT_BYTES
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let locator = file
        .path_locator
        .as_ref()
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    let path = RelativePath::from_locator(locator)
        .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))?;
    let sources = PrivateDirectory::open(
        generation_directory.capability(),
        OsStr::new(SOURCES_DIRECTORY),
    )
    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let bytes = sources
        .read_file_bounded_cancellable(
            OsStr::new(&file.id.to_string()),
            file.byte_length,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    if u64::try_from(bytes.len()).ok() != Some(file.byte_length) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    SourceSnapshot::from_persisted(repository, path, file.id, file.content_hash, bytes)
        .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))
}

fn map_private_read_error(error: PlatformError) -> FirstSliceError {
    match error {
        PlatformError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::CatalogCorrupt,
    }
}

fn generation_data_error(error: FirstSliceError) -> FirstSliceError {
    match error {
        FirstSliceError::Cancelled(reason) => FirstSliceError::Cancelled(reason),
        _ => FirstSliceError::CatalogCorrupt,
    }
}

fn publish_activation_marker(
    repository: &PrivateDirectory<'_>,
    generation: GenerationId,
    repository_activation_sequence: u64,
    global_activation_sequence: u64,
    published_generation_count: u64,
    operation: Option<FirstSliceOperationContext>,
) -> Result<u64, FirstSliceError> {
    if repository_activation_sequence == 0
        || global_activation_sequence == 0
        || published_generation_count == 0
    {
        return Err(FirstSliceError::Retention);
    }
    compact_activation_markers(repository)?;
    let staging_name = random_activation_staging_name(generation)?;
    let staging = PrivateDirectory::create(repository.capability(), OsStr::new(&staging_name))
        .map_err(|_| FirstSliceError::Catalog)?;
    let manifest = DurableActivationManifest {
        version: ACTIVATION_MANIFEST_VERSION,
        generation,
        global_activation_sequence: Some(global_activation_sequence),
        published_generation_count: Some(published_generation_count),
        // Keep the version-2 manifest byte contract rollback-readable. Provider
        // diagnostics remain process-local until a new manifest version exists.
        operation: operation.map(DurableOperationContextV2::from),
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| FirstSliceError::Catalog)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ACTIVATION_MANIFEST_BYTES {
        return Err(FirstSliceError::Limits);
    }
    {
        let mut file = staging
            .create_file(OsStr::new(ACTIVATION_MANIFEST_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        file.write_all(&bytes)
            .map_err(|_| FirstSliceError::Catalog)?;
        file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
    }
    staging.sync_all().map_err(|_| FirstSliceError::Catalog)?;
    let marker_name = activation_name(repository_activation_sequence, generation);
    match staging.publish_noreplace(repository.capability(), OsStr::new(&marker_name)) {
        Ok(marker) => {
            marker.sync_all().map_err(|_| FirstSliceError::Catalog)?;
            u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)
        }
        Err(PublishError::NotCommitted { .. }) => Err(FirstSliceError::Catalog),
        Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
            directory.remove().map_err(|_| FirstSliceError::Catalog)?;
            Err(FirstSliceError::Catalog)
        }
        Err(_) => Err(FirstSliceError::Catalog),
    }
}

fn random_staging_name(generation: GenerationId) -> Result<String, FirstSliceError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
    Ok(format!(
        "{STAGING_PREFIX}{generation}-{}",
        lower_hex(&nonce)
    ))
}

fn random_activation_staging_name(generation: GenerationId) -> Result<String, FirstSliceError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
    Ok(format!(
        "{STAGING_PREFIX}activation-{generation}-{}",
        lower_hex(&nonce)
    ))
}

fn random_metadata_staging_name(sequence: u64) -> Result<String, FirstSliceError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
    Ok(format!(
        "{STAGING_PREFIX}metadata-{sequence:020}-{}",
        lower_hex(&nonce)
    ))
}

fn random_quarantine_name(
    repository: RepositoryId,
    generation: GenerationId,
    activation_sequence: u64,
) -> Result<String, FirstSliceError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
    Ok(format!(
        "{QUARANTINE_PREFIX}{activation_sequence:020}-{repository}-{generation}-{}",
        lower_hex(&nonce)
    ))
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn activation_name(sequence: u64, generation: GenerationId) -> String {
    format!("{ACTIVATION_PREFIX}{sequence:020}-{generation}")
}

fn metadata_name(sequence: u64) -> String {
    format!("{METADATA_PREFIX}{sequence:020}")
}

fn parse_metadata_name(name: &str) -> Option<u64> {
    let sequence = name.strip_prefix(METADATA_PREFIX)?;
    if sequence.len() != 20 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let sequence = sequence.parse().ok()?;
    (sequence > 0).then_some(sequence)
}

fn parse_activation_name(name: &str) -> Option<(u64, GenerationId)> {
    let value = name.strip_prefix(ACTIVATION_PREFIX)?;
    let (sequence, generation) = value.split_once('-')?;
    if sequence.len() != 20 || !sequence.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let sequence = sequence.parse().ok()?;
    let generation = GenerationId::from_str(generation).ok()?;
    Some((sequence, generation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FirstSliceService;
    use rootlight_cancel::Cancellation;
    use rootlight_ids::{GenerationIdentity, content_hash, derive_generation, derive_repository};
    use rootlight_runtime::RuntimePaths;
    use std::{fs, io, time::Duration};
    use tempfile::TempDir;

    #[derive(Default)]
    struct CountingWriter {
        bytes: usize,
        writes: usize,
    }

    impl io::Write for CountingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(buffer.len())
                .expect("test byte count is representable");
            self.writes = self
                .writes
                .checked_add(1)
                .expect("test write count is representable");
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn durable_test_tempdir() -> TempDir {
        #[cfg(target_os = "macos")]
        {
            // Avoid the default `/var` alias rejected by repository-root VFS checks.
            tempfile::Builder::new()
                .prefix("rl-quarantine-")
                .tempdir_in("/private/tmp")
                .expect("durable test directory is available")
        }
        #[cfg(not(target_os = "macos"))]
        {
            TempDir::new().expect("durable test directory is available")
        }
    }

    #[test]
    fn activation_names_round_trip_exact_sequence_and_generation() {
        let repository = derive_repository(b"durable-activation").id();
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: content_hash(b"manifest"),
            config_hash: content_hash(b"config"),
            provider_set_hash: content_hash(b"provider"),
            format_version: 1,
        })
        .id();
        let name = activation_name(42, generation);
        assert_eq!(parse_activation_name(&name), Some((42, generation)));
        assert!(parse_activation_name("activation-42-invalid").is_none());
    }

    #[test]
    fn recovery_snapshot_writer_batches_small_serialization_fragments() {
        let mut writer = buffered_recovery_writer(CountingWriter::default());
        for _ in 0..10_000 {
            writer
                .write_all(b"x")
                .expect("buffered recovery fragment writes");
        }
        writer.flush().expect("buffered recovery writer flushes");

        assert_eq!(writer.bytes, 10_000);
        assert_eq!(writer.inner.get_ref().bytes, 10_000);
        assert_eq!(writer.inner.get_ref().writes, 1);
    }

    #[test]
    fn activation_manifest_v2_keeps_the_legacy_operation_shape() {
        let repository = derive_repository(b"durable-operation-shape").id();
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: content_hash(b"manifest"),
            config_hash: content_hash(b"config"),
            provider_set_hash: content_hash(b"provider"),
            format_version: 1,
        })
        .id();
        let operation = FirstSliceOperationContext {
            operation: rootlight_ids::OperationId::from_bytes([7; 16]),
            started_unix_ms: 42,
            provider: super::super::FirstSliceIndexProvider::ProjectAnalyzer,
        };
        let manifest = DurableActivationManifest {
            version: ACTIVATION_MANIFEST_VERSION,
            generation,
            global_activation_sequence: Some(1),
            published_generation_count: Some(1),
            operation: Some(operation.into()),
        };

        let encoded = serde_json::to_value(&manifest).expect("activation manifest serializes");
        assert!(
            encoded["operation"].get("provider").is_none(),
            "version-2 manifests must remain readable by the previous binary"
        );
        let decoded: DurableActivationManifest =
            serde_json::from_value(encoded).expect("activation manifest round trips");
        let restored = FirstSliceOperationContext::from(
            decoded.operation.expect("operation context is retained"),
        );
        assert_eq!(restored.operation, operation.operation);
        assert_eq!(restored.started_unix_ms, operation.started_unix_ms);
        assert_eq!(
            restored.provider,
            super::super::FirstSliceIndexProvider::Unknown
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn corrupt_newest_generation_is_quarantined_and_predecessor_becomes_active() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn quarantine_target() -> u32 { 1 }\n")
            .expect("initial source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let (first, second) = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let first = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("first generation publishes");
            fs::write(&source, "pub fn quarantine_target() -> u32 { 2 }\n")
                .expect("successor source writes");
            let second = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("second generation publishes");
            (first, second)
        };
        let repositories = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY);
        let repository = repositories.join(first.repository.to_string());
        fs::write(
            repository
                .join(second.generation.to_string())
                .join(MANIFEST_FILENAME),
            b"{",
        )
        .expect("newest generation manifest is corrupted");

        let mut restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("predecessor restores after generation-scoped quarantine");
        assert_eq!(
            restored.active_generation_for(first.repository),
            Some(first.generation)
        );
        assert!(matches!(
            restored.resolve_generation(first.repository, Some(second.generation)),
            Err(FirstSliceError::GenerationNotFound)
        ));
        assert_eq!(
            restored.published_generation_counts.get(&first.repository),
            Some(&2)
        );
        assert!(!repository.join(second.generation.to_string()).exists());
        let quarantine = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(QUARANTINE_DIRECTORY);
        assert_eq!(
            fs::read_dir(&quarantine)
                .expect("quarantine directory reads")
                .count(),
            1
        );
        assert!(
            fs::read_dir(&repository)
                .expect("repository directory reads")
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.starts_with(ACTIVATION_PREFIX))
                .all(|name| !name.contains(&second.generation.to_string()))
        );

        fs::write(&source, "pub fn quarantine_target() -> u32 { 3 }\n")
            .expect("replacement source writes");
        let replacement = restored
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("replacement generation publishes");
        assert_eq!(replacement.parent, Some(first.generation));
        assert_eq!(
            restored.published_generation_counts.get(&first.repository),
            Some(&3)
        );
    }
}
