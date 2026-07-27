//! Crash-safe generation publication and restoration for the first-slice service.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr as _,
};

use cap_std::{ambient_authority, fs::Dir};
use rootlight_cancel::Cancellation;
use rootlight_catalog::OracleReader;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_search::{BuildBudget, LexicalIndex};
use rootlight_storage::{GenerationBudget, GenerationContext, IdentityVerifiedGeneration};
use rootlight_vfs::{
    MAX_SNAPSHOT_BYTES, RelativePath, SourceSnapshot,
    platform::{PrivateDirectory, PublishError, PublishedPrivateDirectory},
};
use serde::{Deserialize, Serialize};

use super::{
    FirstSliceError, FirstSliceIndexReceipt, FirstSliceOperationContext, MAX_SOURCE_BYTES,
    RustSourceInput, check_cancellation, map_catalog_error, map_query_error, map_search_error,
    map_vfs_error, project_lexical_documents,
};

const DURABLE_DIRECTORY: &str = "first-slice";
const REPOSITORIES_DIRECTORY: &str = "repositories";
const QUARANTINE_DIRECTORY: &str = "quarantine";
const SOURCES_DIRECTORY: &str = "sources";
const MANIFEST_FILENAME: &str = "manifest.json";
const ACTIVATION_MANIFEST_FILENAME: &str = "activation.json";
const GENERATION_MANIFEST_VERSION: u16 = 1;
const ACTIVATION_MANIFEST_VERSION: u16 = 2;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ACTIVATION_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_DURABLE_ENTRIES: usize = 65_536;
const MAX_RESTORED_OPERATIONS: usize = 256;
const MAX_QUARANTINED_GENERATIONS: usize = 256;
const STAGING_PREFIX: &str = "stage-";
const ACTIVATION_PREFIX: &str = "activation-";
const QUARANTINE_PREFIX: &str = "generation-";

pub(super) struct DurableCatalog {
    repositories: PrivateDirectory<'static>,
    quarantine: PrivateDirectory<'static>,
    repositories_path: PathBuf,
    maximum_generations_per_repository: usize,
}

pub(super) struct DurablePreparedGeneration {
    staging: Option<PrivateDirectory<'static>>,
    staging_path: PathBuf,
    repository: Option<PrivateDirectory<'static>>,
    generation: GenerationId,
}

pub(super) struct DurablePublishedGeneration {
    directory: Option<PublishedPrivateDirectory>,
    repository: PrivateDirectory<'static>,
    generation: GenerationId,
}

