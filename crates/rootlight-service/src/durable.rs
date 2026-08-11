//! Crash-safe generation publication and restoration for the first-slice service.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{BufWriter, Read as _, Write as _},
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use cap_std::{ambient_authority, fs::Dir};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use rootlight_cancel::Cancellation;
use rootlight_catalog::OracleReader;
use rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES;
use rootlight_discovery::IncrementalDiscoveryBaseline;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use rootlight_incremental::{
    BaselineFile, FileDescriptor, FileMetadata, InputFingerprint, InputKey, InputSnapshot,
    MetadataBaseline, MetadataReliability, PlanningLimits, PlatformFileIdentity, ReconcileLimits,
};
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
    FirstSliceError, FirstSliceIncrementalEvidence, FirstSliceIndexReceipt,
    FirstSliceOperationContext, FirstSliceRecoveryTarget, PreparedIncrementalState,
    RustSourceInput, check_cancellation, map_catalog_error, map_identity_error,
    map_incremental_error, map_query_error, map_search_error, map_vfs_error,
    project_lexical_documents,
};

const DURABLE_DIRECTORY: &str = "first-slice";
const REPOSITORIES_DIRECTORY: &str = "repositories";
const QUARANTINE_DIRECTORY: &str = "quarantine";
const SOURCES_DIRECTORY: &str = "sources";
const SOURCE_BLOBS_DIRECTORY: &str = "source-blobs";
const SOURCE_BLOB_PAYLOAD_FILENAME: &str = "content";
const SOURCE_POINTER_MAGIC: &[u8] = b"rootlight.source-pointer/1\n";
const MANIFEST_FILENAME: &str = "manifest.json";
const RECOVERY_SNAPSHOT_FILENAME: &str = "recovery.json";
const RECOVERY_SNAPSHOT_GZIP_FILENAME: &str = "recovery.json.gz";
const RECOVERY_MANIFEST_FILENAME: &str = "recovery-manifest.json";
const INCREMENTAL_STATE_FILENAME: &str = "incremental.json";
const ACTIVATION_MANIFEST_FILENAME: &str = "activation.json";
const REPOSITORY_METADATA_FILENAME: &str = "metadata.json";
const LEGACY_GENERATION_MANIFEST_VERSION: u16 = 1;
const GENERATION_MANIFEST_VERSION: u16 = 2;
pub(super) const REPOSITORY_METADATA_VERSION: u16 = 1;
const SOURCE_STORAGE_VERSION: u16 = 1;
const LEGACY_RECOVERY_SNAPSHOT_VERSION: u16 = 1;
const RECOVERY_SNAPSHOT_VERSION: u16 = 2;
const INCREMENTAL_STATE_VERSION: u16 = 1;
const LEGACY_ACTIVATION_MANIFEST_VERSION: u16 = 1;
const ACTIVATION_MANIFEST_VERSION: u16 = 2;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ACTIVATION_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_RECOVERY_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_RECOVERY_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RECOVERY_ENCODED_BYTES: u64 = MAX_RECOVERY_SNAPSHOT_BYTES + 1024 * 1024;
const RECOVERY_DECODE_BUFFER_BYTES: usize = 64 * 1024;
const MAX_INCREMENTAL_STATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_POINTER_BYTES: u64 = 256;
const RECOVERY_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_DURABLE_ENTRIES: usize = 65_536;
const MAX_SOURCE_BLOB_ENTRIES: usize = 1_000_000;
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
    incremental_state: Mutex<Option<DurableSidecarDescriptor>>,
    source_storage: Mutex<Option<DurableSourceStorage>>,
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
    pub(super) incremental: Option<PreparedIncrementalState>,
    pub(super) operations: Vec<FirstSliceOperationContext>,
}

struct RestorePolicy<'a> {
    maximum_generations: usize,
    excluded: &'a BTreeSet<GenerationId>,
    compact: bool,
    repair: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    incremental_state: Option<DurableSidecarDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_storage: Option<DurableSourceStorage>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceStorage {
    version: u16,
}

pub(super) struct DurableSourceWrite {
    pub(super) newly_written_bytes: u64,
    pub(super) referenced_bytes: u64,
}

struct SourcePointer {
    digest: ContentHash,
    bytes: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableSidecarDescriptor {
    bytes: u64,
    digest: ContentHash,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableIncrementalState {
    version: u16,
    baseline_files: Vec<DurableBaselineFile>,
    baseline_inputs: Vec<DurableInputFingerprint>,
    analysis_inputs: Vec<DurableInputFingerprint>,
    evidence: FirstSliceIncrementalEvidence,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableBaselineFile {
    file: rootlight_ids::FileId,
    path_hash: ContentHash,
    content_hash: ContentHash,
    length: u64,
    modified_ns: Option<u128>,
    change_token: Option<u128>,
    identity: Option<DurablePlatformFileIdentity>,
    reliability: MetadataReliability,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurablePlatformFileIdentity {
    volume: u64,
    file_index: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableInputFingerprint {
    key: InputKey,
    value: ContentHash,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoding: Option<RecoverySnapshotEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decoded_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decoded_digest: Option<ContentHash>,
    contract_major: u16,
    contract_minor: u16,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecoverySnapshotEncoding {
    Gzip,
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

fn content_hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn decode_recovery_snapshot(
    encoded: &[u8],
    expected_bytes: u64,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FirstSliceError> {
    let mut decoder = GzDecoder::new(encoded);
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(usize::try_from(expected_bytes).map_err(|_| FirstSliceError::Limits)?)
        .map_err(|_| FirstSliceError::Limits)?;
    let mut buffer = [0_u8; RECOVERY_DECODE_BUFFER_BYTES];
    loop {
        check_cancellation(cancellation)?;
        let read = decoder
            .read(&mut buffer)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if read == 0 {
            break;
        }
        let next = u64::try_from(decoded.len())
            .map_err(|_| FirstSliceError::Limits)?
            .checked_add(u64::try_from(read).map_err(|_| FirstSliceError::Limits)?)
            .ok_or(FirstSliceError::Limits)?;
        if next > expected_bytes {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        decoded
            .try_reserve(read)
            .map_err(|_| FirstSliceError::Limits)?;
        decoded.extend_from_slice(&buffer[..read]);
    }
    if u64::try_from(decoded.len()).ok() != Some(expected_bytes) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(decoded)
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

impl DurableIncrementalState {
    fn from_prepared(state: &PreparedIncrementalState) -> Result<Self, FirstSliceError> {
        let baseline_files = state
            .baseline
            .metadata()
            .files()
            .map(|file| {
                let descriptor = file.descriptor();
                let metadata = descriptor.metadata();
                DurableBaselineFile {
                    file: descriptor.file(),
                    path_hash: descriptor.path_hash(),
                    content_hash: file.content_hash(),
                    length: metadata.length(),
                    modified_ns: metadata.modified_ns(),
                    change_token: metadata.change_token(),
                    identity: metadata
                        .identity()
                        .map(|identity| DurablePlatformFileIdentity {
                            volume: identity.volume(),
                            file_index: identity.file_index(),
                        }),
                    reliability: metadata.reliability(),
                }
            })
            .collect();
        let baseline_inputs = durable_input_fingerprints(state.baseline.inputs());
        let analysis_inputs = durable_input_fingerprints(&state.inputs);
        validate_incremental_evidence(&state.evidence)?;
        Ok(Self {
            version: INCREMENTAL_STATE_VERSION,
            baseline_files,
            baseline_inputs,
            analysis_inputs,
            evidence: state.evidence.clone(),
        })
    }

    fn into_prepared(
        self,
        cancellation: &Cancellation,
    ) -> Result<PreparedIncrementalState, FirstSliceError> {
        if self.version != INCREMENTAL_STATE_VERSION {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let reconcile_limits = ReconcileLimits::new(self.baseline_files.len().max(1))
            .map_err(|error| map_incremental_error(error, cancellation))?;
        let mut baseline_files = Vec::new();
        baseline_files
            .try_reserve_exact(self.baseline_files.len())
            .map_err(|_| FirstSliceError::Retention)?;
        for file in self.baseline_files {
            check_cancellation(cancellation)?;
            let identity = file
                .identity
                .map(|identity| PlatformFileIdentity::new(identity.volume, identity.file_index));
            let metadata = match file.reliability {
                MetadataReliability::Trusted => FileMetadata::trusted_with_change_token(
                    file.length,
                    file.modified_ns.ok_or(FirstSliceError::CatalogCorrupt)?,
                    file.change_token.ok_or(FirstSliceError::CatalogCorrupt)?,
                    identity.ok_or(FirstSliceError::CatalogCorrupt)?,
                ),
                MetadataReliability::Untrusted => FileMetadata::untrusted_with_change_token(
                    file.length,
                    file.modified_ns,
                    file.change_token,
                    identity,
                ),
            };
            baseline_files.push(BaselineFile::new(
                FileDescriptor::new(file.file, file.path_hash, metadata),
                file.content_hash,
            ));
        }
        let metadata = MetadataBaseline::new(baseline_files, reconcile_limits, cancellation)
            .map_err(|error| map_incremental_error(error, cancellation))?;
        let baseline_inputs = restore_input_snapshot(self.baseline_inputs, cancellation)?;
        let analysis_inputs = restore_input_snapshot(self.analysis_inputs, cancellation)?;
        validate_incremental_evidence(&self.evidence)?;
        Ok(PreparedIncrementalState {
            baseline: IncrementalDiscoveryBaseline::from_validated_parts(metadata, baseline_inputs),
            inputs: analysis_inputs,
            evidence: self.evidence,
        })
    }
}

fn durable_input_fingerprints(snapshot: &InputSnapshot) -> Vec<DurableInputFingerprint> {
    snapshot
        .iter()
        .map(|input| DurableInputFingerprint {
            key: input.key(),
            value: input.value(),
        })
        .collect()
}

fn restore_input_snapshot(
    inputs: Vec<DurableInputFingerprint>,
    cancellation: &Cancellation,
) -> Result<InputSnapshot, FirstSliceError> {
    let limits = PlanningLimits::new(inputs.len().max(1), 1, 1, 1)
        .map_err(|error| map_incremental_error(error, cancellation))?;
    InputSnapshot::new(
        inputs
            .into_iter()
            .map(|input| InputFingerprint::new(input.key, input.value)),
        limits,
        cancellation,
    )
    .map_err(|error| map_incremental_error(error, cancellation))
}

fn validate_incremental_evidence(
    evidence: &FirstSliceIncrementalEvidence,
) -> Result<(), FirstSliceError> {
    if evidence.input_changes.len() > 9
        || evidence.file_changes.len() > 6
        || evidence.invalidated_domains.len() > 9
        || evidence
            .parsed_files
            .checked_add(evidence.reused_parser_artifacts)
            .is_none_or(|total| total > evidence.lowered_files)
        || evidence.reused_parser_artifacts == 0 && evidence.reused_parser_artifact_bytes != 0
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let input_classes = evidence
        .input_changes
        .iter()
        .map(|change| change.class)
        .collect::<BTreeSet<_>>();
    let file_kinds = evidence
        .file_changes
        .iter()
        .map(|change| change.kind)
        .collect::<BTreeSet<_>>();
    let domains = evidence
        .invalidated_domains
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let fallback_matches_strategy = match evidence.strategy {
        super::FirstSliceBuildStrategy::Initial
        | super::FirstSliceBuildStrategy::DependencyDirected => evidence.fallback_reason.is_none(),
        super::FirstSliceBuildStrategy::ConservativeRepositoryRebuild => {
            evidence.fallback_reason.is_some()
        }
    };
    if input_classes.len() != evidence.input_changes.len()
        || file_kinds.len() != evidence.file_changes.len()
        || domains.len() != evidence.invalidated_domains.len()
        || evidence
            .input_changes
            .iter()
            .any(|change| change.inputs == 0)
        || evidence.file_changes.iter().any(|change| change.files == 0)
        || !fallback_matches_strategy
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
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
            incremental_state: Mutex::new(None),
            source_storage: Mutex::new(None),
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
        let manifest = generation
            .read_file_bounded(OsStr::new(MANIFEST_FILENAME), MAX_MANIFEST_BYTES)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&manifest).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let uses_source_blobs = match (manifest.version, manifest.source_storage) {
            (LEGACY_GENERATION_MANIFEST_VERSION, None) => false,
            (
                GENERATION_MANIFEST_VERSION,
                Some(DurableSourceStorage {
                    version: SOURCE_STORAGE_VERSION,
                }),
            ) => true,
            _ => return Err(FirstSliceError::CatalogCorrupt),
        };
        read_persisted_source(
            &repository,
            &generation,
            file.repository,
            file,
            uses_source_blobs,
            cancellation,
        )
    }

    pub(super) fn restore(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        self.restore_with_policy(
            self.maximum_generations_per_repository,
            &BTreeSet::new(),
            true,
            true,
            cancellation,
        )
    }

    pub(super) fn restore_active(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        self.restore_with_policy(1, &BTreeSet::new(), false, true, cancellation)
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

    pub(super) fn active_restore_targets(
        &self,
    ) -> Result<Vec<FirstSliceRecoveryTarget>, FirstSliceError> {
        let repository_names = private_entry_names(&self.repositories)?;
        if repository_names.len() > super::MAX_FIRST_SLICE_REPOSITORIES {
            return Err(FirstSliceError::Retention);
        }
        let mut targets = Vec::new();
        for repository_name in repository_names {
            let repository_text = repository_name
                .to_str()
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            let repository_id = RepositoryId::from_str(repository_text)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let repository =
                PrivateDirectory::open(self.repositories.capability(), &repository_name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let mut newest = None;
            for entry_name in private_entry_names(&repository)? {
                let entry_text = entry_name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
                let Some((sequence, generation)) = parse_activation_name(entry_text) else {
                    continue;
                };
                let marker = read_activation_marker(&repository, entry_name, sequence, generation)?;
                let ordering = marker
                    .manifest
                    .global_activation_sequence
                    .unwrap_or(marker.sequence);
                if newest.is_none_or(|(current, _, _)| ordering > current) {
                    newest = Some((ordering, marker.sequence, generation));
                }
            }
            if let Some((ordering, sequence, generation)) = newest {
                targets.push((
                    ordering,
                    sequence,
                    FirstSliceRecoveryTarget {
                        repository: repository_id,
                        generation,
                    },
                ));
            }
        }
        targets.sort_unstable_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.repository.cmp(&right.2.repository))
        });
        Ok(targets.into_iter().map(|(_, _, target)| target).collect())
    }

    pub(super) fn restore_active_repository(
        &self,
        repository_id: RepositoryId,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let repository_name = repository_id.to_string();
        let repository =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let repository_path = self.repositories_path.join(&repository_name);
        self.restore_repository(
            repository_id,
            &repository,
            &repository_path,
            &RestorePolicy {
                maximum_generations: 1,
                excluded: &BTreeSet::new(),
                compact: false,
                repair: true,
            },
            cancellation,
        )
    }

    pub(super) fn restore_retained_repository(
        &self,
        repository_id: RepositoryId,
        excluded: &BTreeSet<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        check_cancellation(cancellation)?;
        let repository_name = repository_id.to_string();
        let repository =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let repository_path = self.repositories_path.join(&repository_name);
        self.restore_repository(
            repository_id,
            &repository,
            &repository_path,
            &RestorePolicy {
                maximum_generations: self.maximum_generations_per_repository,
                excluded,
                compact: false,
                repair: false,
            },
            cancellation,
        )
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
            true,
            cancellation,
        )
    }

    fn restore_with_policy(
        &self,
        maximum_generations_per_repository: usize,
        excluded: &BTreeSet<GenerationId>,
        compact: bool,
        repair: bool,
        cancellation: &Cancellation,
    ) -> Result<Vec<RestoredGeneration>, FirstSliceError> {
        let policy = RestorePolicy {
            maximum_generations: maximum_generations_per_repository,
            excluded,
            compact,
            repair,
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
            } else if text == SOURCE_BLOBS_DIRECTORY {
                continue;
            } else if let Ok(generation) = GenerationId::from_str(text) {
                generation_names.insert(generation);
            } else {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }

        if policy.repair {
            for staging_name in staging_names {
                PrivateDirectory::open(repository.capability(), &staging_name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?
                    .remove()
                    .map_err(|_| FirstSliceError::Catalog)?;
            }
        }

        if markers.is_empty() {
            if policy.repair {
                remove_generation_directories(repository, &generation_names)?;
                remove_repository_metadata_directories(repository, metadata_names.values())?;
                compact_source_blobs(repository, &BTreeSet::new())?;
            }
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
        if policy.repair {
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
            } else if parse_metadata_name(text).is_some() || text == SOURCE_BLOBS_DIRECTORY {
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
    ) -> Result<DurableSourceWrite, FirstSliceError> {
        let staging = self.staging();
        let sources_directory = staging
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        let blobs = ensure_private_directory(
            self.repository().capability(),
            OsStr::new(SOURCE_BLOBS_DIRECTORY),
        )?;
        let mut created = BTreeSet::new();
        let mut newly_written_bytes = 0_u64;
        let mut referenced_bytes = 0_u64;
        for source in sources {
            let content = source.snapshot.content();
            let digest = source.snapshot.content_hash();
            let content_bytes =
                u64::try_from(content.len()).map_err(|_| FirstSliceError::Limits)?;
            let newly_written = if created.contains(&digest) {
                false
            } else {
                persist_source_blob(&blobs, digest, content)?
            };
            if newly_written {
                created.insert(digest);
                newly_written_bytes = newly_written_bytes
                    .checked_add(content_bytes)
                    .ok_or(FirstSliceError::Limits)?;
            } else if !created.contains(&digest) {
                referenced_bytes = referenced_bytes
                    .checked_add(content_bytes)
                    .ok_or(FirstSliceError::Limits)?;
            }
            let pointer = encode_source_pointer(digest, content_bytes)?;
            let mut file = sources_directory
                .create_file(OsStr::new(&source.snapshot.file().to_string()))
                .map_err(|_| FirstSliceError::Catalog)?;
            file.write_all(&pointer)
                .map_err(|_| FirstSliceError::Catalog)?;
            file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
            let pointer_bytes =
                u64::try_from(pointer.len()).map_err(|_| FirstSliceError::Limits)?;
            newly_written_bytes = newly_written_bytes
                .checked_add(pointer_bytes)
                .ok_or(FirstSliceError::Limits)?;
        }
        sources_directory
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        blobs.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(newly_written_bytes)?;
        let mut storage = self
            .source_storage
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if storage
            .replace(DurableSourceStorage {
                version: SOURCE_STORAGE_VERSION,
            })
            .is_some()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(DurableSourceWrite {
            newly_written_bytes,
            referenced_bytes,
        })
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
            .create_file(OsStr::new(RECOVERY_SNAPSHOT_GZIP_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let encoded_writer = buffered_recovery_writer(file);
        let encoder = GzEncoder::new(encoded_writer, Compression::fast());
        let mut decoded_writer = RecoverySnapshotWriter::new(encoder);
        serde_json::to_writer(&mut decoded_writer, snapshot.document())
            .map_err(|_| FirstSliceError::Catalog)?;
        decoded_writer
            .flush()
            .map_err(|_| FirstSliceError::Catalog)?;
        if decoded_writer.bytes != expected_bytes
            || decoded_writer.bytes > MAX_RECOVERY_SNAPSHOT_BYTES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let decoded_bytes = decoded_writer.bytes;
        let decoded_digest = ContentHash::from_bytes(*decoded_writer.hasher.finalize().as_bytes());
        let mut encoded_writer = decoded_writer
            .inner
            .finish()
            .map_err(|_| FirstSliceError::Catalog)?;
        encoded_writer
            .flush()
            .map_err(|_| FirstSliceError::Catalog)?;
        if encoded_writer.bytes == 0 || encoded_writer.bytes > MAX_RECOVERY_ENCODED_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        encoded_writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(encoded_writer.bytes)?;
        let metadata = snapshot.metadata();
        let contract = metadata.contract_version();
        let recovery = DurableRecoverySnapshot {
            version: RECOVERY_SNAPSHOT_VERSION,
            bytes: encoded_writer.bytes,
            digest: ContentHash::from_bytes(*encoded_writer.hasher.finalize().as_bytes()),
            encoding: Some(RecoverySnapshotEncoding::Gzip),
            decoded_bytes: Some(decoded_bytes),
            decoded_digest: Some(decoded_digest),
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
        encoded_writer
            .bytes
            .checked_add(descriptor_bytes)
            .ok_or(FirstSliceError::Limits)
    }

    pub(super) fn write_incremental_state(
        &self,
        state: &PreparedIncrementalState,
    ) -> Result<u64, FirstSliceError> {
        let durable = DurableIncrementalState::from_prepared(state)?;
        let staging = self.staging();
        let file = staging
            .create_file(OsStr::new(INCREMENTAL_STATE_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut writer = buffered_recovery_writer(file);
        serde_json::to_writer(&mut writer, &durable).map_err(|_| FirstSliceError::Catalog)?;
        writer.flush().map_err(|_| FirstSliceError::Catalog)?;
        if writer.bytes > MAX_INCREMENTAL_STATE_BYTES {
            return Err(FirstSliceError::Limits);
        }
        writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(writer.bytes)?;
        let descriptor = DurableSidecarDescriptor {
            bytes: writer.bytes,
            digest: ContentHash::from_bytes(*writer.hasher.finalize().as_bytes()),
        };
        let mut slot = self
            .incremental_state
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if slot.replace(descriptor).is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(writer.bytes)
    }

    pub(super) fn finish(
        &self,
        root_identity: ContentHash,
        display_name: &str,
        root_path: &str,
        receipt: &mut FirstSliceIndexReceipt,
    ) -> Result<u64, FirstSliceError> {
        if receipt.generation != self.generation || display_name.is_empty() || root_path.is_empty()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let retained_before_manifest = self.accounted_bytes.load(Ordering::Acquire);
        let incremental_state = *self
            .incremental_state
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        let source_storage = *self
            .source_storage
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        // The manifest stores its own contribution to retained size. Re-encode
        // until the decimal field width is reflected in that exact total.
        let bytes = loop {
            let manifest = DurableGenerationManifest {
                version: GENERATION_MANIFEST_VERSION,
                root_identity,
                display_name: display_name.to_owned(),
                root_path: Some(root_path.to_owned()),
                receipt: receipt.clone(),
                incremental_state,
                source_storage,
            };
            let bytes = serde_json::to_vec(&manifest).map_err(|_| FirstSliceError::Catalog)?;
            let retained_durable_bytes = retained_before_manifest
                .checked_add(u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?)
                .ok_or(FirstSliceError::Limits)?;
            if receipt.retained_durable_bytes == retained_durable_bytes {
                break bytes;
            }
            receipt.retained_durable_bytes = retained_durable_bytes;
        };
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

fn persist_source_blob(
    blobs: &PrivateDirectory<'_>,
    digest: ContentHash,
    content: &[u8],
) -> Result<bool, FirstSliceError> {
    if content_hash_bytes(content) != digest {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let name = digest.to_string();
    match blobs.capability().symlink_metadata(Path::new(&name)) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            validate_source_blob(blobs, digest, content)?;
            return Ok(false);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(FirstSliceError::Catalog),
    }
    let staging_name = random_source_blob_staging_name(digest)?;
    let staging = PrivateDirectory::create(blobs.capability(), OsStr::new(&staging_name))
        .map_err(|_| FirstSliceError::Catalog)?;
    {
        let mut payload = staging
            .create_file(OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        payload
            .write_all(content)
            .map_err(|_| FirstSliceError::Catalog)?;
        payload.sync_all().map_err(|_| FirstSliceError::Catalog)?;
    }
    staging.sync_all().map_err(|_| FirstSliceError::Catalog)?;
    match staging.publish_noreplace(blobs.capability(), OsStr::new(&name)) {
        Ok(published) => {
            published.sync_all().map_err(|_| FirstSliceError::Catalog)?;
            Ok(true)
        }
        Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
            directory.remove().map_err(|_| FirstSliceError::Catalog)?;
            Err(FirstSliceError::Catalog)
        }
        Err(PublishError::NotCommitted { source }) if source.is_already_exists() => {
            validate_source_blob(blobs, digest, content)?;
            Ok(false)
        }
        Err(_) => Err(FirstSliceError::Catalog),
    }
}

fn validate_source_blob(
    blobs: &PrivateDirectory<'_>,
    digest: ContentHash,
    expected: &[u8],
) -> Result<(), FirstSliceError> {
    let directory = PrivateDirectory::open(blobs.capability(), OsStr::new(&digest.to_string()))
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let maximum = u64::try_from(expected.len()).map_err(|_| FirstSliceError::Limits)?;
    let actual = directory
        .read_file_bounded(OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME), maximum)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if actual != expected || content_hash_bytes(&actual) != digest {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

fn encode_source_pointer(digest: ContentHash, bytes: u64) -> Result<Vec<u8>, FirstSliceError> {
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(
            SOURCE_POINTER_MAGIC
                .len()
                .checked_add(digest.to_string().len())
                .and_then(|length| length.checked_add(1 + 20 + 1))
                .ok_or(FirstSliceError::Limits)?,
        )
        .map_err(|_| FirstSliceError::Retention)?;
    encoded.extend_from_slice(SOURCE_POINTER_MAGIC);
    encoded.extend_from_slice(digest.to_string().as_bytes());
    encoded.push(b'\n');
    encoded.extend_from_slice(bytes.to_string().as_bytes());
    encoded.push(b'\n');
    if u64::try_from(encoded.len()).map_err(|_| FirstSliceError::Limits)? > MAX_SOURCE_POINTER_BYTES
    {
        return Err(FirstSliceError::Limits);
    }
    Ok(encoded)
}

fn decode_source_pointer(encoded: &[u8]) -> Result<SourcePointer, FirstSliceError> {
    if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > MAX_SOURCE_POINTER_BYTES {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let payload = encoded
        .strip_prefix(SOURCE_POINTER_MAGIC)
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    let payload = std::str::from_utf8(payload).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let mut fields = payload.split('\n');
    let digest = fields
        .next()
        .ok_or(FirstSliceError::CatalogCorrupt)
        .and_then(|value| {
            ContentHash::from_str(value).map_err(|_| FirstSliceError::CatalogCorrupt)
        })?;
    let bytes = fields
        .next()
        .ok_or(FirstSliceError::CatalogCorrupt)?
        .parse::<u64>()
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if fields.next() != Some("") || fields.next().is_some() {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(SourcePointer { digest, bytes })
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
    let referenced = retained_source_blobs(repository, retained_generations)?;
    compact_source_blobs(repository, &referenced)?;
    repository.sync_all().map_err(|_| FirstSliceError::Catalog)
}

fn retained_source_blobs(
    repository: &PrivateDirectory<'_>,
    retained_generations: &BTreeSet<GenerationId>,
) -> Result<BTreeSet<ContentHash>, FirstSliceError> {
    let mut referenced = BTreeSet::new();
    for generation in retained_generations {
        let generation =
            PrivateDirectory::open(repository.capability(), OsStr::new(&generation.to_string()))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let manifest = generation
            .read_file_bounded(OsStr::new(MANIFEST_FILENAME), MAX_MANIFEST_BYTES)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let manifest: DurableGenerationManifest =
            serde_json::from_slice(&manifest).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        match (manifest.version, manifest.source_storage) {
            (LEGACY_GENERATION_MANIFEST_VERSION, None) => continue,
            (
                GENERATION_MANIFEST_VERSION,
                Some(DurableSourceStorage {
                    version: SOURCE_STORAGE_VERSION,
                }),
            ) => {}
            _ => return Err(FirstSliceError::CatalogCorrupt),
        }
        let sources =
            PrivateDirectory::open(generation.capability(), OsStr::new(SOURCES_DIRECTORY))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        for name in private_entry_names(&sources)? {
            let pointer = sources
                .read_file_bounded(&name, MAX_SOURCE_POINTER_BYTES)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            referenced.insert(decode_source_pointer(&pointer)?.digest);
        }
    }
    Ok(referenced)
}

fn compact_source_blobs(
    repository: &PrivateDirectory<'_>,
    referenced: &BTreeSet<ContentHash>,
) -> Result<(), FirstSliceError> {
    let metadata = match repository
        .capability()
        .symlink_metadata(Path::new(SOURCE_BLOBS_DIRECTORY))
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if referenced.is_empty() {
                Ok(())
            } else {
                Err(FirstSliceError::CatalogCorrupt)
            };
        }
        Err(_) => return Err(FirstSliceError::Catalog),
    };
    if !metadata.is_dir() {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let blobs = PrivateDirectory::open(repository.capability(), OsStr::new(SOURCE_BLOBS_DIRECTORY))
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let entries = blobs
        .capability()
        .entries()
        .map_err(|_| FirstSliceError::Catalog)?;
    let mut visited = 0_usize;
    let mut observed = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|_| FirstSliceError::Catalog)?;
        visited = visited.checked_add(1).ok_or(FirstSliceError::Limits)?;
        if visited > MAX_SOURCE_BLOB_ENTRIES {
            return Err(FirstSliceError::Retention);
        }
        let name = entry.file_name();
        let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
        let remove = if text.starts_with(STAGING_PREFIX) {
            true
        } else {
            let digest =
                ContentHash::from_str(text).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            observed.insert(digest);
            !referenced.contains(&digest)
        };
        if remove {
            PrivateDirectory::open(blobs.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?
                .remove()
                .map_err(|_| FirstSliceError::Catalog)?;
        }
    }
    if !referenced.is_subset(&observed) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    blobs.sync_all().map_err(|_| FirstSliceError::Catalog)
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
        } else if parse_metadata_name(text).is_some() || text == SOURCE_BLOBS_DIRECTORY {
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
    let uses_source_blobs = match (manifest.version, manifest.source_storage) {
        (LEGACY_GENERATION_MANIFEST_VERSION, None) => false,
        (
            GENERATION_MANIFEST_VERSION,
            Some(DurableSourceStorage {
                version: SOURCE_STORAGE_VERSION,
            }),
        ) => true,
        _ => return Err(FirstSliceError::CatalogCorrupt),
    };
    if manifest.receipt.repository != repository
        || manifest.receipt.generation != generation
        || !valid_repository_root_path(manifest.root_path.as_deref())
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let incremental = match restore_incremental_state(
        &generation_directory,
        manifest.incremental_state,
        cancellation,
    ) {
        Ok(incremental) => incremental,
        Err(FirstSliceError::CatalogCorrupt) => None,
        Err(error) => return Err(error),
    };

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
                    repository_directory,
                    &generation_directory,
                    repository,
                    file,
                    uses_source_blobs,
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
        incremental,
        operations: Vec::new(),
    })
}

fn restore_incremental_state(
    generation_directory: &PrivateDirectory<'_>,
    descriptor: Option<DurableSidecarDescriptor>,
    cancellation: &Cancellation,
) -> Result<Option<PreparedIncrementalState>, FirstSliceError> {
    let Some(descriptor) = descriptor else {
        return Ok(None);
    };
    if descriptor.bytes == 0 || descriptor.bytes > MAX_INCREMENTAL_STATE_BYTES {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    let bytes = generation_directory
        .read_file_bounded(OsStr::new(INCREMENTAL_STATE_FILENAME), descriptor.bytes)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if u64::try_from(bytes.len()).ok() != Some(descriptor.bytes)
        || content_hash_bytes(&bytes) != descriptor.digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let durable: DurableIncrementalState =
        serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    durable.into_prepared(cancellation).map(Some)
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
    let contract = GenerationContractVersion::new(recovery.contract_major, recovery.contract_minor);
    if contract != GENERATION_CONTRACT_VERSION {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let (snapshot_name, decoded_bytes, decoded_digest) = match recovery.version {
        LEGACY_RECOVERY_SNAPSHOT_VERSION
            if recovery.encoding.is_none()
                && recovery.decoded_bytes.is_none()
                && recovery.decoded_digest.is_none()
                && recovery.bytes > 0
                && recovery.bytes <= MAX_RECOVERY_SNAPSHOT_BYTES =>
        {
            (RECOVERY_SNAPSHOT_FILENAME, recovery.bytes, recovery.digest)
        }
        RECOVERY_SNAPSHOT_VERSION
            if matches!(recovery.encoding, Some(RecoverySnapshotEncoding::Gzip))
                && recovery.bytes > 0
                && recovery.bytes <= MAX_RECOVERY_ENCODED_BYTES =>
        {
            let decoded_bytes = recovery
                .decoded_bytes
                .filter(|bytes| *bytes > 0 && *bytes <= MAX_RECOVERY_SNAPSHOT_BYTES)
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            let decoded_digest = recovery
                .decoded_digest
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            (
                RECOVERY_SNAPSHOT_GZIP_FILENAME,
                decoded_bytes,
                decoded_digest,
            )
        }
        _ => return Err(FirstSliceError::CatalogCorrupt),
    };
    let encoded = generation_directory
        .read_file_bounded_cancellable(OsStr::new(snapshot_name), recovery.bytes, cancellation)
        .map_err(map_private_read_error)?;
    if u64::try_from(encoded.len()).ok() != Some(recovery.bytes)
        || content_hash_bytes(&encoded) != recovery.digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let decoded = match recovery.version {
        LEGACY_RECOVERY_SNAPSHOT_VERSION => encoded,
        RECOVERY_SNAPSHOT_VERSION => {
            decode_recovery_snapshot(&encoded, decoded_bytes, cancellation)?
        }
        _ => return Err(FirstSliceError::CatalogCorrupt),
    };
    if content_hash_bytes(&decoded) != decoded_digest {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let metadata = GenerationMetadata::new_for_contract(
        contract,
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
        usize::try_from(decoded_bytes).map_err(|_| FirstSliceError::Limits)?;
    IdentityVerifiedGeneration::restore_published_json(
        metadata,
        &decoded,
        decoded_digest,
        &recovery_limits,
        &ExtensionSupport::default(),
        context,
    )
    .map(Some)
    .map_err(|error| map_persisted_identity_error(error, cancellation))
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
        .map_err(|error| map_persisted_identity_error(error, cancellation))?;
    Ok((verified, allocated_bytes))
}

fn map_persisted_identity_error(
    error: IdentityVerificationError,
    cancellation: &Cancellation,
) -> FirstSliceError {
    match error {
        error @ IdentityVerificationError::Control(_) => map_identity_error(error, cancellation),
        _ => FirstSliceError::CatalogCorrupt,
    }
}

fn read_persisted_source(
    repository_directory: &PrivateDirectory<'_>,
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    file: &FileRecord,
    uses_source_blobs: bool,
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
    let maximum = if uses_source_blobs {
        MAX_SOURCE_POINTER_BYTES
    } else {
        file.byte_length
    };
    let persisted = sources
        .read_file_bounded_cancellable(OsStr::new(&file.id.to_string()), maximum, cancellation)
        .map_err(map_private_read_error)?;
    let bytes = if uses_source_blobs {
        let pointer = decode_source_pointer(&persisted)?;
        if pointer.digest != file.content_hash || pointer.bytes != file.byte_length {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let blobs = PrivateDirectory::open(
            repository_directory.capability(),
            OsStr::new(SOURCE_BLOBS_DIRECTORY),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let blob =
            PrivateDirectory::open(blobs.capability(), OsStr::new(&pointer.digest.to_string()))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        blob.read_file_bounded_cancellable(
            OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME),
            pointer.bytes,
            cancellation,
        )
        .map_err(map_private_read_error)?
    } else {
        persisted
    };
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

fn random_source_blob_staging_name(digest: ContentHash) -> Result<String, FirstSliceError> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| FirstSliceError::RandomUnavailable)?;
    Ok(format!(
        "{STAGING_PREFIX}source-{digest}-{}",
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
    fn persisted_identity_failures_are_generation_scoped_corruption() {
        let cancellation = Cancellation::new();

        for error in [
            IdentityVerificationError::InvalidGeneration,
            IdentityVerificationError::LegacyContract,
            IdentityVerificationError::MissingClaim,
            IdentityVerificationError::DuplicateClaim,
            IdentityVerificationError::ManifestMismatch,
            IdentityVerificationError::UnsupportedExtension,
            IdentityVerificationError::RecipeEncoding,
        ] {
            assert_eq!(
                map_persisted_identity_error(error, &cancellation),
                FirstSliceError::CatalogCorrupt
            );
        }
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