pub(super) struct RestoredGeneration {
    pub(super) root_identity: ContentHash,
    pub(super) display_name: String,
    pub(super) receipt: FirstSliceIndexReceipt,
    pub(super) activation_sequence: u64,
    pub(super) global_activation_sequence: Option<u64>,
    pub(super) published_generation_count: Option<u64>,
    pub(super) verified: IdentityVerifiedGeneration,
    pub(super) search: LexicalIndex,
    pub(super) sources: Vec<RustSourceInput>,
    pub(super) operations: Vec<FirstSliceOperationContext>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableGenerationManifest {
    version: u16,
    root_identity: ContentHash,
    display_name: String,
    receipt: FirstSliceIndexReceipt,
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
    operation: Option<FirstSliceOperationContext>,
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

    pub(super) fn restore(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
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
    ) -> Result<(), FirstSliceError> {
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

    fn restore_repository(
        &self,
        repository_id: RepositoryId,
        repository: &PrivateDirectory<'_>,
        repository_path: &Path,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        let names = private_entry_names(repository)?;
        let mut markers = BTreeMap::<u64, ActivationMarker>::new();
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
            return Ok(Vec::new());
        }

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

        let mut restored = Vec::new();
        restored
            .try_reserve_exact(self.maximum_generations_per_repository)
            .map_err(|_| FirstSliceError::Retention)?;
        let mut corrupted = Vec::new();
        for (activation_sequence, generation) in recency {
            if restored.len() == self.maximum_generations_per_repository {
                break;
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
        let retained: BTreeSet<_> = restored
            .iter()
            .map(|generation| generation.receipt.generation)
            .collect();
        let retained_marker_names = retained_activation_marker_names(&markers, &retained);
        for restored_generation in &mut restored {
            let generation = restored_generation.receipt.generation;
            restored_generation.operations = markers
                .values()
                .filter_map(|marker| {
                    (marker.manifest.generation == generation
                        && retained_marker_names.contains(&marker.name))
                    .then_some(marker.manifest.operation)
                    .flatten()
                })
                .collect();
        }
        compact_repository_entries(
            repository,
            &markers,
            &generation_names,
            &retained,
            &retained_marker_names,
        )?;
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

    pub(super) fn write_sources(&self, sources: &[RustSourceInput]) -> Result<(), FirstSliceError> {
        let staging = self.staging();
        let sources_directory = staging
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        for source in sources {
            let mut file = sources_directory
                .create_file(OsStr::new(&source.snapshot.file().to_string()))
                .map_err(|_| FirstSliceError::Catalog)?;
            file.write_all(source.snapshot.content())
                .map_err(|_| FirstSliceError::Catalog)?;
            file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        }
        sources_directory
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)
    }

    pub(super) fn finish(
        &self,
        root_identity: ContentHash,
        display_name: &str,
        receipt: FirstSliceIndexReceipt,
    ) -> Result<(), FirstSliceError> {
        if receipt.generation != self.generation || display_name.is_empty() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let manifest = DurableGenerationManifest {
            version: GENERATION_MANIFEST_VERSION,
            root_identity,
            display_name: display_name.to_owned(),
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
        staging.sync_all().map_err(|_| FirstSliceError::Catalog)
    }

    pub(super) fn publish(mut self) -> Result<DurablePublishedGeneration, FirstSliceError> {
        let staging = self.staging.take().ok_or(FirstSliceError::Catalog)?;
        let generation_name = self.generation.to_string();
        let directory = match staging
            .publish_noreplace(self.repository().capability(), OsStr::new(&generation_name))
        {
            Ok(directory) => directory,
            Err(PublishError::NotCommitted { .. }) => return Err(FirstSliceError::Catalog),
            Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
                directory.remove().map_err(|_| FirstSliceError::Catalog)?;
                return Err(FirstSliceError::Catalog);
            }
            Err(_) => return Err(FirstSliceError::Catalog),
        };
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
}

impl DurablePublishedGeneration {
    pub(super) fn activate(
        &mut self,
        repository_activation_sequence: u64,
        global_activation_sequence: u64,
        published_generation_count: u64,
        operation: Option<FirstSliceOperationContext>,
    ) -> Result<(), FirstSliceError> {
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
            let _ = staging.remove();
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
        GENERATION_MANIFEST_VERSION => {
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
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }

    let context = GenerationContext::new(cancellation, GenerationBudget::default());
    let generation_path = repository_path.join(&generation_name);
    let oracle = OracleReader::open_in(&generation_path, &context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let allocated_bytes = oracle
        .allocated_bytes(&context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let persisted = oracle
        .read(&context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let metadata = persisted.metadata();
    if metadata.repository() != repository
        || metadata.generation() != generation
        || metadata.parent() != manifest.receipt.parent
        || allocated_bytes != manifest.receipt.oracle_allocated_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let documents = project_lexical_documents(&persisted, BuildBudget::default(), cancellation)
        .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
    if u64::try_from(persisted.document().files.len()).ok() != Some(manifest.receipt.indexed_files)
        || u64::try_from(persisted.document().entities.len()).ok()
            != Some(manifest.receipt.entities)
        || u64::try_from(documents.len()).ok() != Some(manifest.receipt.lexical_documents)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let search =
        LexicalIndex::build_ephemeral(generation, documents, BuildBudget::default(), cancellation)
            .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    let sources_directory = PrivateDirectory::open(
        generation_directory.capability(),
        OsStr::new(SOURCES_DIRECTORY),
    )
    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(persisted.document().files.len())
        .map_err(|_| FirstSliceError::Retention)?;
    for file in &persisted.document().files {
        check_cancellation(cancellation)?;
        if file.byte_length > u64::try_from(MAX_SOURCE_BYTES).unwrap_or(u64::MAX)
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
        let bytes = sources_directory
            .read_file_bounded(OsStr::new(&file.id.to_string()), file.byte_length)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if u64::try_from(bytes.len()).ok() != Some(file.byte_length) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let snapshot =
            SourceSnapshot::from_persisted(repository, path, file.id, file.content_hash, bytes)
                .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))?;
        sources.push(RustSourceInput {
            snapshot,
            generated: file.generated,
        });
    }
    let verified = oracle
        .read_verified(&context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    Ok(RestoredGeneration {
        root_identity: manifest.root_identity,
        display_name: manifest.display_name,
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
) -> Result<(), FirstSliceError> {
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
        operation,
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
        Ok(marker) => marker.sync_all().map_err(|_| FirstSliceError::Catalog),
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
    use std::{fs, time::Duration};
    use tempfile::TempDir;

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
