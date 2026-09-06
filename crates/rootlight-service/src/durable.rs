//! Crash-safe generation publication and restoration for the first-slice service.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fmt,
    io::{BufReader, BufWriter, Read as _, Write as _},
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use cap_std::{ambient_authority, fs::Dir};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_catalog::OracleReader;
use rootlight_config::DEFAULT_MAX_SOURCE_FILE_BYTES;
use rootlight_discovery::IncrementalDiscoveryBaseline;
use rootlight_ids::{
    ContentHash, FactId, FileId, FileIdentity, GenerationId, RepositoryId, derive_file,
};
use rootlight_incremental::{
    BaselineFile, FileDescriptor, FileMetadata, InputFingerprint, InputKey, InputSnapshot,
    MetadataBaseline, MetadataReliability, PlanningLimits, PlatformFileIdentity, ReconcileLimits,
};
use rootlight_ir::{
    ExtensionSupport, FactEvidence, FileIdentityClaim, FilePathLocator, FilePathLocatorEncoding,
    FileRecord, IrLimits, SourceRef, SourceSpan,
};
use rootlight_query::project_source_fallback_document_with_text_limit;
use rootlight_search::{BuildBudget, EphemeralLexicalIndexBuilder, LexicalIndex};
use rootlight_storage::{
    GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationContext, GenerationContractVersion,
    GenerationMetadata, GenerationReader, GenerationSnapshot, IdentityVerificationError,
    IdentityVerifiedGeneration, SourceFileCatalog, SourceFileCatalogEntry,
};
use rootlight_vfs::{
    MAX_PATH_BYTES, MAX_PATH_COMPONENTS, MAX_SNAPSHOT_BYTES, RelativePath, SourceSnapshot,
    platform::{PlatformError, PrivateDirectory, PublishError, PublishedPrivateDirectory},
};
use serde::{Deserialize, Serialize};

use super::{
    FirstSliceError, FirstSliceIncrementalEvidence, FirstSliceIndexReceipt,
    FirstSliceLogicalSnapshotIdentity, FirstSliceOperationContext, FirstSliceRecoveryTarget,
    LexicalProjectionBuilder, MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES, PreparedIncrementalState,
    RustSourceInput, SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE, check_cancellation,
    fallible_copy_string, map_catalog_error, map_identity_error, map_incremental_error,
    map_query_error, map_search_error, map_vfs_error, repository_path_hash,
    source_fallback_text_limit,
};

const DURABLE_DIRECTORY: &str = "first-slice";
const REPOSITORIES_DIRECTORY: &str = "repositories";
const QUARANTINE_DIRECTORY: &str = "quarantine";
const SOURCES_DIRECTORY: &str = "sources";
const SOURCE_BLOBS_DIRECTORY: &str = "source-blobs";
const SOURCE_BLOB_PAYLOAD_FILENAME: &str = "content";
const SOURCE_PACK_INDEX_FILENAME: &str = "index.msgpack";
const SOURCE_PACK_PREFIX: &str = "pack-";
const SOURCE_PACK_SUFFIX: &str = ".bin";
const SOURCE_POINTER_MAGIC: &[u8] = b"rootlight.source-pointer/1\n";
const MANIFEST_FILENAME: &str = "manifest.json";
const RECOVERY_SNAPSHOT_FILENAME: &str = "recovery.json";
const RECOVERY_SNAPSHOT_GZIP_FILENAME: &str = "recovery.json.gz";
const RECOVERY_SNAPSHOT_MESSAGEPACK_GZIP_FILENAME: &str = "recovery.msgpack.gz";
const RECOVERY_MANIFEST_FILENAME: &str = "recovery-manifest.json";
const INCREMENTAL_STATE_FILENAME: &str = "incremental.json";
const SOURCE_FILE_CATALOG_FILENAME: &str = "source-files.json";
const LOGICAL_SNAPSHOT_FILENAME: &str = "logical-snapshot.json";
const ACTIVATION_MANIFEST_FILENAME: &str = "activation.json";
const REPOSITORY_METADATA_FILENAME: &str = "metadata.json";
const LEGACY_GENERATION_MANIFEST_VERSION: u16 = 1;
const PACKED_GENERATION_MANIFEST_VERSION: u16 = 2;
const GENERATION_MANIFEST_VERSION: u16 = 3;
pub(super) const REPOSITORY_METADATA_VERSION: u16 = 1;
const LEGACY_SOURCE_STORAGE_VERSION: u16 = 1;
const SOURCE_STORAGE_VERSION: u16 = 2;
const SOURCE_PACK_INDEX_VERSION: u16 = 1;
const LEGACY_RECOVERY_SNAPSHOT_VERSION: u16 = 1;
const JSON_GZIP_RECOVERY_SNAPSHOT_VERSION: u16 = 2;
const RECOVERY_SNAPSHOT_VERSION: u16 = 3;
const LEGACY_INCREMENTAL_STATE_VERSION: u16 = 1;
const INCREMENTAL_STATE_VERSION: u16 = 2;
const LEGACY_SOURCE_FILE_CATALOG_VERSION: u16 = 1;
const SOURCE_FILE_CATALOG_VERSION: u16 = 2;
const LEGACY_LOGICAL_SNAPSHOT_VERSION: u16 = 1;
const LOGICAL_SNAPSHOT_VERSION: u16 = 2;
const LEGACY_ACTIVATION_MANIFEST_VERSION: u16 = 1;
const ACTIVATION_MANIFEST_VERSION: u16 = 2;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ACTIVATION_MANIFEST_BYTES: u64 = 4 * 1024;
pub(super) const DURABLE_PUBLICATION_RESIDUAL_BYTES: u64 = MAX_ACTIVATION_MANIFEST_BYTES;
const MAX_RECOVERY_MANIFEST_BYTES: u64 = 4 * 1024;
const MAX_RECOVERY_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RECOVERY_ENCODED_BYTES: u64 = MAX_RECOVERY_SNAPSHOT_BYTES + 1024 * 1024;
const RECOVERY_DECODE_BUFFER_BYTES: usize = 64 * 1024;
const MAX_INCREMENTAL_STATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_FILE_CATALOG_BYTES: u64 = MAX_INCREMENTAL_STATE_BYTES;
const MAX_LOGICAL_SNAPSHOT_BYTES: u64 = 4 * 1024;
const MAX_SOURCE_POINTER_BYTES: u64 = 256;
const MAX_SOURCE_PACK_INDEX_BYTES: u64 = MAX_INCREMENTAL_STATE_BYTES;
const SOURCE_PACK_TARGET_BYTES: u64 = DEFAULT_MAX_SOURCE_FILE_BYTES;
const PACKED_SOURCE_MIN_FILES: usize = 256;
const STREAMED_SOURCE_PARTITION_FILES: usize = 4_096;
const RECOVERY_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
const RECOVERY_SERIALIZATION_CHECKPOINT_BYTES: usize = 64 * 1024;
const MAX_DURABLE_ENTRIES: usize = 65_536;
const MAX_SOURCE_BLOB_ENTRIES: usize = 1_000_000;
const MAX_STORAGE_INVENTORY_ENTRIES: usize = 2_000_000;
const MAX_RESTORED_OPERATIONS: usize = 256;
const MAX_QUARANTINED_GENERATIONS: usize = 256;
const STAGING_PREFIX: &str = "stage-";
const ACTIVATION_PREFIX: &str = "activation-";
const METADATA_PREFIX: &str = "metadata-";
const QUARANTINE_PREFIX: &str = "generation-";

pub(super) fn recovery_snapshot_output_reservation(
    decoded_bytes: u64,
) -> Result<u64, FirstSliceError> {
    if decoded_bytes == 0 || decoded_bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
        return Err(FirstSliceError::Limits);
    }
    let encoded_bytes = decoded_bytes
        .checked_add(1024 * 1024)
        .filter(|bytes| *bytes <= MAX_RECOVERY_ENCODED_BYTES)
        .ok_or(FirstSliceError::Limits)?;
    encoded_bytes
        .checked_add(MAX_RECOVERY_MANIFEST_BYTES)
        .ok_or(FirstSliceError::Limits)
}

#[cfg(test)]
fn write_test_recovery_file(
    generation_path: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), FirstSliceError> {
    // Ambient file creation can inherit non-private permissions and make a
    // compatibility test silently exercise oracle fallback instead of its codec.
    let parent = Dir::open_ambient_dir(
        generation_path.parent().ok_or(FirstSliceError::Catalog)?,
        ambient_authority(),
    )
    .map_err(|_| FirstSliceError::Catalog)?;
    let generation = PrivateDirectory::open(
        &parent,
        generation_path
            .file_name()
            .ok_or(FirstSliceError::Catalog)?,
    )
    .map_err(|_| FirstSliceError::Catalog)?;
    match generation.capability().remove_file(name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(FirstSliceError::Catalog),
    }
    let mut file = generation
        .create_file(OsStr::new(name))
        .map_err(|_| FirstSliceError::Catalog)?;
    file.write_all(bytes)
        .map_err(|_| FirstSliceError::Catalog)?;
    file.sync_all().map_err(|_| FirstSliceError::Catalog)
}

#[cfg(test)]
pub(super) fn write_legacy_recovery_snapshot(
    generation_directory: &Path,
    snapshot: &GenerationSnapshot,
) -> Result<(), FirstSliceError> {
    let decoded = serde_json::to_vec(snapshot.document()).map_err(|_| FirstSliceError::Catalog)?;
    let decoded_bytes = u64::try_from(decoded.len()).map_err(|_| FirstSliceError::Limits)?;
    if decoded_bytes == 0 || decoded_bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
        return Err(FirstSliceError::Limits);
    }
    write_test_recovery_file(generation_directory, RECOVERY_SNAPSHOT_FILENAME, &decoded)?;
    let metadata = snapshot.metadata();
    let contract = metadata.contract_version();
    let recovery = DurableRecoverySnapshot {
        version: LEGACY_RECOVERY_SNAPSHOT_VERSION,
        bytes: decoded_bytes,
        digest: content_hash_bytes(&decoded),
        encoding: None,
        decoded_bytes: None,
        decoded_digest: None,
        serialized_document_bytes: None,
        contract_major: contract.major(),
        contract_minor: contract.minor(),
        manifest_hash: metadata.manifest_hash(),
        configuration_hash: metadata.configuration_hash(),
        provider_set_hash: metadata.provider_set_hash(),
    };
    let descriptor = serde_json::to_vec(&recovery).map_err(|_| FirstSliceError::Catalog)?;
    if u64::try_from(descriptor.len()).map_err(|_| FirstSliceError::Limits)?
        > MAX_RECOVERY_MANIFEST_BYTES
    {
        return Err(FirstSliceError::Limits);
    }
    write_test_recovery_file(
        generation_directory,
        RECOVERY_MANIFEST_FILENAME,
        &descriptor,
    )
}

#[cfg(test)]
pub(super) fn write_legacy_gzip_recovery_snapshot(
    generation_directory: &Path,
    snapshot: &GenerationSnapshot,
) -> Result<(), FirstSliceError> {
    let decoded = serde_json::to_vec(snapshot.document()).map_err(|_| FirstSliceError::Catalog)?;
    let decoded_bytes = u64::try_from(decoded.len()).map_err(|_| FirstSliceError::Limits)?;
    if decoded_bytes == 0 || decoded_bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
        return Err(FirstSliceError::Limits);
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&decoded)
        .map_err(|_| FirstSliceError::Catalog)?;
    let encoded = encoder.finish().map_err(|_| FirstSliceError::Catalog)?;
    let encoded_bytes = u64::try_from(encoded.len()).map_err(|_| FirstSliceError::Limits)?;
    if encoded_bytes == 0 || encoded_bytes > MAX_RECOVERY_ENCODED_BYTES {
        return Err(FirstSliceError::Limits);
    }
    write_test_recovery_file(
        generation_directory,
        RECOVERY_SNAPSHOT_GZIP_FILENAME,
        &encoded,
    )?;
    let metadata = snapshot.metadata();
    let contract = metadata.contract_version();
    let recovery = DurableRecoverySnapshot {
        version: JSON_GZIP_RECOVERY_SNAPSHOT_VERSION,
        bytes: encoded_bytes,
        digest: content_hash_bytes(&encoded),
        encoding: Some(RecoverySnapshotEncoding::Gzip),
        decoded_bytes: Some(decoded_bytes),
        decoded_digest: Some(content_hash_bytes(&decoded)),
        serialized_document_bytes: None,
        contract_major: contract.major(),
        contract_minor: contract.minor(),
        manifest_hash: metadata.manifest_hash(),
        configuration_hash: metadata.configuration_hash(),
        provider_set_hash: metadata.provider_set_hash(),
    };
    let descriptor = serde_json::to_vec(&recovery).map_err(|_| FirstSliceError::Catalog)?;
    if u64::try_from(descriptor.len()).map_err(|_| FirstSliceError::Limits)?
        > MAX_RECOVERY_MANIFEST_BYTES
    {
        return Err(FirstSliceError::Limits);
    }
    write_test_recovery_file(
        generation_directory,
        RECOVERY_MANIFEST_FILENAME,
        &descriptor,
    )
}

pub(super) struct DurableCatalog {
    repositories: PrivateDirectory<'static>,
    quarantine: PrivateDirectory<'static>,
    repositories_path: PathBuf,
    maximum_generations_per_repository: usize,
    maximum_repositories: usize,
    staging_bytes: Arc<AtomicU64>,
    storage_accounting: Arc<Mutex<DurableStorageAccounting>>,
    generation_cache: Option<Arc<Mutex<super::FirstSliceGenerationCache>>>,
}

pub(super) struct DurablePreparedGeneration {
    staging: Option<PrivateDirectory<'static>>,
    staging_path: PathBuf,
    repository: Option<PrivateDirectory<'static>>,
    repository_id: RepositoryId,
    generation: GenerationId,
    staging_bytes: Arc<AtomicU64>,
    accounted_bytes: AtomicU64,
    incremental_state: Mutex<Option<DurableSidecarDescriptor>>,
    source_file_catalog: Mutex<Option<DurableSidecarDescriptor>>,
    source_storage: Mutex<Option<DurableSourceStorage>>,
    created_source_blobs: Mutex<BTreeSet<ContentHash>>,
    storage_accounting: Arc<Mutex<DurableStorageAccounting>>,
}

pub(super) struct DurablePublishedGeneration {
    directory: Option<PublishedPrivateDirectory>,
    repository: PrivateDirectory<'static>,
    repository_id: RepositoryId,
    generation: GenerationId,
    storage_accounting: Arc<Mutex<DurableStorageAccounting>>,
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
    pub(super) serialized_document_bytes: Option<u64>,
    pub(super) search: LexicalIndex,
    pub(super) sources: Vec<RustSourceInput>,
    pub(super) incremental: Option<PreparedIncrementalState>,
    pub(super) operations: Vec<FirstSliceOperationContext>,
    // Drop payload fields before releasing their aggregate admission charge.
    pub(super) memory_reservation: Option<super::RestoredMemoryReservation>,
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
    source_file_catalog: Option<DurableSidecarDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_storage: Option<DurableSourceStorage>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableLogicalSnapshotIdentity {
    version: u16,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    contract_major: u16,
    contract_minor: u16,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
    schema_version: String,
    hash: ContentHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    serialized_document_bytes: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct RestoredLogicalSnapshotIdentity {
    identity: FirstSliceLogicalSnapshotIdentity,
    serialized_document_bytes: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableLogicalSnapshotSidecar {
    payload: String,
    digest: ContentHash,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceStorage {
    version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_digest: Option<ContentHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload_bytes: Option<u64>,
}

pub(super) struct DurableSourceWrite {
    pub(super) newly_written_bytes: u64,
    pub(super) referenced_bytes: u64,
}

pub(super) struct DurablePackedSourceWriter<'prepared> {
    prepared: &'prepared DurablePreparedGeneration,
    sources_directory: PrivateDirectory<'prepared>,
    entries: Vec<DurablePackedSourceEntry>,
    pack_sizes: Vec<u64>,
    pack: Vec<u8>,
    payload_bytes: u64,
    last_file: Option<FileId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableGenerationStorage {
    pub(super) repository: RepositoryId,
    pub(super) generation: GenerationId,
    pub(super) parent: Option<GenerationId>,
    pub(super) unique_bytes: u64,
    pub(super) active: bool,
    pub(super) predecessor: bool,
    pub(super) reclaimable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DurableStorageInventory {
    pub(super) repositories: Vec<DurableRepositoryStorage>,
    pub(super) generations: Vec<DurableGenerationStorage>,
    pub(super) generation_unique_bytes: u64,
    pub(super) active_generation_bytes: u64,
    pub(super) predecessor_generation_bytes: u64,
    pub(super) other_retained_generation_bytes: u64,
    pub(super) source_pool_bytes: u64,
    pub(super) shared_source_bytes: u64,
    pub(super) temporary_bytes: u64,
    pub(super) reclaimable_bytes: u64,
    pub(super) pinned_bytes: u64,
    pub(super) repository_overhead_bytes: u64,
    pub(super) quarantine_bytes: u64,
    pub(super) total_physical_bytes: u64,
    pub(super) available_bytes: u64,
    pub(super) inflight_catalog_reservation_bytes: u64,
    pub(super) inflight_repository_reservation_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableRepositoryStorage {
    pub(super) repository: RepositoryId,
    pub(super) physical_bytes: u64,
    pub(super) active_generation_bytes: u64,
    pub(super) predecessor_generation_bytes: u64,
    pub(super) other_retained_generation_bytes: u64,
    pub(super) source_pool_bytes: u64,
    pub(super) shared_source_bytes: u64,
    pub(super) temporary_bytes: u64,
    pub(super) reclaimable_bytes: u64,
    pub(super) repository_overhead_bytes: u64,
    pub(super) inflight_reservation_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableStorageAdmissionPolicy {
    pub(super) required_catalog_bytes: u64,
    pub(super) required_repository_bytes: u64,
    pub(super) maximum_repository_bytes: u64,
    pub(super) maximum_storage_bytes: u64,
    pub(super) minimum_free_bytes: u64,
    pub(super) repository_amplification: Option<DurableRepositoryAmplificationPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableRepositoryAmplificationPolicy {
    pub(super) examined_source_bytes: u64,
    pub(super) emitted_fact_bytes: u64,
    pub(super) source_factor: u64,
    pub(super) oracle_factor: u64,
    pub(super) retained_generations: u64,
    pub(super) fixed_headroom_bytes: u64,
}

impl DurableRepositoryAmplificationPolicy {
    fn effective_factor(self) -> u64 {
        1_u64
            .saturating_add(self.source_factor)
            .saturating_add(self.oracle_factor)
    }

    fn limit(self, absolute_limit_bytes: u64) -> DurableRepositoryAmplification {
        let effective_factor = self.effective_factor();
        // Source retention and normalized generation output have independent
        // physical representations. Charging oracle growth only against
        // source bytes rejects repositories that legitimately emit dense IR.
        let amplification_limit_bytes = self
            .examined_source_bytes
            .saturating_mul(1_u64.saturating_add(self.source_factor))
            .saturating_add(
                self.emitted_fact_bytes
                    .saturating_mul(self.oracle_factor)
                    .saturating_mul(self.retained_generations),
            )
            .saturating_add(self.fixed_headroom_bytes);
        DurableRepositoryAmplification {
            examined_source_bytes: self.examined_source_bytes,
            emitted_fact_bytes: self.emitted_fact_bytes,
            effective_factor,
            retained_generations: self.retained_generations,
            absolute_limit_bytes,
            amplification_limit_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DurableStorageAdmissionScope {
    RepositoryBudget,
    RepositoryAmplification,
    CatalogBudget,
    FilesystemFreeSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableStorageAdmission {
    pub(super) required_bytes: u64,
    pub(super) observed_bytes: u64,
    pub(super) limit_bytes: u64,
    pub(super) minimum_free_bytes: u64,
    pub(super) admission_margin_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableStorageHeadroom {
    pub(super) repository_bytes: u64,
    pub(super) catalog_bytes: u64,
    pub(super) filesystem_bytes: u64,
    pub(super) admission_bytes: u64,
}

#[derive(Default)]
struct DurableStorageReservations {
    next_id: u64,
    entries: BTreeMap<u64, DurableStorageReservationEntry>,
}

#[derive(Debug, Clone, Copy)]
struct DurableStorageReservationEntry {
    repository: RepositoryId,
    catalog_bytes: u64,
    repository_bytes: u64,
}

pub(super) struct DurableStorageReservation {
    accounting: Arc<Mutex<DurableStorageAccounting>>,
    id: u64,
}

pub(super) struct DurableSealedGeneration {
    prepared: DurablePreparedGeneration,
    repository: RepositoryId,
    materialized_bytes: u64,
    manifest_written_bytes: u64,
    scanned_generation: ScannedGeneration,
}

pub(super) struct DurableStorageAdmittedGeneration {
    sealed: DurableSealedGeneration,
}

impl std::fmt::Debug for DurableStorageReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableStorageReservation")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for DurableStorageReservation {
    fn drop(&mut self) {
        if let Ok(mut accounting) = self.accounting.lock() {
            accounting.reservations.entries.remove(&self.id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableStorageAdmissionFailure {
    pub(super) scope: DurableStorageAdmissionScope,
    pub(super) required_bytes: u64,
    pub(super) observed_bytes: u64,
    pub(super) projected_bytes: u64,
    pub(super) limit_bytes: u64,
    pub(super) minimum_free_bytes: u64,
    pub(super) repository_amplification: Option<DurableRepositoryAmplification>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DurableRepositoryAmplification {
    pub(super) examined_source_bytes: u64,
    pub(super) emitted_fact_bytes: u64,
    pub(super) effective_factor: u64,
    pub(super) retained_generations: u64,
    pub(super) absolute_limit_bytes: u64,
    pub(super) amplification_limit_bytes: u64,
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

struct DurableSourceFileCatalogRef<'catalog> {
    entries: &'catalog [SourceFileCatalogEntry],
}

impl Serialize for DurableSourceFileCatalogRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;

        let mut state = serializer.serialize_struct("DurableSourceFileCatalog", 2)?;
        state.serialize_field("version", &SOURCE_FILE_CATALOG_VERSION)?;
        state.serialize_field("entries", &DurableSourceFileEntries(self.entries))?;
        state.end()
    }
}

struct DurableSourceFileEntries<'catalog>(&'catalog [SourceFileCatalogEntry]);

impl Serialize for DurableSourceFileEntries<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq as _;

        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            let file = entry.file();
            sequence.serialize_element(&DurableSourceFileEntryRef {
                path_identity: LowerHexBytes(entry.path_identity()),
                content_hash: file.content_hash,
                byte_length: file.byte_length,
                language: &file.language,
                generated: file.generated,
                provenance: file.provenance,
            })?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceFileEntryRef<'entry> {
    path_identity: LowerHexBytes<'entry>,
    content_hash: ContentHash,
    byte_length: u64,
    language: &'entry str,
    generated: bool,
    provenance: FactId,
}

struct LowerHexBytes<'bytes>(&'bytes [u8]);

impl fmt::Display for LowerHexBytes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for LowerHexBytes<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

fn decode_path_identity(encoded: &str) -> Result<Vec<u8>, FirstSliceError> {
    let maximum_bytes = MAX_PATH_BYTES
        .checked_add(MAX_PATH_COMPONENTS.saturating_mul(5))
        .ok_or(FirstSliceError::Limits)?;
    let maximum_hex_bytes = maximum_bytes
        .checked_mul(2)
        .ok_or(FirstSliceError::Limits)?;
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) || encoded.len() > maximum_hex_bytes {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(encoded.len() / 2)
        .map_err(|_| FirstSliceError::Limits)?;
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = decode_lower_hex_nibble(pair[0]).ok_or(FirstSliceError::CatalogCorrupt)?;
        let low = decode_lower_hex_nibble(pair[1]).ok_or(FirstSliceError::CatalogCorrupt)?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

const fn decode_lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[derive(Deserialize)]
struct DurableSourceFileCatalogHeader {
    version: u16,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyDurableSourceFileCatalog {
    version: u16,
    entries: Vec<LegacyDurableSourceFileEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyDurableSourceFileEntry {
    claim: FileIdentityClaim,
    locator_encoding: String,
    locator_components: Vec<String>,
    language: String,
    encoding: String,
    generated: bool,
    provenance: FactId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceFileCatalog {
    version: u16,
    entries: Vec<DurableSourceFileEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableSourceFileEntry {
    path_identity: String,
    content_hash: ContentHash,
    byte_length: u64,
    language: String,
    generated: bool,
    provenance: FactId,
}

impl LegacyDurableSourceFileCatalog {
    fn into_catalog(
        self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<SourceFileCatalog, FirstSliceError> {
        if self.version != LEGACY_SOURCE_FILE_CATALOG_VERSION
            || self.entries.len() > MAX_SOURCE_BLOB_ENTRIES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| FirstSliceError::Limits)?;
        for durable in self.entries {
            if durable.claim.repository != repository {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            let locator_encoding = FilePathLocatorEncoding::parse(&durable.locator_encoding)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let path_locator = FilePathLocator::new(locator_encoding, durable.locator_components)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let span = SourceSpan::new(durable.claim.file, 0, durable.claim.byte_length)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let source = SourceRef::new(
                repository,
                generation,
                span,
                durable.claim.content_hash,
                None,
            );
            let file = FileRecord {
                id: durable.claim.file,
                repository,
                generation,
                path: durable.claim.path.clone(),
                path_locator: Some(path_locator),
                content_hash: durable.claim.content_hash,
                byte_length: durable.claim.byte_length,
                language: durable.language,
                encoding: durable.encoding,
                generated: durable.generated,
                provenance: durable.provenance,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            entries.push(
                SourceFileCatalogEntry::new(file, durable.claim)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?,
            );
        }
        SourceFileCatalog::new(entries).map_err(|_| FirstSliceError::CatalogCorrupt)
    }
}

impl DurableSourceFileCatalog {
    fn into_catalog(
        self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<SourceFileCatalog, FirstSliceError> {
        if self.version != SOURCE_FILE_CATALOG_VERSION
            || self.entries.len() > MAX_SOURCE_BLOB_ENTRIES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| FirstSliceError::Limits)?;
        for durable in self.entries {
            let path_identity = decode_path_identity(&durable.path_identity)?;
            let relative = RelativePath::from_identity_bytes(&path_identity)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let file = derive_file(FileIdentity {
                repository,
                path_identity: &path_identity,
            })
            .id();
            let path = fallible_copy_string(relative.as_str())?;
            let claim = FileIdentityClaim {
                file,
                repository,
                path: path.clone(),
                path_identity,
                content_hash: durable.content_hash,
                byte_length: durable.byte_length,
            };
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, durable.byte_length)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?,
                durable.content_hash,
                None,
            );
            let record = FileRecord {
                id: file,
                repository,
                generation,
                path,
                path_locator: Some(relative.to_locator()),
                content_hash: durable.content_hash,
                byte_length: durable.byte_length,
                language: durable.language,
                encoding: "utf-8".to_owned(),
                generated: durable.generated,
                provenance: durable.provenance,
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            entries.push(
                SourceFileCatalogEntry::new(record, claim)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?,
            );
        }
        SourceFileCatalog::new(entries).map_err(|_| FirstSliceError::CatalogCorrupt)
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
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

#[derive(Deserialize)]
struct DurableIncrementalStateHeader {
    version: u16,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyDurableIncrementalState {
    version: u16,
    baseline_files: Vec<DurableBaselineFile>,
    baseline_inputs: Vec<DurableInputFingerprint>,
    analysis_inputs: Vec<DurableInputFingerprint>,
    evidence: FirstSliceIncrementalEvidence,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableIncrementalState {
    version: u16,
    baseline_files: Vec<DurableBaselineFile>,
    // File content and path fingerprints are reconstructed from the same
    // baseline records so large repositories do not persist them three times.
    baseline_context_inputs: Vec<DurableInputFingerprint>,
    analysis_files: DurableAnalysisFileSelection,
    analysis_context_inputs: Vec<DurableInputFingerprint>,
    evidence: FirstSliceIncrementalEvidence,
}

#[derive(Deserialize, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all = "snake_case",
    tag = "selection",
    content = "files"
)]
enum DurableAnalysisFileSelection {
    AllBaseline,
    Explicit(Vec<FileId>),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    serialized_document_bytes: Option<u64>,
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
    MessagePackGzip,
}

#[derive(Clone, Copy)]
enum RecoverySnapshotFormat {
    Json,
    JsonGzip,
    MessagePackGzip,
}

// Keep descriptor validation separate from payload materialization so admission
// and decoding use the same bounded lengths, codec, and generation identity.
struct RecoverySnapshotPlan {
    snapshot_name: &'static str,
    encoded_bytes: u64,
    encoded_digest: ContentHash,
    decoded_bytes: u64,
    decoded_digest: ContentHash,
    serialized_document_bytes: u64,
    format: RecoverySnapshotFormat,
    metadata: GenerationMetadata,
}

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone)]
struct ActivationMarker {
    name: OsString,
    sequence: u64,
    manifest: DurableActivationManifest,
}

struct PublishedActivationMarker {
    marker: ActivationMarker,
    bytes: u64,
}

struct GenerationRestoreRequest<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    activation_sequence: u64,
    global_activation_sequence: Option<u64>,
    published_generation_count: Option<u64>,
    repository_directory: &'a PrivateDirectory<'a>,
    repository_path: &'a Path,
    generation_cache: Option<&'a Arc<Mutex<super::FirstSliceGenerationCache>>>,
}

struct OracleRestoreExpectation {
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    allocated_bytes: u64,
}

struct PersistedSourceReader {
    repository: RepositoryId,
    sources: PrivateDirectory<'static>,
    layout: PersistedSourceLayout,
}

enum PersistedSourceLayout {
    Inline,
    Blobs(PrivateDirectory<'static>),
    Packed {
        index: PackedSourceIndex,
        cached_pack: Mutex<Option<CachedSourcePack>>,
    },
}

#[derive(Clone, Copy)]
enum DurableSourceLayout {
    Inline,
    Blobs,
    Packed(DurableSourceStorage),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurablePackedSourceIndex {
    version: u16,
    payload_bytes: u64,
    pack_bytes: Vec<u64>,
    entries: Vec<DurablePackedSourceEntry>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurablePackedSourceEntry {
    file: FileId,
    digest: ContentHash,
    pack: u32,
    offset: u64,
    bytes: u64,
}

struct PackedSourceIndex {
    pack_bytes: Vec<u64>,
    entries: BTreeMap<FileId, DurablePackedSourceEntry>,
}

struct CachedSourcePack {
    ordinal: u32,
    bytes: Vec<u8>,
}

struct StorageScanBudget {
    visited_entries: usize,
    cancellation: Cancellation,
    #[cfg(test)]
    after_visit: Option<Box<dyn FnMut(usize) + Send>>,
}

#[derive(Clone, Copy)]
enum SourceBlobScan {
    /// Establish exact physical accounting without reading immutable payload bytes.
    AccountPhysicalBytes,
    /// Recompute content identities before integrity-sensitive reporting.
    VerifyContent,
}

#[derive(Clone)]
struct ScannedGeneration {
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    tree_bytes: u64,
    source_blobs: BTreeMap<ContentHash, u64>,
}

#[derive(Clone)]
struct ScannedSourceBlob {
    bytes: u64,
    payload_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VerifiedSourceBlobMetadata {
    payload_bytes: u64,
    modified_ns: u128,
    volume: u64,
    file_index: u64,
}

#[derive(Clone, Default)]
struct ScannedRepositoryStorage {
    repository: Option<RepositoryId>,
    generations: BTreeMap<GenerationId, ScannedGeneration>,
    markers: BTreeMap<u64, ActivationMarker>,
    metadata_bytes: BTreeMap<u64, u64>,
    marker_bytes: BTreeMap<u64, u64>,
    source_blobs: BTreeMap<ContentHash, ScannedSourceBlob>,
    temporary_bytes: u64,
}

struct DurableStorageAccounting {
    reservations: DurableStorageReservations,
    repositories: Option<BTreeMap<RepositoryId, ScannedRepositoryStorage>>,
    quarantine_bytes: u64,
    dirty: bool,
    verified_source_blobs: BTreeMap<(RepositoryId, ContentHash), VerifiedSourceBlobMetadata>,
    #[cfg(test)]
    full_scan_count: u64,
    #[cfg(test)]
    repository_scan_count: u64,
}

impl Default for DurableStorageAccounting {
    fn default() -> Self {
        Self {
            reservations: DurableStorageReservations {
                next_id: 0,
                entries: BTreeMap::new(),
            },
            repositories: None,
            quarantine_bytes: 0,
            dirty: false,
            verified_source_blobs: BTreeMap::new(),
            #[cfg(test)]
            full_scan_count: 0,
            #[cfg(test)]
            repository_scan_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryWriteFailure {
    Cancelled(CancellationReason),
    Limit,
}

struct RecoverySnapshotWriter<'a, W> {
    inner: W,
    hasher: blake3::Hasher,
    bytes: u64,
    limit: u64,
    cancellation: &'a Cancellation,
    failure: Option<RecoveryWriteFailure>,
}

impl<'a, W> RecoverySnapshotWriter<'a, W> {
    fn new(inner: W, limit: u64, cancellation: &'a Cancellation) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            bytes: 0,
            limit,
            cancellation,
            failure: None,
        }
    }

    fn checkpoint(&mut self) -> std::io::Result<()> {
        self.cancellation.check().map_err(|cancelled| {
            self.failure = Some(RecoveryWriteFailure::Cancelled(cancelled.reason()));
            std::io::Error::other("recovery serialization cancelled")
        })
    }

    fn fail_limit<T>(&mut self) -> std::io::Result<T> {
        self.failure = Some(RecoveryWriteFailure::Limit);
        Err(std::io::Error::other(
            "recovery serialization limit exceeded",
        ))
    }
}

fn buffered_recovery_writer<W: std::io::Write>(
    inner: W,
    limit: u64,
    cancellation: &Cancellation,
) -> RecoverySnapshotWriter<'_, BufWriter<W>> {
    RecoverySnapshotWriter::new(
        BufWriter::with_capacity(RECOVERY_WRITE_BUFFER_BYTES, inner),
        limit,
        cancellation,
    )
}

fn content_hash_bytes(bytes: &[u8]) -> ContentHash {
    ContentHash::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn legacy_source_storage() -> DurableSourceStorage {
    DurableSourceStorage {
        version: LEGACY_SOURCE_STORAGE_VERSION,
        files: None,
        packs: None,
        index_bytes: None,
        index_digest: None,
        payload_bytes: None,
    }
}

fn source_storage_layout(
    manifest_version: u16,
    storage: Option<DurableSourceStorage>,
) -> Result<DurableSourceLayout, FirstSliceError> {
    match (manifest_version, storage) {
        (LEGACY_GENERATION_MANIFEST_VERSION, None) => Ok(DurableSourceLayout::Inline),
        (version, Some(storage))
            if matches!(
                version,
                PACKED_GENERATION_MANIFEST_VERSION | GENERATION_MANIFEST_VERSION
            ) && storage.version == LEGACY_SOURCE_STORAGE_VERSION
                && storage.files.is_none()
                && storage.packs.is_none()
                && storage.index_bytes.is_none()
                && storage.index_digest.is_none()
                && storage.payload_bytes.is_none() =>
        {
            Ok(DurableSourceLayout::Blobs)
        }
        (version, Some(storage))
            if matches!(
                version,
                PACKED_GENERATION_MANIFEST_VERSION | GENERATION_MANIFEST_VERSION
            ) && storage.version == SOURCE_STORAGE_VERSION
                && storage.files.is_some()
                && storage.packs.is_some()
                && storage.index_bytes.is_some()
                && storage.index_digest.is_some()
                && storage.payload_bytes.is_some() =>
        {
            Ok(DurableSourceLayout::Packed(storage))
        }
        _ => Err(FirstSliceError::CatalogCorrupt),
    }
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

impl<W: std::io::Write> std::io::Write for RecoverySnapshotWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.checkpoint()?;
        if buffer.is_empty() {
            return Ok(0);
        }
        let checkpoint_bytes = buffer.len().min(RECOVERY_SERIALIZATION_CHECKPOINT_BYTES);
        let checkpoint_bytes_u64 =
            u64::try_from(checkpoint_bytes).map_err(|_| std::io::Error::other("size overflow"))?;
        let next_bytes = self
            .bytes
            .checked_add(checkpoint_bytes_u64)
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        if next_bytes > self.limit {
            return self.fail_limit();
        }
        let written = self.inner.write(&buffer[..checkpoint_bytes])?;
        if written > checkpoint_bytes {
            return Err(std::io::Error::other(
                "recovery writer returned an invalid byte count",
            ));
        }
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
        self.checkpoint()?;
        self.inner.flush()
    }
}

fn recovery_writer_error(
    failures: impl IntoIterator<Item = Option<RecoveryWriteFailure>>,
    fallback: FirstSliceError,
    limit_error: FirstSliceError,
) -> FirstSliceError {
    match failures.into_iter().flatten().next() {
        Some(RecoveryWriteFailure::Cancelled(reason)) => FirstSliceError::Cancelled(reason),
        Some(RecoveryWriteFailure::Limit) => limit_error,
        None => fallback,
    }
}

fn recovery_json_serialized_bytes<T: Serialize>(
    value: &T,
    limit: u64,
    cancellation: &Cancellation,
) -> Result<u64, FirstSliceError> {
    check_cancellation(cancellation)?;
    let mut writer = RecoverySnapshotWriter::new(std::io::sink(), limit, cancellation);
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(recovery_writer_error(
            [writer.failure],
            FirstSliceError::Limits,
            FirstSliceError::CatalogCorrupt,
        ));
    }
    check_cancellation(cancellation)?;
    Ok(writer.bytes)
}

impl LegacyDurableIncrementalState {
    fn into_prepared(
        self,
        cancellation: &Cancellation,
    ) -> Result<PreparedIncrementalState, FirstSliceError> {
        if self.version != LEGACY_INCREMENTAL_STATE_VERSION {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let metadata = restore_metadata_baseline(&self.baseline_files, cancellation)?;
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

impl DurableIncrementalState {
    fn from_prepared(
        state: &PreparedIncrementalState,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        let mut baseline_files = Vec::new();
        baseline_files
            .try_reserve_exact(state.baseline.metadata().len())
            .map_err(|_| FirstSliceError::Retention)?;
        for file in state.baseline.metadata().files() {
            check_cancellation(cancellation)?;
            let descriptor = file.descriptor();
            let metadata = descriptor.metadata();
            baseline_files.push(DurableBaselineFile {
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
            });
        }
        let (baseline_file_ids, baseline_context_inputs) =
            compact_incremental_inputs(state.baseline.inputs(), &baseline_files, cancellation)?;
        if !file_ids_cover_baseline(&baseline_file_ids, &baseline_files) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        drop(baseline_file_ids);
        let (analysis_file_ids, analysis_context_inputs) =
            compact_incremental_inputs(&state.inputs, &baseline_files, cancellation)?;
        let analysis_files = if file_ids_cover_baseline(&analysis_file_ids, &baseline_files) {
            DurableAnalysisFileSelection::AllBaseline
        } else {
            DurableAnalysisFileSelection::Explicit(analysis_file_ids)
        };
        validate_incremental_evidence(&state.evidence)?;
        Ok(Self {
            version: INCREMENTAL_STATE_VERSION,
            baseline_files,
            baseline_context_inputs,
            analysis_files,
            analysis_context_inputs,
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
        validate_durable_baseline_order(&self.baseline_files)?;
        let metadata = restore_metadata_baseline(&self.baseline_files, cancellation)?;
        let baseline_inputs = restore_compact_input_snapshot(
            self.baseline_files.iter().map(|file| file.file),
            self.baseline_files.len(),
            &self.baseline_files,
            self.baseline_context_inputs,
            cancellation,
        )?;
        let analysis_inputs = match self.analysis_files {
            DurableAnalysisFileSelection::AllBaseline => restore_compact_input_snapshot(
                self.baseline_files.iter().map(|file| file.file),
                self.baseline_files.len(),
                &self.baseline_files,
                self.analysis_context_inputs,
                cancellation,
            )?,
            DurableAnalysisFileSelection::Explicit(files) => {
                if files.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let file_count = files.len();
                restore_compact_input_snapshot(
                    files,
                    file_count,
                    &self.baseline_files,
                    self.analysis_context_inputs,
                    cancellation,
                )?
            }
        };
        validate_incremental_evidence(&self.evidence)?;
        Ok(PreparedIncrementalState {
            baseline: IncrementalDiscoveryBaseline::from_validated_parts(metadata, baseline_inputs),
            inputs: analysis_inputs,
            evidence: self.evidence,
        })
    }
}

fn compact_incremental_inputs(
    snapshot: &InputSnapshot,
    baseline_files: &[DurableBaselineFile],
    cancellation: &Cancellation,
) -> Result<(Vec<FileId>, Vec<DurableInputFingerprint>), FirstSliceError> {
    let mut content_files = Vec::new();
    let mut path_files = Vec::new();
    content_files
        .try_reserve_exact(baseline_files.len())
        .map_err(|_| FirstSliceError::Retention)?;
    path_files
        .try_reserve_exact(baseline_files.len())
        .map_err(|_| FirstSliceError::Retention)?;
    let mut context_inputs = Vec::new();
    for input in snapshot.iter() {
        check_cancellation(cancellation)?;
        let (file, expected, target) = match input.key() {
            InputKey::FileContent(file) => (
                file,
                durable_baseline_file(baseline_files, file)?.content_hash,
                &mut content_files,
            ),
            InputKey::FilePath(file) => (
                file,
                durable_baseline_file(baseline_files, file)?.path_hash,
                &mut path_files,
            ),
            key => {
                context_inputs.push(DurableInputFingerprint {
                    key,
                    value: input.value(),
                });
                continue;
            }
        };
        if input.value() != expected {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        target.push(file);
    }
    if content_files != path_files {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok((content_files, context_inputs))
}

fn durable_baseline_file(
    baseline_files: &[DurableBaselineFile],
    file: FileId,
) -> Result<&DurableBaselineFile, FirstSliceError> {
    baseline_files
        .binary_search_by_key(&file, |candidate| candidate.file)
        .ok()
        .map(|index| &baseline_files[index])
        .ok_or(FirstSliceError::CatalogCorrupt)
}

fn file_ids_cover_baseline(file_ids: &[FileId], baseline_files: &[DurableBaselineFile]) -> bool {
    file_ids.len() == baseline_files.len()
        && file_ids
            .iter()
            .zip(baseline_files)
            .all(|(file, baseline)| *file == baseline.file)
}

fn validate_durable_baseline_order(
    baseline_files: &[DurableBaselineFile],
) -> Result<(), FirstSliceError> {
    if baseline_files
        .windows(2)
        .any(|pair| pair[0].file >= pair[1].file)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

fn restore_metadata_baseline(
    durable_files: &[DurableBaselineFile],
    cancellation: &Cancellation,
) -> Result<MetadataBaseline, FirstSliceError> {
    let reconcile_limits = ReconcileLimits::new(durable_files.len().max(1))
        .map_err(|error| map_incremental_error(error, cancellation))?;
    let mut baseline_files = Vec::new();
    baseline_files
        .try_reserve_exact(durable_files.len())
        .map_err(|_| FirstSliceError::Retention)?;
    for file in durable_files {
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
    MetadataBaseline::new(baseline_files, reconcile_limits, cancellation)
        .map_err(|error| map_incremental_error(error, cancellation))
}

fn restore_compact_input_snapshot(
    file_ids: impl IntoIterator<Item = FileId>,
    file_count: usize,
    baseline_files: &[DurableBaselineFile],
    context_inputs: Vec<DurableInputFingerprint>,
    cancellation: &Cancellation,
) -> Result<InputSnapshot, FirstSliceError> {
    if context_inputs
        .iter()
        .any(|input| matches!(input.key, InputKey::FileContent(_) | InputKey::FilePath(_)))
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let expected = file_count
        .checked_mul(2)
        .and_then(|count| count.checked_add(context_inputs.len()))
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    let mut inputs = Vec::new();
    inputs
        .try_reserve_exact(expected)
        .map_err(|_| FirstSliceError::Retention)?;
    for file in file_ids {
        check_cancellation(cancellation)?;
        let baseline = durable_baseline_file(baseline_files, file)?;
        inputs.extend([
            DurableInputFingerprint {
                key: InputKey::FileContent(file),
                value: baseline.content_hash,
            },
            DurableInputFingerprint {
                key: InputKey::FilePath(file),
                value: baseline.path_hash,
            },
        ]);
    }
    inputs.extend(context_inputs);
    if inputs.len() != expected {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    restore_input_snapshot(inputs, cancellation)
}

#[cfg(test)]
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
    let limits = PlanningLimits::new(inputs.len().max(1), 1, 1, 1, 1)
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
    let structural_inputs = evidence
        .parsed_files
        .checked_add(evidence.reused_parser_artifacts)
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    // Unsupported encodings can produce a bounded document without either a
    // successful parse or a reusable artifact, so only the opposite delta
    // proves that lowering was skipped through normalized rebind.
    let skipped_lowering = structural_inputs.saturating_sub(evidence.lowered_files);
    let planned_reused_lowering_files = evidence
        .planned_fact_work
        .iter()
        .filter(|work| {
            work.disposition == super::FirstSliceFactWorkDisposition::Reuse
                && work.cause == super::FirstSliceFactWorkCause::CompleteDependencyMatch
                && work.provider_pass == super::LOWERING_PASS_ID
        })
        .map(|work| work.files)
        .max()
        .unwrap_or(0);
    if evidence.input_changes.len() > 9
        || evidence.file_changes.len() > 6
        || evidence.invalidated_domains.len() > 9
        || skipped_lowering > 0
            && evidence.reused_normalized_facts == 0
            && planned_reused_lowering_files < skipped_lowering
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
        | super::FirstSliceBuildStrategy::DependencyDirected
        | super::FirstSliceBuildStrategy::CleanRebuild => evidence.fallback_reason.is_none(),
        super::FirstSliceBuildStrategy::ConservativeRepositoryRebuild => {
            evidence.fallback_reason.is_some()
        }
    };
    let clean_rebuild_evidence_is_consistent =
        evidence.strategy != super::FirstSliceBuildStrategy::CleanRebuild
            || evidence.reused_parser_artifacts == 0
                && evidence.reused_parser_artifact_bytes == 0
                && evidence.reused_normalized_facts == 0
                && evidence.planned_fact_work.iter().all(|work| {
                    work.disposition == super::FirstSliceFactWorkDisposition::Rebuild
                        && work.cause == super::FirstSliceFactWorkCause::UserRequestedCleanRebuild
                })
                && evidence.normalized_fact_work.iter().all(|work| {
                    work.cause != super::FirstSliceFactWorkCause::CompleteDependencyMatch
                });
    if input_classes.len() != evidence.input_changes.len()
        || file_kinds.len() != evidence.file_changes.len()
        || domains.len() != evidence.invalidated_domains.len()
        || evidence
            .input_changes
            .iter()
            .any(|change| change.inputs == 0)
        || evidence.file_changes.iter().any(|change| change.files == 0)
        || !fallback_matches_strategy
        || !clean_rebuild_evidence_is_consistent
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

impl DurableCatalog {
    pub(super) fn open(
        state_root: &Path,
        maximum_generations_per_repository: usize,
        maximum_repositories: usize,
    ) -> Result<Self, FirstSliceError> {
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
            maximum_repositories,
            staging_bytes: Arc::new(AtomicU64::new(0)),
            storage_accounting: Arc::new(Mutex::new(DurableStorageAccounting::default())),
            generation_cache: None,
        })
    }

    pub(super) fn with_generation_cache(
        mut self,
        cache: Arc<Mutex<super::FirstSliceGenerationCache>>,
    ) -> Self {
        self.generation_cache = Some(cache);
        self
    }

    pub(super) fn begin_generation(
        &self,
        repository: RepositoryId,
        generation: GenerationId,
    ) -> Result<DurablePreparedGeneration, FirstSliceError> {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let repository_name = repository.to_string();
        let repository_directory =
            ensure_private_directory(self.repositories.capability(), OsStr::new(&repository_name))?;
        if !accounting.dirty
            && let Some(repositories) = accounting.repositories.as_mut()
        {
            repositories
                .entry(repository)
                .or_insert_with(|| ScannedRepositoryStorage {
                    repository: Some(repository),
                    ..ScannedRepositoryStorage::default()
                });
        }
        let repository_path = self.repositories_path.join(&repository_name);
        let staging_name = random_staging_name(generation)?;
        let staging =
            PrivateDirectory::create(repository_directory.capability(), OsStr::new(&staging_name))
                .map_err(|_| FirstSliceError::Catalog)?;
        let staging_path = repository_path.join(&staging_name);
        Ok(DurablePreparedGeneration {
            staging: Some(staging),
            staging_path,
            repository: Some(repository_directory),
            repository_id: repository,
            generation,
            staging_bytes: Arc::clone(&self.staging_bytes),
            accounted_bytes: AtomicU64::new(0),
            incremental_state: Mutex::new(None),
            source_file_catalog: Mutex::new(None),
            source_storage: Mutex::new(None),
            created_source_blobs: Mutex::new(BTreeSet::new()),
            storage_accounting: Arc::clone(&self.storage_accounting),
        })
    }

    pub(super) fn ensure_staging_capacity(
        &self,
        repository: RepositoryId,
        policy: DurableStorageAdmissionPolicy,
    ) -> Result<
        Result<
            (DurableStorageAdmission, DurableStorageReservation),
            DurableStorageAdmissionFailure,
        >,
        FirstSliceError,
    > {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        let inventory = self.inventory_for_admission(&mut accounting, available_bytes)?;
        let admission = match check_storage_admission(&inventory, repository, policy) {
            Ok(admission) => admission,
            Err(failure) => return Ok(Err(failure)),
        };
        accounting.reservations.next_id = accounting
            .reservations
            .next_id
            .checked_add(1)
            .ok_or(FirstSliceError::Limits)?;
        let id = accounting.reservations.next_id;
        if accounting
            .reservations
            .entries
            .insert(
                id,
                DurableStorageReservationEntry {
                    repository,
                    catalog_bytes: policy.required_catalog_bytes,
                    repository_bytes: policy.required_repository_bytes,
                },
            )
            .is_some()
        {
            return Err(FirstSliceError::Retention);
        }
        Ok(Ok((
            admission,
            DurableStorageReservation {
                accounting: Arc::clone(&self.storage_accounting),
                id,
            },
        )))
    }

    pub(super) fn resize_staging_reservation(
        &self,
        reservation: &DurableStorageReservation,
        policy: DurableStorageAdmissionPolicy,
    ) -> Result<Result<DurableStorageAdmission, DurableStorageAdmissionFailure>, FirstSliceError>
    {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        if accounting.dirty || accounting.repositories.is_none() {
            return Err(FirstSliceError::Retention);
        }
        let current = accounting
            .reservations
            .entries
            .remove(&reservation.id)
            .ok_or(FirstSliceError::Retention)?;
        let inventory = match self.inventory_for_admission(&mut accounting, available_bytes) {
            Ok(inventory) => inventory,
            Err(error) => {
                accounting
                    .reservations
                    .entries
                    .insert(reservation.id, current);
                return Err(error);
            }
        };
        let result = check_storage_admission(&inventory, current.repository, policy);
        accounting.reservations.entries.insert(
            reservation.id,
            DurableStorageReservationEntry {
                repository: current.repository,
                catalog_bytes: result
                    .as_ref()
                    .map_or(current.catalog_bytes, |_| policy.required_catalog_bytes),
                repository_bytes: result.as_ref().map_or(current.repository_bytes, |_| {
                    policy.required_repository_bytes
                }),
            },
        );
        Ok(result)
    }

    pub(super) fn finalize_repository_capacity(
        &self,
        reservation: &DurableStorageReservation,
        sealed: DurableSealedGeneration,
        policy: DurableStorageAdmissionPolicy,
    ) -> Result<
        Result<
            (DurableStorageAdmission, DurableStorageAdmittedGeneration),
            DurableStorageAdmissionFailure,
        >,
        FirstSliceError,
    > {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        let current = accounting
            .reservations
            .entries
            .remove(&reservation.id)
            .ok_or(FirstSliceError::Retention)?;
        if current.repository != sealed.repository || current.repository_bytes != 0 {
            accounting
                .reservations
                .entries
                .insert(reservation.id, current);
            return Err(FirstSliceError::Retention);
        }
        if accounting.dirty || accounting.repositories.is_none() {
            accounting
                .reservations
                .entries
                .insert(reservation.id, current);
            return Err(FirstSliceError::Retention);
        }
        let repository_directory = sealed.prepared.repository();
        let mut budget = StorageScanBudget::new();
        let replacement = match scan_repository_storage(
            sealed.repository,
            repository_directory,
            &mut budget,
            &mut accounting.verified_source_blobs,
            SourceBlobScan::AccountPhysicalBytes,
        ) {
            Ok(replacement) => replacement,
            Err(error) => {
                accounting.dirty = true;
                accounting
                    .reservations
                    .entries
                    .insert(reservation.id, current);
                return Err(error);
            }
        };
        let live_source_blobs = replacement
            .source_blobs
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        #[cfg(test)]
        {
            accounting.repository_scan_count = accounting.repository_scan_count.saturating_add(1);
        }
        let previous = accounting
            .repositories
            .as_mut()
            .ok_or(FirstSliceError::Retention)?
            .insert(sealed.repository, replacement);
        let inventory = match inventory_from_accounting(&accounting, available_bytes) {
            Ok(inventory) => inventory,
            Err(error) => {
                restore_repository_accounting(&mut accounting, sealed.repository, previous);
                accounting.dirty = true;
                accounting
                    .reservations
                    .entries
                    .insert(reservation.id, current);
                return Err(error);
            }
        };
        let observed_repository_bytes = inventory
            .repositories
            .iter()
            .find(|repository| repository.repository == sealed.repository)
            .map_or(0, |repository| repository.physical_bytes);
        let catalog_remaining_bytes = current
            .catalog_bytes
            .checked_sub(sealed.materialized_bytes)
            .filter(|bytes| *bytes >= policy.required_repository_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let observed_before_candidate = observed_repository_bytes
            .checked_sub(sealed.materialized_bytes)
            .ok_or(FirstSliceError::Retention)?;
        let candidate_bytes = sealed
            .materialized_bytes
            .checked_add(policy.required_repository_bytes)
            .ok_or(FirstSliceError::Limits)?;
        let other_repository_reservations =
            accounting
                .reservations
                .entries
                .values()
                .fold(0_u64, |total, entry| {
                    if entry.repository == sealed.repository {
                        total.saturating_add(entry.repository_bytes)
                    } else {
                        total
                    }
                });
        let projected_repository_bytes = observed_repository_bytes
            .saturating_add(other_repository_reservations)
            .saturating_add(policy.required_repository_bytes);
        let repository_amplification = policy
            .repository_amplification
            .map(|amplification| amplification.limit(policy.maximum_repository_bytes));
        let (repository_limit_bytes, repository_scope) = repository_amplification.map_or_else(
            || {
                (
                    policy.maximum_repository_bytes,
                    DurableStorageAdmissionScope::RepositoryBudget,
                )
            },
            |amplification| {
                if policy.maximum_repository_bytes <= amplification.amplification_limit_bytes {
                    (
                        policy.maximum_repository_bytes,
                        DurableStorageAdmissionScope::RepositoryBudget,
                    )
                } else {
                    (
                        amplification.amplification_limit_bytes,
                        DurableStorageAdmissionScope::RepositoryAmplification,
                    )
                }
            },
        );
        if projected_repository_bytes > repository_limit_bytes {
            restore_repository_accounting(&mut accounting, sealed.repository, previous);
            accounting
                .reservations
                .entries
                .insert(reservation.id, current);
            return Ok(Err(DurableStorageAdmissionFailure {
                scope: repository_scope,
                required_bytes: candidate_bytes,
                observed_bytes: observed_before_candidate
                    .saturating_add(other_repository_reservations),
                projected_bytes: projected_repository_bytes,
                limit_bytes: repository_limit_bytes,
                minimum_free_bytes: policy.minimum_free_bytes,
                repository_amplification,
            }));
        }
        let catalog_policy = DurableStorageAdmissionPolicy {
            required_catalog_bytes: catalog_remaining_bytes,
            required_repository_bytes: 0,
            maximum_repository_bytes: u64::MAX,
            maximum_storage_bytes: policy.maximum_storage_bytes,
            minimum_free_bytes: policy.minimum_free_bytes,
            repository_amplification: None,
        };
        if let Err(failure) = check_storage_admission(&inventory, sealed.repository, catalog_policy)
        {
            restore_repository_accounting(&mut accounting, sealed.repository, previous);
            accounting
                .reservations
                .entries
                .insert(reservation.id, current);
            return Ok(Err(failure));
        }
        accounting.reservations.entries.insert(
            reservation.id,
            DurableStorageReservationEntry {
                repository: current.repository,
                catalog_bytes: catalog_remaining_bytes,
                repository_bytes: policy.required_repository_bytes,
            },
        );
        accounting
            .verified_source_blobs
            .retain(|(repository, digest), _| {
                *repository != sealed.repository || live_source_blobs.contains(digest)
            });
        Ok(Ok((
            DurableStorageAdmission {
                required_bytes: candidate_bytes,
                observed_bytes: observed_before_candidate
                    .saturating_add(other_repository_reservations),
                limit_bytes: repository_limit_bytes,
                minimum_free_bytes: policy.minimum_free_bytes,
                admission_margin_bytes: repository_limit_bytes
                    .saturating_sub(projected_repository_bytes),
            },
            DurableStorageAdmittedGeneration { sealed },
        )))
    }

    pub(super) fn release_staging_reservation(
        &self,
        reservation: DurableStorageReservation,
    ) -> Result<(), FirstSliceError> {
        drop(reservation);
        Ok(())
    }

    pub(super) fn storage_inventory(&self) -> Result<DurableStorageInventory, FirstSliceError> {
        self.scan_storage_inventory(SourceBlobScan::VerifyContent, StorageScanBudget::new())
    }

    pub(super) fn reconcile_storage_inventory(
        &self,
        cancellation: &Cancellation,
    ) -> Result<DurableStorageInventory, FirstSliceError> {
        self.scan_storage_inventory(
            SourceBlobScan::AccountPhysicalBytes,
            StorageScanBudget::with_cancellation(cancellation),
        )
    }

    fn scan_storage_inventory(
        &self,
        source_blob_scan: SourceBlobScan,
        mut budget: StorageScanBudget,
    ) -> Result<DurableStorageInventory, FirstSliceError> {
        budget.check()?;
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        budget.check()?;
        // A cancelled reconciliation must not expose a partially refreshed cache.
        accounting.dirty = true;
        accounting.verified_source_blobs.clear();
        let scanned = scan_storage_state_with_budget(
            &self.repositories,
            &self.quarantine,
            self.maximum_repositories,
            &mut accounting.verified_source_blobs,
            source_blob_scan,
            &mut budget,
        );
        let (repositories, quarantine_bytes) = match scanned {
            Ok(scanned) => scanned,
            Err(error) => {
                accounting.dirty = true;
                return Err(error);
            }
        };
        #[cfg(test)]
        {
            accounting.full_scan_count = accounting.full_scan_count.saturating_add(1);
        }
        let built = build_storage_inventory(
            repositories.values().cloned().collect(),
            quarantine_bytes,
            available_bytes,
        );
        let mut inventory = match built {
            Ok(inventory) => inventory,
            Err(error) => {
                accounting.dirty = true;
                return Err(error);
            }
        };
        apply_storage_reservations(&mut inventory, &accounting.reservations)?;
        budget.check()?;
        if accounting.reservations.entries.is_empty() {
            accounting.repositories = Some(repositories);
            accounting.quarantine_bytes = quarantine_bytes;
            accounting.dirty = false;
        }
        Ok(inventory)
    }

    pub(super) fn storage_inventory_cached(
        &self,
    ) -> Result<Option<DurableStorageInventory>, FirstSliceError> {
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        let accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        if accounting.dirty || accounting.repositories.is_none() {
            return Ok(None);
        }
        inventory_from_accounting(&accounting, available_bytes).map(Some)
    }

    fn inventory_for_admission(
        &self,
        accounting: &mut DurableStorageAccounting,
        available_bytes: u64,
    ) -> Result<DurableStorageInventory, FirstSliceError> {
        if accounting.dirty || accounting.repositories.is_none() {
            // An in-flight reservation may already be materializing bytes that a
            // scan cannot attribute to that reservation. Refuse instead of
            // combining an ambiguous scan with the live reservation ledger.
            if !accounting.reservations.entries.is_empty() {
                return Err(FirstSliceError::Retention);
            }
            accounting.dirty = true;
            accounting.verified_source_blobs.clear();
            let (repositories, quarantine_bytes) = scan_storage_state(
                &self.repositories,
                &self.quarantine,
                self.maximum_repositories,
                &mut accounting.verified_source_blobs,
                SourceBlobScan::AccountPhysicalBytes,
            )?;
            let mut inventory = build_storage_inventory(
                repositories.values().cloned().collect(),
                quarantine_bytes,
                available_bytes,
            )?;
            apply_storage_reservations(&mut inventory, &accounting.reservations)?;
            accounting.repositories = Some(repositories);
            accounting.quarantine_bytes = quarantine_bytes;
            accounting.dirty = false;
            #[cfg(test)]
            {
                accounting.full_scan_count = accounting.full_scan_count.saturating_add(1);
            }
            return Ok(inventory);
        }
        inventory_from_accounting(accounting, available_bytes)
    }

    fn reconcile_repository_accounting(
        &self,
        accounting: &mut DurableStorageAccounting,
        repository: RepositoryId,
        directory: &PrivateDirectory<'_>,
    ) -> Result<(), FirstSliceError> {
        if accounting.dirty || accounting.repositories.is_none() {
            if !accounting.reservations.entries.is_empty() {
                return Err(FirstSliceError::Retention);
            }
            accounting.dirty = true;
            accounting.verified_source_blobs.clear();
            let (repositories, quarantine_bytes) = scan_storage_state(
                &self.repositories,
                &self.quarantine,
                self.maximum_repositories,
                &mut accounting.verified_source_blobs,
                SourceBlobScan::AccountPhysicalBytes,
            )?;
            let available_bytes = fs2::available_space(&self.repositories_path)
                .map_err(|_| FirstSliceError::Catalog)?;
            build_storage_inventory(
                repositories.values().cloned().collect(),
                quarantine_bytes,
                available_bytes,
            )?;
            accounting.repositories = Some(repositories);
            accounting.quarantine_bytes = quarantine_bytes;
            accounting.dirty = false;
            #[cfg(test)]
            {
                accounting.full_scan_count = accounting.full_scan_count.saturating_add(1);
            }
            return Ok(());
        }
        let mut budget = StorageScanBudget::new();
        let scanned = scan_repository_storage(
            repository,
            directory,
            &mut budget,
            &mut accounting.verified_source_blobs,
            SourceBlobScan::AccountPhysicalBytes,
        )?;
        let live_source_blobs = scanned
            .source_blobs
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let previous = accounting
            .repositories
            .as_mut()
            .ok_or(FirstSliceError::Retention)?
            .insert(repository, scanned);
        let available_bytes =
            fs2::available_space(&self.repositories_path).map_err(|_| FirstSliceError::Catalog)?;
        if let Err(error) = inventory_from_accounting(accounting, available_bytes) {
            restore_repository_accounting(accounting, repository, previous);
            accounting.dirty = true;
            return Err(error);
        }
        accounting
            .verified_source_blobs
            .retain(|(candidate, digest), _| {
                *candidate != repository || live_source_blobs.contains(digest)
            });
        #[cfg(test)]
        {
            accounting.repository_scan_count = accounting.repository_scan_count.saturating_add(1);
        }
        Ok(())
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
        let source_layout = source_storage_layout(manifest.version, manifest.source_storage)?;
        PersistedSourceReader::open(&repository, &generation, file.repository, source_layout)?
            .read(file, cancellation)
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

    pub(super) fn active_restore_targets(
        &self,
    ) -> Result<Vec<FirstSliceRecoveryTarget>, FirstSliceError> {
        let repository_names = private_entry_names(&self.repositories)?;
        if repository_names.len() > self.maximum_repositories {
            return Err(FirstSliceError::Retention);
        }
        let mut targets = Vec::new();
        let mut global_activation_generations = BTreeMap::new();
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
            let mut identity_candidates = BTreeMap::new();
            for entry_name in private_entry_names(&repository)? {
                let entry_text = entry_name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
                let Some((sequence, generation)) = parse_activation_name(entry_text) else {
                    continue;
                };
                let marker = read_activation_marker(&repository, entry_name, sequence, generation)?;
                if let Some(global_sequence) = marker.manifest.global_activation_sequence
                    && global_activation_generations
                        .insert(global_sequence, generation)
                        .is_some()
                {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let ordering = marker
                    .manifest
                    .global_activation_sequence
                    .unwrap_or(marker.sequence);
                let published_generation_count = marker
                    .manifest
                    .published_generation_count
                    .unwrap_or(marker.sequence);
                identity_candidates
                    .entry(generation)
                    .and_modify(|current: &mut (u64, u64)| {
                        *current = (*current).max((ordering, marker.sequence));
                    })
                    .or_insert((ordering, marker.sequence));
                if newest.is_none_or(|(current, _, _, _)| ordering > current) {
                    newest = Some((
                        ordering,
                        marker.sequence,
                        published_generation_count,
                        generation,
                    ));
                }
            }
            if let Some((ordering, sequence, published_generation_count, generation)) = newest {
                let mut identity_candidates = identity_candidates.into_iter().collect::<Vec<_>>();
                identity_candidates.sort_unstable_by_key(|(generation, ordering)| {
                    (Reverse(*ordering), *generation)
                });
                let mut root_identity = None;
                // Payload recovery may quarantine malformed generations. Every readable
                // predecessor must still agree on the repository binding before publication.
                for (candidate, _) in identity_candidates {
                    let Some(identity) =
                        read_generation_bootstrap_identity(&repository, repository_id, candidate)?
                    else {
                        continue;
                    };
                    if root_identity.is_some_and(|current| current != identity) {
                        return Err(FirstSliceError::CatalogCorrupt);
                    }
                    root_identity = Some(identity);
                }
                let root_identity = root_identity.ok_or(FirstSliceError::CatalogCorrupt)?;
                targets.push((
                    ordering,
                    sequence,
                    FirstSliceRecoveryTarget {
                        repository: repository_id,
                        generation,
                        root_identity,
                        activation_sequence: sequence,
                        global_activation_sequence: ordering,
                        published_generation_count,
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

    pub(super) fn restore_exact_generation(
        &self,
        repository_id: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<RestoredGeneration, FirstSliceError> {
        check_cancellation(cancellation)?;
        let repository_name = repository_id.to_string();
        let repository =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let repository_path = self.repositories_path.join(&repository_name);
        let mut selected = None;
        let mut metadata_names = BTreeMap::new();
        for entry_name in private_entry_names(&repository)? {
            check_cancellation(cancellation)?;
            let entry_text = entry_name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
            if let Some((sequence, candidate)) = parse_activation_name(entry_text) {
                let marker = read_activation_marker(&repository, entry_name, sequence, candidate)?;
                if candidate == generation
                    && selected
                        .as_ref()
                        .is_none_or(|current: &ActivationMarker| current.sequence < sequence)
                {
                    selected = Some(marker);
                }
            } else if let Some(sequence) = parse_metadata_name(entry_text)
                && metadata_names.insert(sequence, entry_name).is_some()
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let marker = selected.ok_or(FirstSliceError::GenerationNotFound)?;
        let mut restored = restore_generation(
            GenerationRestoreRequest {
                repository: repository_id,
                generation,
                activation_sequence: marker.sequence,
                global_activation_sequence: marker.manifest.global_activation_sequence,
                published_generation_count: marker.manifest.published_generation_count,
                repository_directory: &repository,
                repository_path: &repository_path,
                // Exact lazy rehydration already holds the cache lock and its
                // reload reservation; reacquiring either would deadlock.
                generation_cache: None,
            },
            cancellation,
        )?;
        if let Some((sequence, name)) = metadata_names.last_key_value() {
            let metadata = read_repository_metadata(repository_id, &repository, name, *sequence)?;
            restored.root_path = metadata.root_path;
            restored.alias = metadata.alias;
            restored.metadata_sequence = metadata.sequence;
        }
        restored.operations = marker
            .manifest
            .operation
            .map(FirstSliceOperationContext::from)
            .into_iter()
            .collect();
        Ok(restored)
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

    pub(super) fn restore_progressively(
        &self,
        cancellation: &Cancellation,
        install: impl FnMut(Vec<RestoredGeneration>) -> Result<(), FirstSliceError>,
    ) -> Result<(), FirstSliceError> {
        self.restore_each_repository_with_policy(
            self.maximum_generations_per_repository,
            &BTreeSet::new(),
            true,
            true,
            cancellation,
            install,
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
        let mut restored = Vec::new();
        self.restore_each_repository_with_policy(
            maximum_generations_per_repository,
            excluded,
            compact,
            repair,
            cancellation,
            |mut repository_generations| {
                restored
                    .try_reserve(repository_generations.len())
                    .map_err(|_| FirstSliceError::Retention)?;
                restored.append(&mut repository_generations);
                Ok(())
            },
        )?;
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

    fn restore_each_repository_with_policy(
        &self,
        maximum_generations_per_repository: usize,
        excluded: &BTreeSet<GenerationId>,
        compact: bool,
        repair: bool,
        cancellation: &Cancellation,
        mut visit: impl FnMut(Vec<RestoredGeneration>) -> Result<(), FirstSliceError>,
    ) -> Result<(), FirstSliceError> {
        let policy = RestorePolicy {
            maximum_generations: maximum_generations_per_repository,
            excluded,
            compact,
            repair,
        };
        check_cancellation(cancellation)?;
        let repository_names = private_entry_names(&self.repositories)?;
        if repository_names.len() > self.maximum_repositories {
            return Err(FirstSliceError::Retention);
        }
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
            let repository_generations = self.restore_repository(
                repository_id,
                &repository,
                &repository_path,
                &policy,
                cancellation,
            )?;
            visit(repository_generations)?;
        }
        check_cancellation(cancellation)?;
        Ok(())
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
        let repository_directory =
            PrivateDirectory::open(self.repositories.capability(), OsStr::new(&repository_name))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let generation_name = generation.to_string();
        let _generation = PrivateDirectory::open(
            repository_directory.capability(),
            OsStr::new(&generation_name),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        validate_activation_accounting(
            &accounting,
            repository,
            generation,
            repository_activation_sequence,
        )?;
        let published = publish_activation_marker(
            &repository_directory,
            generation,
            repository_activation_sequence,
            global_activation_sequence,
            published_generation_count,
            operation,
        );
        match published {
            Ok(published) => {
                if let Err(error) = account_activation_marker(
                    &mut accounting,
                    repository,
                    published.marker,
                    published.bytes,
                ) {
                    accounting.dirty = true;
                    Err(error)
                } else {
                    Ok(published.bytes)
                }
            }
            Err(error) => {
                accounting.dirty = true;
                Err(error)
            }
        }
    }

    pub(super) fn write_repository_metadata(
        &self,
        metadata: DurableRepositoryMetadata,
    ) -> Result<u64, FirstSliceError> {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let result = (|| {
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
            if accounting.dirty || accounting.repositories.is_none() {
                self.reconcile_repository_accounting(
                    &mut accounting,
                    metadata.repository,
                    &repository,
                )?;
            }
            let staging_name = random_metadata_staging_name(metadata.sequence)?;
            let staging =
                PrivateDirectory::create(repository.capability(), OsStr::new(&staging_name))
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
            let written_bytes = u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?;
            if let Some(repository) = accounting
                .repositories
                .as_mut()
                .and_then(|repositories| repositories.get_mut(&metadata.repository))
            {
                repository.metadata_bytes.clear();
                repository
                    .metadata_bytes
                    .insert(metadata.sequence, written_bytes);
            } else {
                // The metadata publication is already durable. Preserve its
                // successful contract and force later accounting to reconcile.
                accounting.dirty = true;
            }
            Ok(written_bytes)
        })();
        if result.is_err() {
            accounting.dirty = true;
        }
        result
    }

    pub(super) fn remove_repository(
        &self,
        repository: RepositoryId,
    ) -> Result<(), FirstSliceError> {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let result = (|| {
            let directory = PrivateDirectory::open(
                self.repositories.capability(),
                OsStr::new(&repository.to_string()),
            )
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            directory.remove().map_err(|_| FirstSliceError::Catalog)?;
            self.repositories
                .sync_all()
                .map_err(|_| FirstSliceError::Catalog)?;
            if let Some(repositories) = accounting.repositories.as_mut() {
                repositories.remove(&repository);
            }
            accounting
                .verified_source_blobs
                .retain(|(candidate, _), _| *candidate != repository);
            Ok(())
        })();
        if result.is_err() {
            accounting.dirty = true;
        }
        result
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
            if !staging_names.is_empty() {
                mark_storage_accounting_dirty(&self.storage_accounting);
            }
            for staging_name in staging_names {
                PrivateDirectory::open(repository.capability(), &staging_name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?
                    .remove()
                    .map_err(|_| FirstSliceError::Catalog)?;
            }
        }

        if markers.is_empty() {
            if policy.repair {
                mark_storage_accounting_dirty(&self.storage_accounting);
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
                    generation_cache: self.generation_cache.as_ref(),
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
            if !corrupted.is_empty() {
                mark_storage_accounting_dirty(&self.storage_accounting);
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
            mark_storage_accounting_dirty(&self.storage_accounting);
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
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let result = (|| {
            let repository_name = repository.to_string();
            let repository_directory = PrivateDirectory::open(
                self.repositories.capability(),
                OsStr::new(&repository_name),
            )
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let names = private_entry_names(&repository_directory)?;
            let mut markers = BTreeMap::<u64, ActivationMarker>::new();
            let mut generation_names = BTreeSet::new();
            for name in names {
                let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
                if text.starts_with(STAGING_PREFIX) {
                    continue;
                }
                if let Some((sequence, generation)) = parse_activation_name(text) {
                    let marker =
                        read_activation_marker(&repository_directory, name, sequence, generation)?;
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
                &repository_directory,
                &markers,
                &generation_names,
                retained,
                &retained_marker_names,
            )?;
            self.reconcile_repository_accounting(&mut accounting, repository, &repository_directory)
        })();
        if result.is_err() {
            accounting.dirty = true;
        }
        result
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
        {
            let created = self
                .created_source_blobs
                .lock()
                .map_err(|_| FirstSliceError::Catalog)?;
            if !created.is_empty() {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        if sources.len() >= PACKED_SOURCE_MIN_FILES {
            let mut ordered = Vec::new();
            ordered
                .try_reserve_exact(sources.len())
                .map_err(|_| FirstSliceError::Retention)?;
            ordered.extend(sources);
            ordered.sort_unstable_by_key(|source| source.snapshot.file());
            let mut writer = self.begin_packed_source_write()?;
            for source in ordered {
                writer.push(&source.snapshot)?;
            }
            return writer.finish();
        }
        let staging = self.staging();
        let sources_directory = staging
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        let blobs = ensure_private_directory(
            self.repository().capability(),
            OsStr::new(SOURCE_BLOBS_DIRECTORY),
        )?;
        let mut created = self
            .created_source_blobs
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if !created.is_empty() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
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
        drop(created);
        self.set_source_storage(legacy_source_storage())?;
        Ok(DurableSourceWrite {
            newly_written_bytes,
            referenced_bytes,
        })
    }

    pub(super) fn begin_packed_source_write(
        &self,
    ) -> Result<DurablePackedSourceWriter<'_>, FirstSliceError> {
        {
            let created = self
                .created_source_blobs
                .lock()
                .map_err(|_| FirstSliceError::Catalog)?;
            if !created.is_empty() {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        }
        let sources_directory = self
            .staging()
            .create_directory(OsStr::new(SOURCES_DIRECTORY))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut pack = Vec::new();
        pack.try_reserve_exact(
            usize::try_from(SOURCE_PACK_TARGET_BYTES).map_err(|_| FirstSliceError::Limits)?,
        )
        .map_err(|_| FirstSliceError::Retention)?;
        Ok(DurablePackedSourceWriter {
            prepared: self,
            sources_directory,
            entries: Vec::new(),
            pack_sizes: Vec::new(),
            pack,
            payload_bytes: 0,
            last_file: None,
        })
    }

    fn set_source_storage(
        &self,
        source_storage: DurableSourceStorage,
    ) -> Result<(), FirstSliceError> {
        let mut storage = self
            .source_storage
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if storage.replace(source_storage).is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(())
    }

    pub(super) fn write_recovery_snapshot(
        &self,
        snapshot: &GenerationSnapshot,
        expected_bytes: u64,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        check_cancellation(cancellation)?;
        if snapshot.metadata().generation() != self.generation
            || expected_bytes == 0
            || expected_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let observed_bytes =
            recovery_json_serialized_bytes(snapshot.document(), expected_bytes, cancellation)?;
        if observed_bytes != expected_bytes {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let staging = self.staging();
        let file = staging
            .create_file(OsStr::new(RECOVERY_SNAPSHOT_MESSAGEPACK_GZIP_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let encoded_writer =
            buffered_recovery_writer(file, MAX_RECOVERY_ENCODED_BYTES, cancellation);
        let encoder = GzEncoder::new(encoded_writer, Compression::fast());
        let mut decoded_writer =
            RecoverySnapshotWriter::new(encoder, MAX_RECOVERY_SNAPSHOT_BYTES, cancellation);
        if rmp_serde::encode::write_named(&mut decoded_writer, snapshot.document()).is_err() {
            return Err(recovery_writer_error(
                [
                    decoded_writer.failure,
                    decoded_writer.inner.get_ref().failure,
                ],
                FirstSliceError::Catalog,
                FirstSliceError::CatalogCorrupt,
            ));
        }
        if decoded_writer.flush().is_err() {
            return Err(recovery_writer_error(
                [
                    decoded_writer.failure,
                    decoded_writer.inner.get_ref().failure,
                ],
                FirstSliceError::Catalog,
                FirstSliceError::CatalogCorrupt,
            ));
        }
        check_cancellation(cancellation)?;
        if decoded_writer.bytes == 0 || decoded_writer.bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let decoded_bytes = decoded_writer.bytes;
        validate_recovery_document_accounting(decoded_bytes, expected_bytes)?;
        let decoded_digest = ContentHash::from_bytes(*decoded_writer.hasher.finalize().as_bytes());
        let mut encoder = decoded_writer.inner;
        if encoder.try_finish().is_err() {
            return Err(recovery_writer_error(
                [encoder.get_ref().failure],
                FirstSliceError::Catalog,
                FirstSliceError::CatalogCorrupt,
            ));
        }
        check_cancellation(cancellation)?;
        let mut encoded_writer = encoder.finish().map_err(|_| FirstSliceError::Catalog)?;
        if encoded_writer.flush().is_err() {
            return Err(recovery_writer_error(
                [encoded_writer.failure],
                FirstSliceError::Catalog,
                FirstSliceError::CatalogCorrupt,
            ));
        }
        check_cancellation(cancellation)?;
        if encoded_writer.bytes == 0 || encoded_writer.bytes > MAX_RECOVERY_ENCODED_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        check_cancellation(cancellation)?;
        encoded_writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        check_cancellation(cancellation)?;
        self.account_staging_bytes(encoded_writer.bytes)?;
        let metadata = snapshot.metadata();
        let contract = metadata.contract_version();
        let recovery = DurableRecoverySnapshot {
            version: RECOVERY_SNAPSHOT_VERSION,
            bytes: encoded_writer.bytes,
            digest: ContentHash::from_bytes(*encoded_writer.hasher.finalize().as_bytes()),
            encoding: Some(RecoverySnapshotEncoding::MessagePackGzip),
            decoded_bytes: Some(decoded_bytes),
            decoded_digest: Some(decoded_digest),
            serialized_document_bytes: Some(expected_bytes),
            contract_major: contract.major(),
            contract_minor: contract.minor(),
            manifest_hash: metadata.manifest_hash(),
            configuration_hash: metadata.configuration_hash(),
            provider_set_hash: metadata.provider_set_hash(),
        };
        check_cancellation(cancellation)?;
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
        check_cancellation(cancellation)?;
        self.account_staging_bytes(descriptor_bytes)?;
        encoded_writer
            .bytes
            .checked_add(descriptor_bytes)
            .ok_or(FirstSliceError::Limits)
    }

    pub(super) fn write_incremental_state(
        &self,
        state: &PreparedIncrementalState,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        check_cancellation(cancellation)?;
        let durable = DurableIncrementalState::from_prepared(state, cancellation)?;
        check_cancellation(cancellation)?;
        let staging = self.staging();
        let file = staging
            .create_file(OsStr::new(INCREMENTAL_STATE_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut writer = buffered_recovery_writer(file, MAX_INCREMENTAL_STATE_BYTES, cancellation);
        if serde_json::to_writer(&mut writer, &durable).is_err() {
            return Err(recovery_writer_error(
                [writer.failure],
                FirstSliceError::Catalog,
                FirstSliceError::Limits,
            ));
        }
        if writer.flush().is_err() {
            return Err(recovery_writer_error(
                [writer.failure],
                FirstSliceError::Catalog,
                FirstSliceError::Limits,
            ));
        }
        check_cancellation(cancellation)?;
        if writer.bytes > MAX_INCREMENTAL_STATE_BYTES {
            return Err(FirstSliceError::Limits);
        }
        writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        check_cancellation(cancellation)?;
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

    pub(super) fn write_source_file_catalog(
        &self,
        catalog: &SourceFileCatalog,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        check_cancellation(cancellation)?;
        if catalog.len() > MAX_SOURCE_BLOB_ENTRIES
            || catalog.entries().iter().any(|entry| {
                entry.file().repository != self.repository_id
                    || entry.file().generation != self.generation
            })
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let staging = self.staging();
        let file = staging
            .create_file(OsStr::new(SOURCE_FILE_CATALOG_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        let mut writer =
            buffered_recovery_writer(file, MAX_SOURCE_FILE_CATALOG_BYTES, cancellation);
        let durable = DurableSourceFileCatalogRef {
            entries: catalog.entries(),
        };
        if serde_json::to_writer(&mut writer, &durable).is_err() || writer.flush().is_err() {
            return Err(recovery_writer_error(
                [writer.failure],
                FirstSliceError::Catalog,
                FirstSliceError::Limits,
            ));
        }
        check_cancellation(cancellation)?;
        if writer.bytes == 0 || writer.bytes > MAX_SOURCE_FILE_CATALOG_BYTES {
            return Err(FirstSliceError::Limits);
        }
        writer
            .inner
            .get_ref()
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        check_cancellation(cancellation)?;
        self.account_staging_bytes(writer.bytes)?;
        let descriptor = DurableSidecarDescriptor {
            bytes: writer.bytes,
            digest: ContentHash::from_bytes(*writer.hasher.finalize().as_bytes()),
        };
        let mut slot = self
            .source_file_catalog
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        if slot.replace(descriptor).is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Ok(writer.bytes)
    }

    pub(super) fn write_logical_snapshot_identity(
        &self,
        snapshot: &GenerationSnapshot,
        identity: &FirstSliceLogicalSnapshotIdentity,
        serialized_document_bytes: u64,
    ) -> Result<u64, FirstSliceError> {
        let metadata = snapshot.metadata();
        if metadata.repository() != self.repository_id
            || metadata.generation() != self.generation
            || serialized_document_bytes == 0
            || serialized_document_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let contract = metadata.contract_version();
        let descriptor = DurableLogicalSnapshotIdentity {
            version: LOGICAL_SNAPSHOT_VERSION,
            repository: metadata.repository(),
            generation: metadata.generation(),
            parent: metadata.parent(),
            contract_major: contract.major(),
            contract_minor: contract.minor(),
            manifest_hash: metadata.manifest_hash(),
            configuration_hash: metadata.configuration_hash(),
            provider_set_hash: metadata.provider_set_hash(),
            schema_version: identity.schema_version().to_owned(),
            hash: identity.hash(),
            serialized_document_bytes: Some(serialized_document_bytes),
        };
        let payload = serde_json::to_string(&descriptor).map_err(|_| FirstSliceError::Catalog)?;
        let sidecar = DurableLogicalSnapshotSidecar {
            digest: content_hash_bytes(payload.as_bytes()),
            payload,
        };
        let bytes = serde_json::to_vec(&sidecar).map_err(|_| FirstSliceError::Catalog)?;
        let bytes_len = u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?;
        if bytes_len == 0 || bytes_len > MAX_LOGICAL_SNAPSHOT_BYTES {
            return Err(FirstSliceError::Limits);
        }
        let staging = self.staging();
        let mut file = staging
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        file.write_all(&bytes)
            .map_err(|_| FirstSliceError::Catalog)?;
        file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
        self.account_staging_bytes(bytes_len)?;
        Ok(bytes_len)
    }

    pub(super) fn finish(
        self,
        repository: RepositoryId,
        root_identity: ContentHash,
        display_name: &str,
        root_path: &str,
        receipt: &mut FirstSliceIndexReceipt,
    ) -> Result<DurableSealedGeneration, FirstSliceError> {
        if repository != receipt.repository
            || receipt.generation != self.generation
            || display_name.is_empty()
            || root_path.is_empty()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let retained_before_manifest = self.accounted_bytes.load(Ordering::Acquire);
        let incremental_state = *self
            .incremental_state
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?;
        let source_file_catalog = *self
            .source_file_catalog
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
                source_file_catalog,
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
        let materialized_bytes = self.accounted_bytes.load(Ordering::Acquire);
        let mut budget = StorageScanBudget::new();
        let scanned_generation = scan_generation_storage(
            repository,
            self.generation,
            self.staging(),
            &mut budget,
            SourceBlobScan::AccountPhysicalBytes,
        )?;
        Ok(DurableSealedGeneration {
            prepared: self,
            repository,
            materialized_bytes,
            manifest_written_bytes: manifest_bytes,
            scanned_generation,
        })
    }

    fn publish(
        mut self,
        scanned_generation: ScannedGeneration,
    ) -> Result<DurablePublishedGeneration, FirstSliceError> {
        let accounting_handle = Arc::clone(&self.storage_accounting);
        let mut accounting = accounting_handle
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        if accounting.dirty || accounting.repositories.is_none() {
            return Err(FirstSliceError::Retention);
        }
        let staging = self.staging.take().ok_or(FirstSliceError::Catalog)?;
        let generation_name = self.generation.to_string();
        let directory = match staging
            .publish_noreplace(self.repository().capability(), OsStr::new(&generation_name))
        {
            Ok(directory) => directory,
            Err(PublishError::NotCommitted { .. }) => {
                accounting.dirty = true;
                self.release_staging_bytes();
                return Err(FirstSliceError::Catalog);
            }
            Err(PublishError::CommittedButDurabilityUnknown { directory, .. }) => {
                accounting.dirty = true;
                self.release_staging_bytes();
                if directory.remove().is_err()
                    && let Ok(mut created) = self.created_source_blobs.lock()
                {
                    // The committed generation may still reference these blobs.
                    created.clear();
                }
                return Err(FirstSliceError::Catalog);
            }
            Err(_) => {
                accounting.dirty = true;
                self.release_staging_bytes();
                return Err(FirstSliceError::Catalog);
            }
        };
        let accounting_result = (|| {
            let repository_accounting = accounting
                .repositories
                .as_mut()
                .and_then(|repositories| repositories.get_mut(&self.repository_id))
                .ok_or(FirstSliceError::Retention)?;
            repository_accounting.temporary_bytes = repository_accounting
                .temporary_bytes
                .checked_sub(scanned_generation.tree_bytes)
                .ok_or(FirstSliceError::Retention)?;
            if repository_accounting
                .generations
                .insert(self.generation, scanned_generation)
                .is_some()
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            Ok(())
        })();
        if let Err(error) = accounting_result {
            accounting.dirty = true;
            self.release_staging_bytes();
            if directory.remove().is_err()
                && let Ok(mut created) = self.created_source_blobs.lock()
            {
                // The committed generation may still reference these blobs.
                created.clear();
            }
            return Err(error);
        }
        self.release_staging_bytes();
        self.created_source_blobs
            .lock()
            .map_err(|_| FirstSliceError::Catalog)?
            .clear();
        let repository = self.repository.take().ok_or(FirstSliceError::Catalog)?;
        Ok(DurablePublishedGeneration {
            directory: Some(directory),
            repository,
            repository_id: self.repository_id,
            generation: self.generation,
            storage_accounting: Arc::clone(&self.storage_accounting),
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

impl DurablePackedSourceWriter<'_> {
    pub(super) fn push(&mut self, snapshot: &SourceSnapshot) -> Result<(), FirstSliceError> {
        let file = snapshot.file();
        if self.entries.len() >= MAX_SOURCE_BLOB_ENTRIES
            || self.last_file.is_some_and(|previous| previous >= file)
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let content = snapshot.content();
        let digest = snapshot.content_hash();
        if content_hash_bytes(content) != digest {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let bytes = u64::try_from(content.len()).map_err(|_| FirstSliceError::Limits)?;
        if bytes > SOURCE_PACK_TARGET_BYTES {
            return Err(FirstSliceError::Limits);
        }
        let pack_len = u64::try_from(self.pack.len()).map_err(|_| FirstSliceError::Limits)?;
        if !self.pack.is_empty()
            && pack_len
                .checked_add(bytes)
                .is_none_or(|total| total > SOURCE_PACK_TARGET_BYTES)
        {
            write_source_pack(&self.sources_directory, &self.pack, &mut self.pack_sizes)?;
            self.pack.clear();
        }
        let offset = u64::try_from(self.pack.len()).map_err(|_| FirstSliceError::Limits)?;
        let pack_ordinal =
            u32::try_from(self.pack_sizes.len()).map_err(|_| FirstSliceError::Limits)?;
        self.pack.extend_from_slice(content);
        self.payload_bytes = self
            .payload_bytes
            .checked_add(bytes)
            .ok_or(FirstSliceError::Limits)?;
        self.entries.push(DurablePackedSourceEntry {
            file,
            digest,
            pack: pack_ordinal,
            offset,
            bytes,
        });
        self.last_file = Some(file);
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<DurableSourceWrite, FirstSliceError> {
        if self.entries.is_empty() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if !self.pack.is_empty() {
            write_source_pack(&self.sources_directory, &self.pack, &mut self.pack_sizes)?;
        }
        let index = DurablePackedSourceIndex {
            version: SOURCE_PACK_INDEX_VERSION,
            payload_bytes: self.payload_bytes,
            pack_bytes: self.pack_sizes,
            entries: self.entries,
        };
        let encoded = rmp_serde::to_vec_named(&index).map_err(|_| FirstSliceError::Catalog)?;
        let index_bytes = u64::try_from(encoded.len()).map_err(|_| FirstSliceError::Limits)?;
        if index_bytes == 0 || index_bytes > MAX_SOURCE_PACK_INDEX_BYTES {
            return Err(FirstSliceError::Limits);
        }
        let mut index_file = self
            .sources_directory
            .create_file(OsStr::new(SOURCE_PACK_INDEX_FILENAME))
            .map_err(|_| FirstSliceError::Catalog)?;
        index_file
            .write_all(&encoded)
            .map_err(|_| FirstSliceError::Catalog)?;
        index_file
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        drop(index_file);
        self.sources_directory
            .sync_all()
            .map_err(|_| FirstSliceError::Catalog)?;
        let files = u64::try_from(index.entries.len()).map_err(|_| FirstSliceError::Limits)?;
        let packs = u64::try_from(index.pack_bytes.len()).map_err(|_| FirstSliceError::Limits)?;
        let newly_written_bytes = self
            .payload_bytes
            .checked_add(index_bytes)
            .ok_or(FirstSliceError::Limits)?;
        self.prepared.account_staging_bytes(newly_written_bytes)?;
        self.prepared.set_source_storage(DurableSourceStorage {
            version: SOURCE_STORAGE_VERSION,
            files: Some(files),
            packs: Some(packs),
            index_bytes: Some(index_bytes),
            index_digest: Some(content_hash_bytes(&encoded)),
            payload_bytes: Some(self.payload_bytes),
        })?;
        Ok(DurableSourceWrite {
            newly_written_bytes,
            referenced_bytes: 0,
        })
    }
}

impl DurableSealedGeneration {
    pub(super) const fn manifest_written_bytes(&self) -> u64 {
        self.manifest_written_bytes
    }
}

impl DurableStorageAdmittedGeneration {
    pub(super) fn publish(self) -> Result<DurablePublishedGeneration, FirstSliceError> {
        self.sealed.prepared.publish(self.sealed.scanned_generation)
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
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        validate_activation_accounting(
            &accounting,
            self.repository_id,
            self.generation,
            repository_activation_sequence,
        )?;
        let published = publish_activation_marker(
            &self.repository,
            self.generation,
            repository_activation_sequence,
            global_activation_sequence,
            published_generation_count,
            operation,
        );
        match published {
            Ok(published) => {
                if let Err(error) = account_activation_marker(
                    &mut accounting,
                    self.repository_id,
                    published.marker,
                    published.bytes,
                ) {
                    accounting.dirty = true;
                    Err(error)
                } else {
                    Ok(published.bytes)
                }
            }
            Err(error) => {
                accounting.dirty = true;
                Err(error)
            }
        }
    }

    pub(super) fn disarm(mut self) {
        drop(self.directory.take());
    }

    pub(super) fn discard(mut self) -> Result<(), FirstSliceError> {
        let mut accounting = self
            .storage_accounting
            .lock()
            .map_err(|_| FirstSliceError::Retention)?;
        let result = self
            .directory
            .take()
            .ok_or(FirstSliceError::Catalog)?
            .remove()
            .map_err(|_| FirstSliceError::Catalog);
        match result {
            Ok(()) => {
                let removed = accounting
                    .repositories
                    .as_mut()
                    .and_then(|repositories| repositories.get_mut(&self.repository_id))
                    .and_then(|repository| repository.generations.remove(&self.generation));
                if removed.is_none() {
                    accounting.dirty = true;
                    return Err(FirstSliceError::Retention);
                }
                Ok(())
            }
            Err(error) => {
                accounting.dirty = true;
                Err(error)
            }
        }
    }
}

impl Drop for DurablePreparedGeneration {
    fn drop(&mut self) {
        if self.staging.is_some() {
            mark_storage_accounting_dirty(&self.storage_accounting);
        }
        if let Some(staging) = self.staging.take() {
            // The primary preparation error remains authoritative; restart
            // recovery removes this validated staging tree if cleanup fails.
            if staging.remove().is_ok() {
                self.release_staging_bytes();
            }
        }
        if let Ok(created) = self.created_source_blobs.lock()
            && let Some(repository) = self.repository.as_ref()
            && let Ok(blobs) =
                PrivateDirectory::open(repository.capability(), OsStr::new(SOURCE_BLOBS_DIRECTORY))
        {
            for digest in created.iter() {
                if let Ok(blob) =
                    PrivateDirectory::open(blobs.capability(), OsStr::new(&digest.to_string()))
                {
                    let _ = blob.remove();
                }
            }
            let _ = blobs.sync_all();
        }
    }
}

impl Drop for DurablePublishedGeneration {
    fn drop(&mut self) {
        if self.directory.is_some() {
            mark_storage_accounting_dirty(&self.storage_accounting);
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

fn write_source_pack(
    sources_directory: &PrivateDirectory<'_>,
    bytes: &[u8],
    pack_sizes: &mut Vec<u64>,
) -> Result<(), FirstSliceError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SOURCE_PACK_TARGET_BYTES {
        return Err(FirstSliceError::Limits);
    }
    let ordinal = u32::try_from(pack_sizes.len()).map_err(|_| FirstSliceError::Limits)?;
    let mut file = sources_directory
        .create_file(OsStr::new(&source_pack_name(ordinal)))
        .map_err(|_| FirstSliceError::Catalog)?;
    file.write_all(bytes)
        .map_err(|_| FirstSliceError::Catalog)?;
    // Immutable bounded packs preserve crash safety with one durability barrier
    // per pack instead of one barrier for every source file.
    file.sync_all().map_err(|_| FirstSliceError::Catalog)?;
    pack_sizes.push(u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?);
    Ok(())
}

fn source_pack_name(ordinal: u32) -> String {
    format!("{SOURCE_PACK_PREFIX}{ordinal:08}{SOURCE_PACK_SUFFIX}")
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

fn read_packed_source_index(
    sources: &PrivateDirectory<'_>,
    storage: DurableSourceStorage,
) -> Result<PackedSourceIndex, FirstSliceError> {
    read_packed_source_index_with_cancellation(sources, storage, &Cancellation::new())
}

fn read_packed_source_index_with_cancellation(
    sources: &PrivateDirectory<'_>,
    storage: DurableSourceStorage,
    cancellation: &Cancellation,
) -> Result<PackedSourceIndex, FirstSliceError> {
    check_cancellation(cancellation)?;
    let expected_index_bytes = storage.index_bytes.ok_or(FirstSliceError::CatalogCorrupt)?;
    let expected_index_digest = storage
        .index_digest
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    if expected_index_bytes == 0 || expected_index_bytes > MAX_SOURCE_PACK_INDEX_BYTES {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let encoded = sources
        .read_file_bounded_cancellable(
            OsStr::new(SOURCE_PACK_INDEX_FILENAME),
            expected_index_bytes,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    if u64::try_from(encoded.len()).ok() != Some(expected_index_bytes)
        || content_hash_bytes(&encoded) != expected_index_digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    let index: DurablePackedSourceIndex =
        rmp_serde::from_slice(&encoded).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    validate_packed_source_index(index, storage, cancellation)
}

fn validate_packed_source_index(
    index: DurablePackedSourceIndex,
    storage: DurableSourceStorage,
    cancellation: &Cancellation,
) -> Result<PackedSourceIndex, FirstSliceError> {
    check_cancellation(cancellation)?;
    let expected_files = storage.files.ok_or(FirstSliceError::CatalogCorrupt)?;
    let expected_packs = storage.packs.ok_or(FirstSliceError::CatalogCorrupt)?;
    let expected_payload_bytes = storage
        .payload_bytes
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    if index.version != SOURCE_PACK_INDEX_VERSION
        || u64::try_from(index.entries.len()).ok() != Some(expected_files)
        || u64::try_from(index.pack_bytes.len()).ok() != Some(expected_packs)
        || index.entries.len() > MAX_SOURCE_BLOB_ENTRIES
        || index.pack_bytes.len() > index.entries.len()
        || index.payload_bytes != expected_payload_bytes
        || index
            .pack_bytes
            .iter()
            .any(|bytes| *bytes > SOURCE_PACK_TARGET_BYTES)
        || index.pack_bytes.iter().try_fold(0_u64, |total, bytes| {
            check_cancellation(cancellation)?;
            total.checked_add(*bytes).ok_or(FirstSliceError::Limits)
        })? != expected_payload_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    if index.entries.is_empty() || index.pack_bytes.is_empty() {
        return Err(FirstSliceError::CatalogCorrupt);
    }

    let mut entries = BTreeMap::new();
    let mut expected_pack = 0_u32;
    let mut expected_offset = 0_u64;
    for entry in index.entries {
        check_cancellation(cancellation)?;
        if entry.bytes > DEFAULT_MAX_SOURCE_FILE_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if entry.pack != expected_pack {
            let expected_size = index
                .pack_bytes
                .get(usize::try_from(expected_pack).map_err(|_| FirstSliceError::Limits)?)
                .copied()
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            if expected_offset != expected_size
                || entry.pack != expected_pack.saturating_add(1)
                || entry.offset != 0
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            expected_pack = entry.pack;
            expected_offset = 0;
        }
        if entry.offset != expected_offset {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        expected_offset = expected_offset
            .checked_add(entry.bytes)
            .ok_or(FirstSliceError::Limits)?;
        let pack_size = index
            .pack_bytes
            .get(usize::try_from(entry.pack).map_err(|_| FirstSliceError::Limits)?)
            .copied()
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if expected_offset > pack_size || entries.insert(entry.file, entry).is_some() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    let final_size = index
        .pack_bytes
        .get(usize::try_from(expected_pack).map_err(|_| FirstSliceError::Limits)?)
        .copied()
        .ok_or(FirstSliceError::CatalogCorrupt)?;
    if expected_offset != final_size
        || usize::try_from(expected_pack)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            != Some(index.pack_bytes.len())
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    Ok(PackedSourceIndex {
        pack_bytes: index.pack_bytes,
        entries,
    })
}

fn private_entry_names(directory: &PrivateDirectory<'_>) -> Result<Vec<OsString>, FirstSliceError> {
    private_entry_names_with_cancellation(directory, &Cancellation::new())
}

fn private_entry_names_with_cancellation(
    directory: &PrivateDirectory<'_>,
    cancellation: &Cancellation,
) -> Result<Vec<OsString>, FirstSliceError> {
    check_cancellation(cancellation)?;
    let entries = directory
        .capability()
        .entries()
        .map_err(|_| FirstSliceError::Catalog)?;
    let mut names = Vec::new();
    for entry in entries {
        check_cancellation(cancellation)?;
        let entry = entry.map_err(|_| FirstSliceError::Catalog)?;
        if names.len() >= MAX_DURABLE_ENTRIES {
            return Err(FirstSliceError::Retention);
        }
        names
            .try_reserve(1)
            .map_err(|_| FirstSliceError::Retention)?;
        names.push(entry.file_name());
    }
    check_cancellation(cancellation)?;
    names.sort();
    check_cancellation(cancellation)?;
    Ok(names)
}

impl StorageScanBudget {
    fn new() -> Self {
        Self {
            visited_entries: 0,
            cancellation: Cancellation::new(),
            #[cfg(test)]
            after_visit: None,
        }
    }

    fn with_cancellation(cancellation: &Cancellation) -> Self {
        Self {
            cancellation: cancellation.clone(),
            ..Self::new()
        }
    }

    fn check(&self) -> Result<(), FirstSliceError> {
        check_cancellation(&self.cancellation)
    }

    fn entry_names(
        &self,
        directory: &PrivateDirectory<'_>,
    ) -> Result<Vec<OsString>, FirstSliceError> {
        private_entry_names_with_cancellation(directory, &self.cancellation)
    }

    fn visit(&mut self) -> Result<(), FirstSliceError> {
        self.check()?;
        self.visited_entries = self
            .visited_entries
            .checked_add(1)
            .ok_or(FirstSliceError::Limits)?;
        if self.visited_entries > MAX_STORAGE_INVENTORY_ENTRIES {
            return Err(FirstSliceError::Retention);
        }
        #[cfg(test)]
        if let Some(after_visit) = self.after_visit.as_mut() {
            after_visit(self.visited_entries);
        }
        self.check()?;
        Ok(())
    }
}

fn inventory_from_accounting(
    accounting: &DurableStorageAccounting,
    available_bytes: u64,
) -> Result<DurableStorageInventory, FirstSliceError> {
    let repositories = accounting
        .repositories
        .as_ref()
        .ok_or(FirstSliceError::Retention)?;
    let mut inventory = build_storage_inventory(
        repositories.values().cloned().collect(),
        accounting.quarantine_bytes,
        available_bytes,
    )?;
    apply_storage_reservations(&mut inventory, &accounting.reservations)?;
    Ok(inventory)
}

fn apply_storage_reservations(
    inventory: &mut DurableStorageInventory,
    reservations: &DurableStorageReservations,
) -> Result<(), FirstSliceError> {
    for reservation in reservations.entries.values() {
        checked_add_assign(
            &mut inventory.inflight_catalog_reservation_bytes,
            reservation.catalog_bytes,
        )?;
        checked_add_assign(
            &mut inventory.inflight_repository_reservation_bytes,
            reservation.repository_bytes,
        )?;
        if let Some(repository) = inventory
            .repositories
            .iter_mut()
            .find(|repository| repository.repository == reservation.repository)
        {
            checked_add_assign(
                &mut repository.inflight_reservation_bytes,
                reservation.repository_bytes,
            )?;
        } else {
            inventory.repositories.push(DurableRepositoryStorage {
                repository: reservation.repository,
                physical_bytes: 0,
                active_generation_bytes: 0,
                predecessor_generation_bytes: 0,
                other_retained_generation_bytes: 0,
                source_pool_bytes: 0,
                shared_source_bytes: 0,
                temporary_bytes: 0,
                reclaimable_bytes: 0,
                repository_overhead_bytes: 0,
                inflight_reservation_bytes: reservation.repository_bytes,
            });
        }
    }
    inventory
        .repositories
        .sort_unstable_by_key(|repository| repository.repository);
    Ok(())
}

fn restore_repository_accounting(
    accounting: &mut DurableStorageAccounting,
    repository: RepositoryId,
    previous: Option<ScannedRepositoryStorage>,
) {
    let Some(repositories) = accounting.repositories.as_mut() else {
        accounting.dirty = true;
        return;
    };
    if let Some(previous) = previous {
        repositories.insert(repository, previous);
    } else {
        repositories.remove(&repository);
    }
}

fn mark_storage_accounting_dirty(accounting: &Arc<Mutex<DurableStorageAccounting>>) {
    if let Ok(mut accounting) = accounting.lock() {
        accounting.dirty = true;
    }
}

fn validate_activation_accounting(
    accounting: &DurableStorageAccounting,
    repository: RepositoryId,
    generation: GenerationId,
    sequence: u64,
) -> Result<(), FirstSliceError> {
    if accounting.dirty {
        return Err(FirstSliceError::Retention);
    }
    let repository = accounting
        .repositories
        .as_ref()
        .and_then(|repositories| repositories.get(&repository))
        .ok_or(FirstSliceError::Retention)?;
    if !repository.generations.contains_key(&generation)
        || repository.markers.contains_key(&sequence)
        || repository.marker_bytes.contains_key(&sequence)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

fn account_activation_marker(
    accounting: &mut DurableStorageAccounting,
    repository: RepositoryId,
    marker: ActivationMarker,
    bytes: u64,
) -> Result<(), FirstSliceError> {
    if accounting.dirty {
        return Err(FirstSliceError::Retention);
    }
    let repository = accounting
        .repositories
        .as_mut()
        .and_then(|repositories| repositories.get_mut(&repository))
        .ok_or(FirstSliceError::Retention)?;
    if !repository
        .generations
        .contains_key(&marker.manifest.generation)
    {
        accounting.dirty = true;
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let generations = repository.generations.keys().copied().collect();
    let retained = retained_activation_marker_names(&repository.markers, &generations);
    repository
        .markers
        .retain(|_, existing| retained.contains(&existing.name));
    repository
        .marker_bytes
        .retain(|sequence, _| repository.markers.contains_key(sequence));
    if repository
        .marker_bytes
        .insert(marker.sequence, bytes)
        .is_some()
        || repository.markers.insert(marker.sequence, marker).is_some()
    {
        accounting.dirty = true;
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(())
}

fn scan_storage_state(
    repositories: &PrivateDirectory<'_>,
    quarantine: &PrivateDirectory<'_>,
    maximum_repositories: usize,
    verified_source_blobs: &mut BTreeMap<(RepositoryId, ContentHash), VerifiedSourceBlobMetadata>,
    source_blob_scan: SourceBlobScan,
) -> Result<(BTreeMap<RepositoryId, ScannedRepositoryStorage>, u64), FirstSliceError> {
    scan_storage_state_with_budget(
        repositories,
        quarantine,
        maximum_repositories,
        verified_source_blobs,
        source_blob_scan,
        &mut StorageScanBudget::new(),
    )
}

fn scan_storage_state_with_budget(
    repositories: &PrivateDirectory<'_>,
    quarantine: &PrivateDirectory<'_>,
    maximum_repositories: usize,
    verified_source_blobs: &mut BTreeMap<(RepositoryId, ContentHash), VerifiedSourceBlobMetadata>,
    source_blob_scan: SourceBlobScan,
    budget: &mut StorageScanBudget,
) -> Result<(BTreeMap<RepositoryId, ScannedRepositoryStorage>, u64), FirstSliceError> {
    let repository_names = budget.entry_names(repositories)?;
    if repository_names.len() > maximum_repositories {
        return Err(FirstSliceError::Retention);
    }
    let mut scanned_repositories = BTreeMap::new();
    for repository_name in repository_names {
        budget.visit()?;
        let repository_text = repository_name
            .to_str()
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let repository_id =
            RepositoryId::from_str(repository_text).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let repository = PrivateDirectory::open(repositories.capability(), &repository_name)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let scanned = scan_repository_storage(
            repository_id,
            &repository,
            budget,
            verified_source_blobs,
            source_blob_scan,
        )?;
        if scanned_repositories
            .insert(repository_id, scanned)
            .is_some()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    let quarantine_bytes = directory_tree_bytes(quarantine, budget)?;
    budget.check()?;
    Ok((scanned_repositories, quarantine_bytes))
}

fn scan_repository_storage(
    repository_id: RepositoryId,
    repository: &PrivateDirectory<'_>,
    budget: &mut StorageScanBudget,
    verified_source_blobs: &mut BTreeMap<(RepositoryId, ContentHash), VerifiedSourceBlobMetadata>,
    source_blob_scan: SourceBlobScan,
) -> Result<ScannedRepositoryStorage, FirstSliceError> {
    let mut scanned = ScannedRepositoryStorage {
        repository: Some(repository_id),
        ..ScannedRepositoryStorage::default()
    };
    for name in budget.entry_names(repository)? {
        budget.visit()?;
        let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
        if text.starts_with(STAGING_PREFIX) {
            let staging = PrivateDirectory::open(repository.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            checked_add_assign(
                &mut scanned.temporary_bytes,
                directory_tree_bytes(&staging, budget)?,
            )?;
        } else if let Some((sequence, generation)) = parse_activation_name(text) {
            let marker = read_activation_marker(repository, name, sequence, generation)?;
            let bytes = directory_tree_bytes(
                &PrivateDirectory::open(repository.capability(), &marker.name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?,
                budget,
            )?;
            if scanned.markers.insert(sequence, marker).is_some()
                || scanned.marker_bytes.insert(sequence, bytes).is_some()
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        } else if let Some(sequence) = parse_metadata_name(text) {
            read_repository_metadata(repository_id, repository, &name, sequence)?;
            let bytes = directory_tree_bytes(
                &PrivateDirectory::open(repository.capability(), &name)
                    .map_err(|_| FirstSliceError::CatalogCorrupt)?,
                budget,
            )?;
            if scanned.metadata_bytes.insert(sequence, bytes).is_some() {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        } else if text == SOURCE_BLOBS_DIRECTORY {
            scan_source_blob_storage(
                repository_id,
                repository,
                budget,
                &mut scanned,
                verified_source_blobs,
                source_blob_scan,
            )?;
        } else if let Ok(generation) = GenerationId::from_str(text) {
            let generation_directory = PrivateDirectory::open(repository.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let generation_storage = scan_generation_storage(
                repository_id,
                generation,
                &generation_directory,
                budget,
                source_blob_scan,
            )?;
            if scanned
                .generations
                .insert(generation, generation_storage)
                .is_some()
            {
                return Err(FirstSliceError::CatalogCorrupt);
            }
        } else {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    if scanned.markers.values().any(|marker| {
        !scanned
            .generations
            .contains_key(&marker.manifest.generation)
    }) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(scanned)
}

fn scan_generation_storage(
    repository: RepositoryId,
    generation: GenerationId,
    directory: &PrivateDirectory<'_>,
    budget: &mut StorageScanBudget,
    source_scan: SourceBlobScan,
) -> Result<ScannedGeneration, FirstSliceError> {
    let manifest_bytes = directory
        .read_file_bounded(OsStr::new(MANIFEST_FILENAME), MAX_MANIFEST_BYTES)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let manifest: DurableGenerationManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if manifest.receipt.repository != repository || manifest.receipt.generation != generation {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let source_blobs = match source_storage_layout(manifest.version, manifest.source_storage)? {
        DurableSourceLayout::Inline => BTreeMap::new(),
        DurableSourceLayout::Blobs => generation_source_digests(directory, budget)?,
        DurableSourceLayout::Packed(storage) => {
            scan_packed_source_storage(directory, storage, budget, source_scan)?;
            BTreeMap::new()
        }
    };
    Ok(ScannedGeneration {
        repository,
        generation,
        parent: manifest.receipt.parent,
        tree_bytes: directory_tree_bytes(directory, budget)?,
        source_blobs,
    })
}

fn generation_source_digests(
    generation: &PrivateDirectory<'_>,
    budget: &StorageScanBudget,
) -> Result<BTreeMap<ContentHash, u64>, FirstSliceError> {
    let sources = PrivateDirectory::open(generation.capability(), OsStr::new(SOURCES_DIRECTORY))
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let mut blobs = BTreeMap::new();
    for name in budget.entry_names(&sources)? {
        budget.check()?;
        let pointer = sources
            .read_file_bounded(&name, MAX_SOURCE_POINTER_BYTES)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let pointer = decode_source_pointer(&pointer)?;
        if let Some(bytes) = blobs.insert(pointer.digest, pointer.bytes)
            && bytes != pointer.bytes
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    Ok(blobs)
}

fn scan_packed_source_storage(
    generation: &PrivateDirectory<'_>,
    storage: DurableSourceStorage,
    budget: &mut StorageScanBudget,
    source_scan: SourceBlobScan,
) -> Result<(), FirstSliceError> {
    let sources = PrivateDirectory::open(generation.capability(), OsStr::new(SOURCES_DIRECTORY))
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    budget.check()?;
    let index =
        read_packed_source_index_with_cancellation(&sources, storage, &budget.cancellation)?;
    let mut expected_names = BTreeSet::from([OsString::from(SOURCE_PACK_INDEX_FILENAME)]);
    for ordinal in 0..index.pack_bytes.len() {
        let ordinal = u32::try_from(ordinal).map_err(|_| FirstSliceError::Limits)?;
        expected_names.insert(OsString::from(source_pack_name(ordinal)));
    }
    let observed_names = budget.entry_names(&sources)?;
    if observed_names.len() != expected_names.len()
        || observed_names
            .iter()
            .any(|name| !expected_names.contains(name))
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    for (ordinal, expected_bytes) in index.pack_bytes.iter().copied().enumerate() {
        budget.visit()?;
        let ordinal = u32::try_from(ordinal).map_err(|_| FirstSliceError::Limits)?;
        let name = source_pack_name(ordinal);
        let metadata = sources
            .capability()
            .symlink_metadata(Path::new(&name))
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != expected_bytes
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if matches!(source_scan, SourceBlobScan::VerifyContent) {
            let pack = sources
                .read_file_bounded(OsStr::new(&name), expected_bytes)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            verify_source_pack(&pack, ordinal, &index)?;
        }
    }
    Ok(())
}

fn verify_source_pack(
    pack: &[u8],
    ordinal: u32,
    index: &PackedSourceIndex,
) -> Result<(), FirstSliceError> {
    for entry in index.entries.values().filter(|entry| entry.pack == ordinal) {
        let start = usize::try_from(entry.offset).map_err(|_| FirstSliceError::Limits)?;
        let end = entry
            .offset
            .checked_add(entry.bytes)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(FirstSliceError::Limits)?;
        let content = pack
            .get(start..end)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if content_hash_bytes(content) != entry.digest {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    Ok(())
}

fn scan_source_blob_storage(
    repository_id: RepositoryId,
    repository: &PrivateDirectory<'_>,
    budget: &mut StorageScanBudget,
    scanned: &mut ScannedRepositoryStorage,
    verified_source_blobs: &mut BTreeMap<(RepositoryId, ContentHash), VerifiedSourceBlobMetadata>,
    source_blob_scan: SourceBlobScan,
) -> Result<(), FirstSliceError> {
    let blobs = PrivateDirectory::open(repository.capability(), OsStr::new(SOURCE_BLOBS_DIRECTORY))
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    for name in budget.entry_names(&blobs)? {
        budget.visit()?;
        let text = name.to_str().ok_or(FirstSliceError::CatalogCorrupt)?;
        let blob = PrivateDirectory::open(blobs.capability(), &name)
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let bytes = directory_tree_bytes(&blob, budget)?;
        if text.starts_with(STAGING_PREFIX) {
            checked_add_assign(&mut scanned.temporary_bytes, bytes)?;
            continue;
        }
        let digest = ContentHash::from_str(text).map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let payload_metadata = blob
            .capability()
            .symlink_metadata(Path::new(SOURCE_BLOB_PAYLOAD_FILENAME))
            .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        if !payload_metadata.is_file() || payload_metadata.file_type().is_symlink() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let payload_bytes = payload_metadata.len();
        if payload_bytes > MAX_SNAPSHOT_BYTES || payload_bytes > DEFAULT_MAX_SOURCE_FILE_BYTES {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let verification_key = (repository_id, digest);
        let metadata_before = source_blob_verification_metadata(&payload_metadata);
        if matches!(source_blob_scan, SourceBlobScan::VerifyContent)
            && (metadata_before.is_none()
                || verified_source_blobs.get(&verification_key).copied() != metadata_before)
        {
            let payload = blob
                .read_file_bounded(OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME), payload_bytes)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            if content_hash_bytes(&payload) != digest {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            let metadata_after = blob
                .capability()
                .symlink_metadata(Path::new(SOURCE_BLOB_PAYLOAD_FILENAME))
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            let metadata_after = source_blob_verification_metadata(&metadata_after);
            if metadata_before != metadata_after {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            if let Some(metadata) = metadata_after {
                // Atomically published blobs are immutable, but the owner can
                // still alter state externally. Reuse is therefore bound to
                // file identity, size, and mtime rather than the digest path.
                verified_source_blobs.insert(verification_key, metadata);
            } else {
                verified_source_blobs.remove(&verification_key);
            }
        }
        if scanned
            .source_blobs
            .insert(
                digest,
                ScannedSourceBlob {
                    bytes,
                    payload_bytes,
                },
            )
            .is_some()
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    Ok(())
}

fn source_blob_verification_metadata(
    metadata: &cap_std::fs::Metadata,
) -> Option<VerifiedSourceBlobMetadata> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let modified_ns = metadata
        .modified()
        .ok()?
        .into_std()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(VerifiedSourceBlobMetadata {
        payload_bytes: metadata.len(),
        modified_ns,
        volume: cap_fs_ext::MetadataExt::dev(metadata),
        file_index: cap_fs_ext::MetadataExt::ino(metadata),
    })
}

fn build_storage_inventory(
    scanned_repositories: Vec<ScannedRepositoryStorage>,
    quarantine_bytes: u64,
    available_bytes: u64,
) -> Result<DurableStorageInventory, FirstSliceError> {
    let generation_capacity =
        scanned_repositories
            .iter()
            .try_fold(0_usize, |total, repository| {
                total
                    .checked_add(repository.generations.len())
                    .ok_or(FirstSliceError::Limits)
            })?;
    let mut generations = Vec::new();
    generations
        .try_reserve_exact(generation_capacity)
        .map_err(|_| FirstSliceError::Retention)?;
    let mut generation_unique_bytes = 0_u64;
    let mut active_generation_bytes = 0_u64;
    let mut predecessor_generation_bytes = 0_u64;
    let mut other_retained_generation_bytes = 0_u64;
    let mut source_pool_bytes = 0_u64;
    let mut shared_source_bytes = 0_u64;
    let mut temporary_bytes = 0_u64;
    let mut reclaimable_bytes = 0_u64;
    let mut repository_overhead_bytes = 0_u64;
    let mut repository_totals = Vec::new();
    repository_totals
        .try_reserve_exact(scanned_repositories.len())
        .map_err(|_| FirstSliceError::Retention)?;
    for repository in scanned_repositories {
        let repository_id = repository.repository;
        let mut repository_active_bytes = 0_u64;
        let mut repository_predecessor_bytes = 0_u64;
        let mut repository_other_retained_bytes = 0_u64;
        let mut repository_source_pool_bytes = 0_u64;
        let mut repository_shared_source_bytes = 0_u64;
        let repository_temporary_bytes = repository.temporary_bytes;
        let mut repository_reclaimable_bytes = 0_u64;
        let mut repository_live_overhead_bytes = 0_u64;
        let latest_by_generation = latest_activation_sequences(&repository.markers);
        let active = latest_by_generation
            .iter()
            .max_by_key(|(_, sequence)| **sequence)
            .map(|(generation, _)| *generation);
        let predecessor = active
            .and_then(|generation| repository.generations.get(&generation))
            .and_then(|generation| generation.parent)
            .filter(|generation| repository.generations.contains_key(generation));
        let retained: BTreeSet<_> = latest_by_generation.keys().copied().collect();
        let mut source_reference_counts = BTreeMap::<ContentHash, u64>::new();
        for generation in repository
            .generations
            .values()
            .filter(|generation| retained.contains(&generation.generation))
        {
            for (digest, declared_bytes) in &generation.source_blobs {
                let blob = repository
                    .source_blobs
                    .get(digest)
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                if blob.payload_bytes != *declared_bytes {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let count = source_reference_counts.entry(*digest).or_default();
                *count = count.checked_add(1).ok_or(FirstSliceError::Limits)?;
            }
        }
        for (digest, blob) in &repository.source_blobs {
            match source_reference_counts.get(digest).copied().unwrap_or(0) {
                0 => checked_add_assign(&mut repository_reclaimable_bytes, blob.bytes)?,
                references => {
                    checked_add_assign(&mut repository_source_pool_bytes, blob.bytes)?;
                    if references > 1 {
                        checked_add_assign(&mut repository_shared_source_bytes, blob.bytes)?;
                    }
                }
            }
        }
        let retained_marker_names =
            retained_activation_marker_names(&repository.markers, &retained);
        for (sequence, bytes) in &repository.marker_bytes {
            let marker = repository
                .markers
                .get(sequence)
                .ok_or(FirstSliceError::CatalogCorrupt)?;
            if retained_marker_names.contains(&marker.name) {
                checked_add_assign(&mut repository_live_overhead_bytes, *bytes)?;
            } else {
                checked_add_assign(&mut repository_reclaimable_bytes, *bytes)?;
            }
        }
        let latest_metadata_sequence = repository.metadata_bytes.keys().next_back().copied();
        for (sequence, bytes) in &repository.metadata_bytes {
            if Some(*sequence) == latest_metadata_sequence {
                checked_add_assign(&mut repository_live_overhead_bytes, *bytes)?;
            } else {
                checked_add_assign(&mut repository_reclaimable_bytes, *bytes)?;
            }
        }
        for generation in repository.generations.into_values() {
            let is_retained = retained.contains(&generation.generation);
            let active_generation = active == Some(generation.generation);
            let predecessor_generation = predecessor == Some(generation.generation);
            let reclaimable = !is_retained;
            checked_add_assign(&mut generation_unique_bytes, generation.tree_bytes)?;
            if active_generation {
                checked_add_assign(&mut repository_active_bytes, generation.tree_bytes)?;
            } else if predecessor_generation {
                checked_add_assign(&mut repository_predecessor_bytes, generation.tree_bytes)?;
            } else if is_retained {
                checked_add_assign(&mut repository_other_retained_bytes, generation.tree_bytes)?;
            } else {
                checked_add_assign(&mut repository_reclaimable_bytes, generation.tree_bytes)?;
            }
            generations.push(DurableGenerationStorage {
                repository: generation.repository,
                generation: generation.generation,
                parent: generation.parent,
                unique_bytes: generation.tree_bytes,
                active: active_generation,
                predecessor: predecessor_generation,
                reclaimable,
            });
        }
        if let Some(repository) = repository_id {
            let physical_bytes = repository_active_bytes
                .checked_add(repository_predecessor_bytes)
                .and_then(|bytes| bytes.checked_add(repository_other_retained_bytes))
                .and_then(|bytes| bytes.checked_add(repository_source_pool_bytes))
                .and_then(|bytes| bytes.checked_add(repository_temporary_bytes))
                .and_then(|bytes| bytes.checked_add(repository_reclaimable_bytes))
                .and_then(|bytes| bytes.checked_add(repository_live_overhead_bytes))
                .ok_or(FirstSliceError::Limits)?;
            repository_totals.push(DurableRepositoryStorage {
                repository,
                physical_bytes,
                active_generation_bytes: repository_active_bytes,
                predecessor_generation_bytes: repository_predecessor_bytes,
                other_retained_generation_bytes: repository_other_retained_bytes,
                source_pool_bytes: repository_source_pool_bytes,
                shared_source_bytes: repository_shared_source_bytes,
                temporary_bytes: repository_temporary_bytes,
                reclaimable_bytes: repository_reclaimable_bytes,
                repository_overhead_bytes: repository_live_overhead_bytes,
                inflight_reservation_bytes: 0,
            });
        }
        checked_add_assign(&mut active_generation_bytes, repository_active_bytes)?;
        checked_add_assign(
            &mut predecessor_generation_bytes,
            repository_predecessor_bytes,
        )?;
        checked_add_assign(
            &mut other_retained_generation_bytes,
            repository_other_retained_bytes,
        )?;
        checked_add_assign(&mut source_pool_bytes, repository_source_pool_bytes)?;
        checked_add_assign(&mut shared_source_bytes, repository_shared_source_bytes)?;
        checked_add_assign(&mut temporary_bytes, repository_temporary_bytes)?;
        checked_add_assign(&mut reclaimable_bytes, repository_reclaimable_bytes)?;
        checked_add_assign(
            &mut repository_overhead_bytes,
            repository_live_overhead_bytes,
        )?;
    }
    generations.sort_unstable_by_key(|generation| (generation.repository, generation.generation));
    repository_totals.sort_unstable_by_key(|repository| repository.repository);
    let total_physical_bytes = active_generation_bytes
        .checked_add(predecessor_generation_bytes)
        .and_then(|bytes| bytes.checked_add(other_retained_generation_bytes))
        .and_then(|bytes| bytes.checked_add(source_pool_bytes))
        .and_then(|bytes| bytes.checked_add(temporary_bytes))
        .and_then(|bytes| bytes.checked_add(reclaimable_bytes))
        .and_then(|bytes| bytes.checked_add(repository_overhead_bytes))
        .and_then(|bytes| bytes.checked_add(quarantine_bytes))
        .ok_or(FirstSliceError::Limits)?;
    Ok(DurableStorageInventory {
        repositories: repository_totals,
        generations,
        generation_unique_bytes,
        active_generation_bytes,
        predecessor_generation_bytes,
        other_retained_generation_bytes,
        source_pool_bytes,
        shared_source_bytes,
        temporary_bytes,
        reclaimable_bytes,
        pinned_bytes: 0,
        repository_overhead_bytes,
        quarantine_bytes,
        total_physical_bytes,
        available_bytes,
        inflight_catalog_reservation_bytes: 0,
        inflight_repository_reservation_bytes: 0,
    })
}

fn latest_activation_sequences(
    markers: &BTreeMap<u64, ActivationMarker>,
) -> BTreeMap<GenerationId, u64> {
    let mut latest: BTreeMap<GenerationId, u64> = BTreeMap::new();
    for marker in markers.values() {
        latest
            .entry(marker.manifest.generation)
            .and_modify(|sequence| *sequence = (*sequence).max(marker.sequence))
            .or_insert(marker.sequence);
    }
    latest
}

fn directory_tree_bytes(
    directory: &PrivateDirectory<'_>,
    budget: &mut StorageScanBudget,
) -> Result<u64, FirstSliceError> {
    let mut total = 0_u64;
    for name in budget.entry_names(directory)? {
        budget.visit()?;
        let metadata = directory
            .capability()
            .symlink_metadata(Path::new(&name))
            .map_err(|_| FirstSliceError::Catalog)?;
        if metadata.file_type().is_symlink() {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        if metadata.is_file() {
            checked_add_assign(&mut total, metadata.len())?;
        } else if metadata.is_dir() {
            let child = PrivateDirectory::open(directory.capability(), &name)
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
            checked_add_assign(&mut total, directory_tree_bytes(&child, budget)?)?;
        } else {
            return Err(FirstSliceError::CatalogCorrupt);
        }
    }
    Ok(total)
}

fn checked_add_assign(total: &mut u64, bytes: u64) -> Result<(), FirstSliceError> {
    *total = total.checked_add(bytes).ok_or(FirstSliceError::Limits)?;
    Ok(())
}

fn check_storage_admission(
    inventory: &DurableStorageInventory,
    repository: RepositoryId,
    policy: DurableStorageAdmissionPolicy,
) -> Result<DurableStorageAdmission, DurableStorageAdmissionFailure> {
    let projection = storage_admission_projection(inventory, repository, policy);
    if projection.projected_repository_bytes > policy.maximum_repository_bytes {
        return Err(DurableStorageAdmissionFailure {
            scope: DurableStorageAdmissionScope::RepositoryBudget,
            required_bytes: policy.required_repository_bytes,
            observed_bytes: projection.admitted_repository_bytes,
            projected_bytes: projection.projected_repository_bytes,
            limit_bytes: policy.maximum_repository_bytes,
            minimum_free_bytes: policy.minimum_free_bytes,
            repository_amplification: None,
        });
    }
    if projection.projected_catalog_bytes > policy.maximum_storage_bytes {
        return Err(DurableStorageAdmissionFailure {
            scope: DurableStorageAdmissionScope::CatalogBudget,
            required_bytes: policy.required_catalog_bytes,
            observed_bytes: projection.admitted_catalog_bytes,
            projected_bytes: projection.projected_catalog_bytes,
            limit_bytes: policy.maximum_storage_bytes,
            minimum_free_bytes: policy.minimum_free_bytes,
            repository_amplification: None,
        });
    }
    if policy.required_catalog_bytes > projection.usable_free_bytes {
        return Err(DurableStorageAdmissionFailure {
            scope: DurableStorageAdmissionScope::FilesystemFreeSpace,
            required_bytes: policy.required_catalog_bytes,
            observed_bytes: inventory.available_bytes,
            projected_bytes: policy.required_catalog_bytes,
            limit_bytes: projection.usable_free_bytes,
            minimum_free_bytes: policy.minimum_free_bytes,
            repository_amplification: None,
        });
    }
    Ok(DurableStorageAdmission {
        required_bytes: policy.required_catalog_bytes,
        observed_bytes: projection.admitted_catalog_bytes,
        limit_bytes: policy.maximum_storage_bytes,
        minimum_free_bytes: policy.minimum_free_bytes,
        admission_margin_bytes: projection.headroom.admission_bytes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurableStorageAdmissionProjection {
    admitted_repository_bytes: u64,
    projected_repository_bytes: u64,
    admitted_catalog_bytes: u64,
    projected_catalog_bytes: u64,
    usable_free_bytes: u64,
    headroom: DurableStorageHeadroom,
}

fn storage_admission_projection(
    inventory: &DurableStorageInventory,
    repository: RepositoryId,
    policy: DurableStorageAdmissionPolicy,
) -> DurableStorageAdmissionProjection {
    let repository_storage = inventory
        .repositories
        .iter()
        .find(|storage| storage.repository == repository);
    let admitted_repository_bytes = repository_storage.map_or(0, |storage| {
        storage
            .physical_bytes
            .saturating_add(storage.inflight_reservation_bytes)
    });
    let projected_repository_bytes =
        admitted_repository_bytes.saturating_add(policy.required_repository_bytes);
    let admitted_catalog_bytes = inventory
        .total_physical_bytes
        .saturating_add(inventory.inflight_catalog_reservation_bytes);
    let projected_catalog_bytes =
        admitted_catalog_bytes.saturating_add(policy.required_catalog_bytes);
    let usable_free_bytes = inventory
        .available_bytes
        .saturating_sub(policy.minimum_free_bytes)
        .saturating_sub(inventory.inflight_catalog_reservation_bytes);
    let repository_bytes = policy
        .maximum_repository_bytes
        .saturating_sub(projected_repository_bytes);
    let catalog_bytes = policy
        .maximum_storage_bytes
        .saturating_sub(projected_catalog_bytes);
    let filesystem_bytes = usable_free_bytes.saturating_sub(policy.required_catalog_bytes);
    DurableStorageAdmissionProjection {
        admitted_repository_bytes,
        projected_repository_bytes,
        admitted_catalog_bytes,
        projected_catalog_bytes,
        usable_free_bytes,
        headroom: DurableStorageHeadroom {
            repository_bytes,
            catalog_bytes,
            filesystem_bytes,
            admission_bytes: repository_bytes.min(catalog_bytes).min(filesystem_bytes),
        },
    }
}

pub(super) fn storage_headroom(
    inventory: &DurableStorageInventory,
    repository: RepositoryId,
    policy: DurableStorageAdmissionPolicy,
) -> DurableStorageHeadroom {
    storage_admission_projection(inventory, repository, policy).headroom
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

fn read_generation_bootstrap_identity(
    repository: &PrivateDirectory<'_>,
    repository_id: RepositoryId,
    generation: GenerationId,
) -> Result<Option<ContentHash>, FirstSliceError> {
    let Ok(generation_directory) =
        PrivateDirectory::open(repository.capability(), OsStr::new(&generation.to_string()))
    else {
        return Ok(None);
    };
    let Ok(bytes) =
        generation_directory.read_file_bounded(OsStr::new(MANIFEST_FILENAME), MAX_MANIFEST_BYTES)
    else {
        return Ok(None);
    };
    let Ok(manifest) = serde_json::from_slice::<DurableGenerationManifest>(&bytes) else {
        return Ok(None);
    };
    let version_is_valid = source_storage_layout(manifest.version, manifest.source_storage).is_ok();
    if manifest.receipt.repository != repository_id
        || manifest.receipt.generation != generation
        || !valid_repository_root_path(manifest.root_path.as_deref())
        || !version_is_valid
    {
        return Ok(None);
    }
    // Persisted U+FFFD may represent a lossy OS path, so only lossless root
    // spellings can provide the redundant identity binding.
    if matches!(
        manifest.version,
        PACKED_GENERATION_MANIFEST_VERSION | GENERATION_MANIFEST_VERSION
    ) && let Some(root_path) = manifest.root_path.as_deref()
        && !root_path.contains('\u{fffd}')
        && (!Path::new(root_path).is_absolute()
            || persisted_repository_path_hash(root_path)? != manifest.root_identity)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(Some(manifest.root_identity))
}

fn persisted_repository_path_hash(root_path: &str) -> Result<ContentHash, FirstSliceError> {
    #[cfg(not(windows))]
    let identity_path = PathBuf::from(root_path);
    #[cfg(windows)]
    let identity_path = root_path.strip_prefix(r"\\").map_or_else(
        || PathBuf::from(format!(r"\\?\{root_path}")),
        |unc| PathBuf::from(format!(r"\\?\UNC\{unc}")),
    );
    repository_path_hash(&identity_path).map_err(|_| FirstSliceError::CatalogCorrupt)
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
        match source_storage_layout(manifest.version, manifest.source_storage)? {
            DurableSourceLayout::Inline | DurableSourceLayout::Packed(_) => continue,
            DurableSourceLayout::Blobs => {}
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

fn recovery_sidecar_memory_bytes(
    manifest: &DurableGenerationManifest,
) -> Result<u64, FirstSliceError> {
    // Incremental state is an optional cache: an invalid descriptor is ignored
    // by its loader, whereas a source-file catalog is part of query authority.
    let incremental = manifest
        .incremental_state
        .filter(|descriptor| {
            descriptor.bytes > 0 && descriptor.bytes <= MAX_INCREMENTAL_STATE_BYTES
        })
        .map_or(0, |descriptor| descriptor.bytes);
    let source_catalog = match manifest.source_file_catalog {
        Some(descriptor)
            if descriptor.bytes == 0 || descriptor.bytes > MAX_SOURCE_FILE_CATALOG_BYTES =>
        {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        Some(descriptor) => descriptor.bytes,
        None => 0,
    };
    let packed_index = manifest
        .source_storage
        .and_then(|storage| storage.index_bytes)
        .unwrap_or(0);
    let file_entries = if manifest.source_file_catalog.is_some() {
        manifest
            .receipt
            .indexed_files
            .checked_mul(super::SOURCE_FILE_FALLBACK_ENTRY_MEMORY_BYTES)
            .ok_or(FirstSliceError::Limits)?
    } else {
        0
    };
    // Include encoded/owned sidecar representations and the existing file-only
    // path/identity charge. Backend allocation limits remain independently active.
    incremental
        .checked_add(source_catalog)
        .and_then(|bytes| bytes.checked_add(packed_index))
        .and_then(|bytes| bytes.checked_mul(8))
        .and_then(|bytes| bytes.checked_add(file_entries))
        .and_then(|bytes| bytes.checked_add(super::GENERATION_MEMORY_FIXED_OVERHEAD_BYTES))
        .ok_or(FirstSliceError::Limits)
}

fn recovery_snapshot_memory_bytes(
    plan: &RecoverySnapshotPlan,
    file_only: bool,
) -> Result<u64, FirstSliceError> {
    // JSON-gzip recovery retains both encoded and decoded buffers. MessagePack
    // streams decoding, but reserves the same envelope for verification work.
    // File-only snapshots also retain normalized paths outside the sidecar.
    let document_copies = if file_only { 9 } else { 1 };
    plan.serialized_document_bytes
        .checked_mul(document_copies)
        .and_then(|bytes| bytes.checked_add(plan.encoded_bytes))
        .and_then(|bytes| bytes.checked_add(plan.decoded_bytes))
        .ok_or(FirstSliceError::Limits)
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
        generation_cache,
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
    let mut manifest: DurableGenerationManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let source_layout = source_storage_layout(manifest.version, manifest.source_storage)?;
    if manifest.version != GENERATION_MANIFEST_VERSION && manifest.source_file_catalog.is_some() {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    if manifest.receipt.repository != repository
        || manifest.receipt.generation != generation
        || !valid_repository_root_path(manifest.root_path.as_deref())
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let mut recovery_plan = read_recovery_snapshot_plan(
        &generation_directory,
        repository,
        generation,
        manifest.receipt.parent,
        cancellation,
    );
    let staging_bytes = recovery_sidecar_memory_bytes(&manifest)?;
    let snapshot_bytes = match &recovery_plan {
        Ok(Some(plan)) => {
            recovery_snapshot_memory_bytes(plan, manifest.source_file_catalog.is_some())?
        }
        Ok(None) | Err(FirstSliceError::CatalogCorrupt) => 0,
        Err(error) => return Err(*error),
    };
    let mut memory_reservation = generation_cache
        .map(|cache| {
            let bytes = staging_bytes
                .checked_add(snapshot_bytes)
                .ok_or(FirstSliceError::Limits)?;
            match super::RestoredMemoryReservation::new(Arc::clone(cache), bytes, true) {
                Err(FirstSliceError::GenerationMemoryLimit { .. })
                    if snapshot_bytes > 0 && manifest.receipt.oracle_allocated_bytes > 0 =>
                {
                    // A snapshot is only an accelerator when an authoritative
                    // oracle exists. Its larger decode envelope must not strand
                    // a generation whose oracle can fit the remaining budget.
                    recovery_plan = Ok(None);
                    super::RestoredMemoryReservation::new(Arc::clone(cache), staging_bytes, true)
                }
                result => result,
            }
        })
        .transpose()?;
    let incremental = match restore_incremental_state(
        &generation_directory,
        manifest.incremental_state,
        cancellation,
    ) {
        Ok(incremental) => incremental,
        Err(FirstSliceError::CatalogCorrupt) => None,
        Err(error) => return Err(error),
    };
    let source_file_catalog = restore_source_file_catalog(
        &generation_directory,
        manifest.source_file_catalog,
        repository,
        generation,
        cancellation,
    )?;

    let context = GenerationContext::new(cancellation, GenerationBudget::default());
    let generation_path = repository_path.join(&generation_name);
    let recovered = recovery_plan.and_then(|plan| {
        plan.map(|plan| {
            restore_recovery_generation(
                &generation_directory,
                &plan,
                &source_file_catalog,
                &context,
                cancellation,
            )
        })
        .transpose()
    });
    // A zero oracle charge is the persisted discriminator for semantic
    // generations whose checksummed recovery snapshot is authoritative.
    let (verified, allocated_bytes, recovery_serialized_document_bytes) =
        if manifest.receipt.oracle_allocated_bytes == 0 {
            match recovered {
                Ok(Some((verified, serialized_document_bytes))) => {
                    (verified, 0, Some(serialized_document_bytes))
                }
                Ok(None) | Err(FirstSliceError::CatalogCorrupt) => {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                Err(error) => return Err(error),
            }
        } else {
            match recovered {
                Ok(Some((verified, serialized_document_bytes))) => (
                    verified,
                    manifest.receipt.oracle_allocated_bytes,
                    Some(serialized_document_bytes),
                ),
                Ok(None) | Err(FirstSliceError::CatalogCorrupt) => restore_oracle_generation(
                    &generation_path,
                    OracleRestoreExpectation {
                        repository,
                        generation,
                        parent: manifest.receipt.parent,
                        allocated_bytes: manifest.receipt.oracle_allocated_bytes,
                    },
                    source_file_catalog,
                    &context,
                    cancellation,
                    memory_reservation
                        .as_mut()
                        .map(|reservation| (reservation, staging_bytes)),
                )
                .map(|(verified, allocated_bytes)| (verified, allocated_bytes, None))?,
                Err(error) => return Err(error),
            }
        };
    let restored_logical_snapshot =
        restore_logical_snapshot_identity(&generation_directory, verified.snapshot())?;
    let (logical_snapshot, logical_serialized_document_bytes) =
        if let Some(restored) = restored_logical_snapshot {
            (Some(restored.identity), restored.serialized_document_bytes)
        } else {
            (None, None)
        };
    let serialized_document_bytes = reconcile_restored_serialized_document_bytes(
        recovery_serialized_document_bytes,
        logical_serialized_document_bytes,
    )?;
    manifest.receipt.logical_snapshot = logical_snapshot;
    if manifest.receipt.logical_snapshot.is_some() && incremental.is_none() {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let (search, lexical_documents) = if verified.snapshot().source_files().is_empty() {
        restore_normalized_search(
            verified.snapshot(),
            repository_directory,
            &generation_directory,
            repository,
            generation,
            source_layout,
            cancellation,
        )?
    } else {
        restore_file_only_search(
            verified.snapshot(),
            repository_directory,
            &generation_directory,
            repository,
            source_layout,
            cancellation,
        )?
    };
    let sources = Vec::new();
    if u64::try_from(verified.snapshot().file_count()).ok() != Some(manifest.receipt.indexed_files)
        || u64::try_from(verified.document().entities.len()).ok() != Some(manifest.receipt.entities)
        || lexical_documents != manifest.receipt.lexical_documents
        || allocated_bytes != manifest.receipt.oracle_allocated_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    // The checksummed recovery snapshot and freshly built lexical projection are
    // the query authorities. Rehashing every canonical logical component here
    // only revalidates source-free observability evidence and can turn a bounded
    // active-generation open into minutes of hidden startup work.
    let mut restored = RestoredGeneration {
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
        serialized_document_bytes,
        search,
        sources,
        incremental,
        operations: Vec::new(),
        memory_reservation: memory_reservation.take(),
    };
    if restored.memory_reservation.is_some() {
        let memory_bytes = super::restored_generation_memory_bytes(&mut restored)?;
        if let Some(reservation) = restored.memory_reservation.as_mut() {
            reservation.resize(memory_bytes, true)?;
        }
    }
    Ok(restored)
}

fn restore_normalized_search(
    verified: &GenerationSnapshot,
    repository_directory: &PrivateDirectory<'_>,
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    generation: GenerationId,
    source_layout: DurableSourceLayout,
    cancellation: &Cancellation,
) -> Result<(LexicalIndex, u64), FirstSliceError> {
    let mut projection =
        LexicalProjectionBuilder::new(verified, BuildBudget::default(), cancellation)
            .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
    // Retaining the validated directory capabilities avoids reopening the same
    // private parents for every unsupported file in a large generation.
    let source_reader = if projection.next_source_file().is_none() {
        None
    } else {
        Some(PersistedSourceReader::open(
            repository_directory,
            generation_directory,
            repository,
            source_layout,
        )?)
    };
    while let Some(file_id) = projection.next_source_file() {
        let file = verified
            .find_file(file_id)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let snapshot = source_reader
            .as_ref()
            .ok_or(FirstSliceError::CatalogCorrupt)?
            .read(file, cancellation)?;
        projection
            .push_source(&snapshot, cancellation)
            .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
    }
    let documents = projection
        .finish(cancellation)
        .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
    let lexical_documents = u64::try_from(documents.len()).map_err(|_| FirstSliceError::Limits)?;
    let search =
        LexicalIndex::build_ephemeral(generation, documents, BuildBudget::default(), cancellation)
            .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    Ok((search, lexical_documents))
}

fn restore_file_only_search(
    verified: &GenerationSnapshot,
    repository_directory: &PrivateDirectory<'_>,
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    source_layout: DurableSourceLayout,
    cancellation: &Cancellation,
) -> Result<(LexicalIndex, u64), FirstSliceError> {
    if !verified
        .document()
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let source_reader = PersistedSourceReader::open(
        repository_directory,
        generation_directory,
        repository,
        source_layout,
    )?;
    let text_limit = source_fallback_text_limit(verified)?;
    let budget = BuildBudget::default();
    let mut builder =
        EphemeralLexicalIndexBuilder::new(verified.metadata().generation(), budget, cancellation)
            .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    let mut file_ids = Vec::new();
    file_ids
        .try_reserve_exact(verified.file_count())
        .map_err(|_| FirstSliceError::Retention)?;
    file_ids.extend(verified.document().files.iter().map(|file| file.id));
    file_ids.extend(
        verified
            .source_files()
            .entries()
            .iter()
            .map(|entry| entry.file().id),
    );
    file_ids.sort_unstable();
    if file_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let partition_files = STREAMED_SOURCE_PARTITION_FILES.min(budget.max_documents);
    let mut partition = Vec::new();
    partition
        .try_reserve_exact(partition_files)
        .map_err(|_| FirstSliceError::Retention)?;
    let mut lexical_documents = 0_u64;
    for file_id in file_ids {
        check_cancellation(cancellation)?;
        let file = verified
            .find_file(file_id)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let snapshot = source_reader.read(file, cancellation)?;
        let document = project_source_fallback_document_with_text_limit(
            verified,
            &snapshot,
            text_limit,
            budget,
            cancellation,
        )
        .map_err(|error| generation_data_error(map_query_error(error, cancellation)))?;
        partition.push(document);
        lexical_documents = lexical_documents
            .checked_add(1)
            .ok_or(FirstSliceError::Limits)?;
        if partition.len() == partition_files {
            builder
                .push_partition(std::mem::take(&mut partition), cancellation)
                .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
            partition
                .try_reserve_exact(partition_files)
                .map_err(|_| FirstSliceError::Retention)?;
        }
    }
    if !partition.is_empty() {
        builder
            .push_partition(partition, cancellation)
            .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    }
    let search = builder
        .finish(cancellation)
        .map_err(|error| generation_data_error(map_search_error(error, cancellation)))?;
    Ok((search, lexical_documents))
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
    decode_incremental_state(&bytes, cancellation).map(Some)
}

fn decode_incremental_state(
    bytes: &[u8],
    cancellation: &Cancellation,
) -> Result<PreparedIncrementalState, FirstSliceError> {
    let header: DurableIncrementalStateHeader =
        serde_json::from_slice(bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    match header.version {
        LEGACY_INCREMENTAL_STATE_VERSION => {
            let durable: LegacyDurableIncrementalState =
                serde_json::from_slice(bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            durable.into_prepared(cancellation)
        }
        INCREMENTAL_STATE_VERSION => {
            let durable: DurableIncrementalState =
                serde_json::from_slice(bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            durable.into_prepared(cancellation)
        }
        _ => Err(FirstSliceError::CatalogCorrupt),
    }
}

fn restore_source_file_catalog(
    generation_directory: &PrivateDirectory<'_>,
    descriptor: Option<DurableSidecarDescriptor>,
    repository: RepositoryId,
    generation: GenerationId,
    cancellation: &Cancellation,
) -> Result<SourceFileCatalog, FirstSliceError> {
    let Some(descriptor) = descriptor else {
        return Ok(SourceFileCatalog::default());
    };
    if descriptor.bytes == 0 || descriptor.bytes > MAX_SOURCE_FILE_CATALOG_BYTES {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    let bytes = generation_directory
        .read_file_bounded_cancellable(
            OsStr::new(SOURCE_FILE_CATALOG_FILENAME),
            descriptor.bytes,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    if u64::try_from(bytes.len()).ok() != Some(descriptor.bytes)
        || content_hash_bytes(&bytes) != descriptor.digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    check_cancellation(cancellation)?;
    let header: DurableSourceFileCatalogHeader =
        serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    match header.version {
        LEGACY_SOURCE_FILE_CATALOG_VERSION => {
            let durable: LegacyDurableSourceFileCatalog =
                serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            durable.into_catalog(repository, generation)
        }
        SOURCE_FILE_CATALOG_VERSION => {
            let durable: DurableSourceFileCatalog =
                serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
            durable.into_catalog(repository, generation)
        }
        _ => Err(FirstSliceError::CatalogCorrupt),
    }
}

fn reconcile_restored_serialized_document_bytes(
    recovery: Option<u64>,
    logical: Option<u64>,
) -> Result<Option<u64>, FirstSliceError> {
    match (recovery, logical) {
        (Some(recovery), Some(logical)) if recovery != logical => {
            Err(FirstSliceError::CatalogCorrupt)
        }
        (Some(recovery), Some(_)) => Ok(Some(recovery)),
        _ => Ok(None),
    }
}

fn validate_recovery_document_accounting(
    decoded_bytes: u64,
    serialized_document_bytes: u64,
) -> Result<u64, FirstSliceError> {
    if decoded_bytes == 0
        || decoded_bytes > MAX_RECOVERY_SNAPSHOT_BYTES
        || serialized_document_bytes < decoded_bytes
        || serialized_document_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(serialized_document_bytes)
}

fn restore_logical_snapshot_identity(
    generation_directory: &PrivateDirectory<'_>,
    snapshot: &GenerationSnapshot,
) -> Result<Option<RestoredLogicalSnapshotIdentity>, FirstSliceError> {
    let metadata = match generation_directory
        .capability()
        .symlink_metadata(Path::new(LOGICAL_SNAPSHOT_FILENAME))
    {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(FirstSliceError::CatalogCorrupt),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_LOGICAL_SNAPSHOT_BYTES
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let bytes = generation_directory
        .read_file_bounded(
            OsStr::new(LOGICAL_SNAPSHOT_FILENAME),
            MAX_LOGICAL_SNAPSHOT_BYTES,
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
    let sidecar: DurableLogicalSnapshotSidecar =
        serde_json::from_slice(&bytes).map_err(|_| FirstSliceError::CatalogCorrupt)?;
    if content_hash_bytes(sidecar.payload.as_bytes()) != sidecar.digest {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let Ok(descriptor) = serde_json::from_str::<DurableLogicalSnapshotIdentity>(&sidecar.payload)
    else {
        // A checksum-valid future observability descriptor cannot strand the
        // otherwise valid immutable generation.
        return Ok(None);
    };
    let serialized_document_bytes = match descriptor.version {
        LEGACY_LOGICAL_SNAPSHOT_VERSION
            if descriptor.schema_version == "1.0"
                && descriptor.serialized_document_bytes.is_none() =>
        {
            None
        }
        LOGICAL_SNAPSHOT_VERSION if descriptor.schema_version == "1.0" => Some(
            descriptor
                .serialized_document_bytes
                .filter(|bytes| *bytes > 0 && *bytes <= MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES)
                .ok_or(FirstSliceError::CatalogCorrupt)?,
        ),
        _ => return Ok(None),
    };
    let generation = snapshot.metadata();
    let contract = generation.contract_version();
    if descriptor.repository != generation.repository()
        || descriptor.generation != generation.generation()
        || descriptor.parent != generation.parent()
        || descriptor.contract_major != contract.major()
        || descriptor.contract_minor != contract.minor()
        || descriptor.manifest_hash != generation.manifest_hash()
        || descriptor.configuration_hash != generation.configuration_hash()
        || descriptor.provider_set_hash != generation.provider_set_hash()
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    Ok(Some(RestoredLogicalSnapshotIdentity {
        identity: FirstSliceLogicalSnapshotIdentity::new(descriptor.hash),
        serialized_document_bytes,
    }))
}

fn read_recovery_snapshot_plan(
    generation_directory: &PrivateDirectory<'_>,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    cancellation: &Cancellation,
) -> Result<Option<RecoverySnapshotPlan>, FirstSliceError> {
    check_cancellation(cancellation)?;
    let names = private_entry_names(generation_directory)?;
    if !names
        .iter()
        .any(|name| name == OsStr::new(RECOVERY_MANIFEST_FILENAME))
    {
        return Ok(None);
    }
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
    let (snapshot_name, decoded_bytes, decoded_digest, serialized_document_bytes, format) =
        match recovery.version {
            LEGACY_RECOVERY_SNAPSHOT_VERSION
                if recovery.encoding.is_none()
                    && recovery.decoded_bytes.is_none()
                    && recovery.decoded_digest.is_none()
                    && recovery.serialized_document_bytes.is_none()
                    && recovery.bytes > 0
                    && recovery.bytes <= MAX_RECOVERY_SNAPSHOT_BYTES =>
            {
                (
                    RECOVERY_SNAPSHOT_FILENAME,
                    recovery.bytes,
                    recovery.digest,
                    recovery.bytes,
                    RecoverySnapshotFormat::Json,
                )
            }
            JSON_GZIP_RECOVERY_SNAPSHOT_VERSION
                if matches!(recovery.encoding, Some(RecoverySnapshotEncoding::Gzip))
                    && recovery.serialized_document_bytes.is_none()
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
                    decoded_bytes,
                    RecoverySnapshotFormat::JsonGzip,
                )
            }
            RECOVERY_SNAPSHOT_VERSION
                if matches!(
                    recovery.encoding,
                    Some(RecoverySnapshotEncoding::MessagePackGzip)
                ) && recovery.bytes > 0
                    && recovery.bytes <= MAX_RECOVERY_ENCODED_BYTES =>
            {
                let decoded_bytes = recovery
                    .decoded_bytes
                    .filter(|bytes| *bytes > 0 && *bytes <= MAX_RECOVERY_SNAPSHOT_BYTES)
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                let decoded_digest = recovery
                    .decoded_digest
                    .ok_or(FirstSliceError::CatalogCorrupt)?;
                let serialized_document_bytes = validate_recovery_document_accounting(
                    decoded_bytes,
                    recovery
                        .serialized_document_bytes
                        .ok_or(FirstSliceError::CatalogCorrupt)?,
                )?;
                (
                    RECOVERY_SNAPSHOT_MESSAGEPACK_GZIP_FILENAME,
                    decoded_bytes,
                    decoded_digest,
                    serialized_document_bytes,
                    RecoverySnapshotFormat::MessagePackGzip,
                )
            }
            _ => return Err(FirstSliceError::CatalogCorrupt),
        };
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
    Ok(Some(RecoverySnapshotPlan {
        snapshot_name,
        encoded_bytes: recovery.bytes,
        encoded_digest: recovery.digest,
        decoded_bytes,
        decoded_digest,
        serialized_document_bytes,
        format,
        metadata,
    }))
}

fn restore_recovery_generation(
    generation_directory: &PrivateDirectory<'_>,
    plan: &RecoverySnapshotPlan,
    source_files: &SourceFileCatalog,
    context: &GenerationContext<'_>,
    cancellation: &Cancellation,
) -> Result<(IdentityVerifiedGeneration, u64), FirstSliceError> {
    let encoded = generation_directory
        .read_file_bounded_cancellable(
            OsStr::new(plan.snapshot_name),
            plan.encoded_bytes,
            cancellation,
        )
        .map_err(map_private_read_error)?;
    if u64::try_from(encoded.len()).ok() != Some(plan.encoded_bytes)
        || content_hash_bytes(&encoded) != plan.encoded_digest
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let mut recovery_limits = IrLimits::default();
    // Published in-memory documents may legitimately serialize beyond the
    // default import envelope. The sidecar supplies the exact checksummed
    // length, still capped by the recovery hard bound.
    recovery_limits.max_document_bytes =
        usize::try_from(plan.decoded_bytes).map_err(|_| FirstSliceError::Limits)?;
    let restored = match plan.format {
        RecoverySnapshotFormat::Json => {
            IdentityVerifiedGeneration::restore_published_json_with_source_files(
                plan.metadata,
                &encoded,
                plan.decoded_digest,
                source_files.clone(),
                &recovery_limits,
                &ExtensionSupport::default(),
                context,
            )
        }
        RecoverySnapshotFormat::JsonGzip => {
            let decoded = decode_recovery_snapshot(&encoded, plan.decoded_bytes, cancellation)?;
            IdentityVerifiedGeneration::restore_published_json_with_source_files(
                plan.metadata,
                &decoded,
                plan.decoded_digest,
                source_files.clone(),
                &recovery_limits,
                &ExtensionSupport::default(),
                context,
            )
        }
        RecoverySnapshotFormat::MessagePackGzip => {
            let reader = BufReader::with_capacity(
                RECOVERY_WRITE_BUFFER_BYTES,
                GzDecoder::new(encoded.as_slice()),
            );
            IdentityVerifiedGeneration::restore_published_messagepack_reader_with_source_files(
                plan.metadata,
                reader,
                usize::try_from(plan.decoded_bytes).map_err(|_| FirstSliceError::Limits)?,
                plan.decoded_digest,
                source_files.clone(),
                &recovery_limits,
                &ExtensionSupport::default(),
                context,
            )
        }
    };
    let restored = restored.map_err(|error| map_persisted_identity_error(error, cancellation))?;
    // The caller may reuse this charge only after the independently persisted
    // logical descriptor confirms it; otherwise it recomputes the exact JSON
    // measure from the verified document.
    Ok((restored, plan.serialized_document_bytes))
}

fn restore_oracle_generation(
    generation_path: &Path,
    expectation: OracleRestoreExpectation,
    source_files: SourceFileCatalog,
    context: &GenerationContext<'_>,
    cancellation: &Cancellation,
    admission: Option<(&mut super::RestoredMemoryReservation, u64)>,
) -> Result<(IdentityVerifiedGeneration, u64), FirstSliceError> {
    let oracle = OracleReader::open_in(generation_path, context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let allocated_bytes = oracle
        .allocated_bytes(context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    if let Some((reservation, staging_bytes)) = admission {
        let stats = oracle.stats();
        // Oracle header validation checks text lengths without materializing the
        // document; read() checks the same header and row cardinalities again.
        // Charge fixed record/association work and worst-case JSON text escaping,
        // not SQLite file size. This logical envelope is not an exact RSS bound.
        let text_copies = if source_files.is_empty() { 6 } else { 14 };
        let bytes = stats
            .stored_rows()
            .checked_mul(4 * 1024)
            .and_then(|bytes| bytes.checked_add(stats.text_bytes().checked_mul(text_copies)?))
            .and_then(|bytes| bytes.checked_add(staging_bytes))
            .ok_or(FirstSliceError::Limits)?;
        reservation.resize(bytes, true)?;
    }
    let persisted = oracle
        .read(context)
        .map_err(|error| map_catalog_error(&error, cancellation))?;
    let metadata = persisted.metadata();
    if metadata.repository() != expectation.repository
        || metadata.generation() != expectation.generation
        || metadata.parent() != expectation.parent
        || allocated_bytes != expectation.allocated_bytes
    {
        return Err(FirstSliceError::CatalogCorrupt);
    }
    let persisted = persisted
        .with_source_files(source_files)
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
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

impl PersistedSourceReader {
    fn open(
        repository_directory: &PrivateDirectory<'_>,
        generation_directory: &PrivateDirectory<'_>,
        repository: RepositoryId,
        source_layout: DurableSourceLayout,
    ) -> Result<Self, FirstSliceError> {
        let sources = PrivateDirectory::open(
            generation_directory.capability(),
            OsStr::new(SOURCES_DIRECTORY),
        )
        .map_err(|_| FirstSliceError::CatalogCorrupt)?;
        let layout = match source_layout {
            DurableSourceLayout::Inline => PersistedSourceLayout::Inline,
            DurableSourceLayout::Blobs => {
                let blobs = PrivateDirectory::open(
                    repository_directory.capability(),
                    OsStr::new(SOURCE_BLOBS_DIRECTORY),
                )
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                PersistedSourceLayout::Blobs(blobs)
            }
            DurableSourceLayout::Packed(storage) => PersistedSourceLayout::Packed {
                index: read_packed_source_index(&sources, storage)?,
                cached_pack: Mutex::new(None),
            },
        };
        Ok(Self {
            repository,
            sources,
            layout,
        })
    }

    fn read(
        &self,
        file: &FileRecord,
        cancellation: &Cancellation,
    ) -> Result<SourceSnapshot, FirstSliceError> {
        check_cancellation(cancellation)?;
        if file.repository != self.repository
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
        let bytes = match &self.layout {
            PersistedSourceLayout::Inline => self
                .sources
                .read_file_bounded_cancellable(
                    OsStr::new(&file.id.to_string()),
                    file.byte_length,
                    cancellation,
                )
                .map_err(map_private_read_error)?,
            PersistedSourceLayout::Blobs(blobs) => {
                let persisted = self
                    .sources
                    .read_file_bounded_cancellable(
                        OsStr::new(&file.id.to_string()),
                        MAX_SOURCE_POINTER_BYTES,
                        cancellation,
                    )
                    .map_err(map_private_read_error)?;
                let pointer = decode_source_pointer(&persisted)?;
                if pointer.digest != file.content_hash || pointer.bytes != file.byte_length {
                    return Err(FirstSliceError::CatalogCorrupt);
                }
                let blob = PrivateDirectory::open(
                    blobs.capability(),
                    OsStr::new(&pointer.digest.to_string()),
                )
                .map_err(|_| FirstSliceError::CatalogCorrupt)?;
                blob.read_file_bounded_cancellable(
                    OsStr::new(SOURCE_BLOB_PAYLOAD_FILENAME),
                    pointer.bytes,
                    cancellation,
                )
                .map_err(map_private_read_error)?
            }
            PersistedSourceLayout::Packed { index, cached_pack } => {
                self.read_packed_source(file, index, cached_pack, cancellation)?
            }
        };
        if u64::try_from(bytes.len()).ok() != Some(file.byte_length) {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        SourceSnapshot::from_persisted(self.repository, path, file.id, file.content_hash, bytes)
            .map_err(|error| generation_data_error(map_vfs_error(error, cancellation)))
    }

    fn read_packed_source(
        &self,
        file: &FileRecord,
        index: &PackedSourceIndex,
        cached_pack: &Mutex<Option<CachedSourcePack>>,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, FirstSliceError> {
        let entry = index
            .entries
            .get(&file.id)
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        if entry.digest != file.content_hash || entry.bytes != file.byte_length {
            return Err(FirstSliceError::CatalogCorrupt);
        }
        let expected_pack_bytes = index
            .pack_bytes
            .get(usize::try_from(entry.pack).map_err(|_| FirstSliceError::Limits)?)
            .copied()
            .ok_or(FirstSliceError::CatalogCorrupt)?;
        let mut cached = cached_pack.lock().map_err(|_| FirstSliceError::Catalog)?;
        if cached.as_ref().map(|pack| pack.ordinal) != Some(entry.pack) {
            let bytes = self
                .sources
                .read_file_bounded_cancellable(
                    OsStr::new(&source_pack_name(entry.pack)),
                    expected_pack_bytes,
                    cancellation,
                )
                .map_err(map_private_read_error)?;
            if u64::try_from(bytes.len()).ok() != Some(expected_pack_bytes) {
                return Err(FirstSliceError::CatalogCorrupt);
            }
            verify_source_pack(&bytes, entry.pack, index)?;
            *cached = Some(CachedSourcePack {
                ordinal: entry.pack,
                bytes,
            });
        }
        let pack = cached.as_ref().ok_or(FirstSliceError::CatalogCorrupt)?;
        let start = usize::try_from(entry.offset).map_err(|_| FirstSliceError::Limits)?;
        let end = entry
            .offset
            .checked_add(entry.bytes)
            .and_then(|end| usize::try_from(end).ok())
            .ok_or(FirstSliceError::Limits)?;
        pack.bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(FirstSliceError::CatalogCorrupt)
    }
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
) -> Result<PublishedActivationMarker, FirstSliceError> {
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
            let bytes = u64::try_from(bytes.len()).map_err(|_| FirstSliceError::Limits)?;
            Ok(PublishedActivationMarker {
                marker: ActivationMarker {
                    name: OsString::from(marker_name),
                    sequence: repository_activation_sequence,
                    manifest,
                },
                bytes,
            })
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
    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{
        FileIdentity, GenerationIdentity, content_hash, derive_fact, derive_file,
        derive_generation, derive_repository,
    };
    use rootlight_query::LocateMode;
    use rootlight_runtime::RuntimePaths;
    use std::{fs, io, time::Duration};
    use tempfile::TempDir;

    #[test]
    fn storage_scan_budget_stops_after_inflight_cancellation() {
        for reason in [
            CancellationReason::Shutdown,
            CancellationReason::ClientRequest,
        ] {
            let mut budget = StorageScanBudget::new();
            budget.visit().expect("first entry is admitted");
            let _ = budget.cancellation.cancel(reason);
            assert_eq!(budget.visit(), Err(FirstSliceError::Cancelled(reason)));
            assert_eq!(budget.visited_entries, 1, "cancelled work is not admitted");
        }
    }

    #[test]
    fn storage_scan_tree_rejects_cancelled_traversal() {
        let root = durable_test_tempdir();
        let paths = RuntimePaths::new(root.path().join("state"), root.path().join("runtime"))
            .expect("fixture paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let parent = Dir::open_ambient_dir(paths.state_dir(), ambient_authority())
            .expect("fixture parent opens");
        let directory =
            PrivateDirectory::create(&parent, OsStr::new("scan")).expect("fixture directory opens");
        fs::write(
            paths.state_dir().join("scan/payload"),
            b"bounded inventory fixture",
        )
        .expect("fixture writes");
        let mut budget = StorageScanBudget::new();
        let _ = budget.cancellation.cancel(CancellationReason::Shutdown);
        assert_eq!(
            directory_tree_bytes(&directory, &mut budget),
            Err(FirstSliceError::Cancelled(CancellationReason::Shutdown))
        );
    }

    #[test]
    fn storage_scan_cancellation_invalidates_cache_and_retry_preserves_inventory() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn inventory_value() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let catalog = service.durable.as_ref().expect("durable catalog exists");
        let repository = PrivateDirectory::open(
            catalog.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .expect("repository opens");
        let names_before = private_entry_names(&repository).expect("repository entries read");
        let visited = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&visited);
        let mut budget = StorageScanBudget::new();
        budget.after_visit = Some(Box::new(move |count| {
            observed.store(
                u64::try_from(count).expect("fixture count fits"),
                Ordering::Relaxed,
            );
        }));
        let baseline = catalog
            .scan_storage_inventory(SourceBlobScan::AccountPhysicalBytes, budget)
            .expect("complete physical inventory reads");
        let total = usize::try_from(visited.load(Ordering::Relaxed)).expect("fixture count fits");
        assert!(total > 2, "fixture exercises a nested scan");

        for reason in [
            CancellationReason::Shutdown,
            CancellationReason::ClientRequest,
        ] {
            for cancel_at in [1, total / 2, total] {
                let cancellation = Cancellation::new();
                let trigger = cancellation.clone();
                let mut budget = StorageScanBudget::with_cancellation(&cancellation);
                budget.after_visit = Some(Box::new(move |count| {
                    if count == cancel_at {
                        let _ = trigger.cancel(reason);
                    }
                }));
                assert_eq!(
                    catalog.scan_storage_inventory(SourceBlobScan::AccountPhysicalBytes, budget),
                    Err(FirstSliceError::Cancelled(reason)),
                );
                assert_eq!(catalog.storage_inventory_cached(), Ok(None));
                assert!(
                    catalog
                        .storage_accounting
                        .lock()
                        .expect("accounting lock")
                        .dirty
                );
                assert!(matches!(
                    service.support_inventory_snapshot_reconciled_with_cancellation(&cancellation),
                    Err(FirstSliceError::Cancelled(observed)) if observed == reason
                ));
                let mut retried = catalog
                    .reconcile_storage_inventory(&Cancellation::new())
                    .expect("uncancelled retry reconciles");
                // Free space belongs to the whole volume and can change outside this catalog.
                retried.available_bytes = baseline.available_bytes;
                assert_eq!(retried, baseline);
                let mut cached = catalog
                    .storage_inventory_cached()
                    .expect("cache reads")
                    .expect("retry restores authoritative accounting");
                cached.available_bytes = baseline.available_bytes;
                assert_eq!(cached, baseline);
                assert_eq!(
                    private_entry_names(&repository).expect("repository entries read"),
                    names_before
                );
            }
        }
        service
            .support_inventory_snapshot_reconciled()
            .expect("compatibility API reconciles");
    }

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

    struct CancellingWriter {
        bytes: usize,
        cancellation: Cancellation,
    }

    impl io::Write for CancellingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(buffer.len())
                .expect("test byte count is representable");
            self.cancellation.cancel(CancellationReason::ClientRequest);
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

    fn compact_incremental_fixture(file_count: usize) -> PreparedIncrementalState {
        let cancellation = Cancellation::new();
        let repository = derive_repository(b"compact-incremental-fixture").id();
        let mut files = Vec::new();
        let mut baseline_inputs = Vec::new();
        for ordinal in 0..file_count {
            let path = format!("src/generated-{ordinal:06}.rs");
            let file = derive_file(FileIdentity {
                repository,
                path_identity: path.as_bytes(),
            })
            .id();
            let path_hash = content_hash(path.as_bytes());
            let content_hash = content_hash(format!("fixture-{ordinal}").as_bytes());
            let ordinal = u64::try_from(ordinal).expect("fixture ordinal is representable");
            let descending = u64::MAX - ordinal;
            files.push(BaselineFile::new(
                FileDescriptor::new(
                    file,
                    path_hash,
                    FileMetadata::trusted_with_change_token(
                        descending,
                        u128::MAX - u128::from(ordinal),
                        u128::MAX - u128::from(ordinal) - 1,
                        PlatformFileIdentity::new(descending, descending - 1),
                    ),
                ),
                content_hash,
            ));
            baseline_inputs.extend([
                InputFingerprint::new(InputKey::FileContent(file), content_hash),
                InputFingerprint::new(InputKey::FilePath(file), path_hash),
            ]);
        }
        baseline_inputs.extend([
            InputFingerprint::new(
                InputKey::ConfigurationRevision,
                content_hash(b"fixture-configuration"),
            ),
            InputFingerprint::new(
                InputKey::AdapterVersion(derive_fact("fixture-adapter", b"adapter").id()),
                content_hash(b"fixture-adapter-revision"),
            ),
        ]);
        let mut analysis_inputs = baseline_inputs.clone();
        analysis_inputs.push(InputFingerprint::new(
            InputKey::SearchRevision,
            content_hash(b"fixture-search-revision"),
        ));
        let metadata = MetadataBaseline::new(
            files,
            ReconcileLimits::new(file_count.max(1)).expect("fixture reconcile limit is valid"),
            &cancellation,
        )
        .expect("fixture metadata baseline is valid");
        let baseline_snapshot =
            InputSnapshot::new(baseline_inputs, PlanningLimits::default(), &cancellation)
                .expect("fixture baseline inputs are valid");
        let analysis_snapshot =
            InputSnapshot::new(analysis_inputs, PlanningLimits::default(), &cancellation)
                .expect("fixture analysis inputs are valid");
        PreparedIncrementalState {
            baseline: IncrementalDiscoveryBaseline::from_validated_parts(
                metadata,
                baseline_snapshot,
            ),
            inputs: analysis_snapshot,
            evidence: FirstSliceIncrementalEvidence {
                strategy: crate::FirstSliceBuildStrategy::Initial,
                input_changes: Vec::new(),
                file_changes: Vec::new(),
                hashed_files: u64::try_from(file_count)
                    .expect("fixture file count is representable"),
                invalidated_domains: Vec::new(),
                invalidated_units: 0,
                fallback_reason: None,
                trace_entries: 0,
                invalidation_trace: Vec::new(),
                parsed_files: 0,
                reused_parser_artifacts: 0,
                reused_parser_artifact_bytes: 0,
                reused_durable_artifact_bytes: 0,
                lowered_files: 0,
                reused_normalized_facts: 0,
                rebuilt_normalized_facts: 0,
                planned_fact_work: Vec::new(),
                normalized_fact_work: Vec::new(),
                structural_cache_retained: false,
            },
        }
    }

    #[test]
    fn compact_incremental_state_elides_reconstructible_file_fingerprints() {
        let cancellation = Cancellation::new();
        let prepared = compact_incremental_fixture(128);
        let compact = DurableIncrementalState::from_prepared(&prepared, &cancellation)
            .expect("compact incremental state is derivable");
        assert!(matches!(
            compact.analysis_files,
            DurableAnalysisFileSelection::AllBaseline
        ));
        assert_eq!(compact.baseline_context_inputs.len(), 2);
        assert_eq!(compact.analysis_context_inputs.len(), 3);

        let legacy = LegacyDurableIncrementalState {
            version: LEGACY_INCREMENTAL_STATE_VERSION,
            baseline_files: compact.baseline_files.clone(),
            baseline_inputs: durable_input_fingerprints(prepared.baseline.inputs()),
            analysis_inputs: durable_input_fingerprints(&prepared.inputs),
            evidence: prepared.evidence.clone(),
        };
        let legacy_bytes =
            serde_json::to_vec(&legacy).expect("legacy incremental state serializes");
        let compact_bytes =
            serde_json::to_vec(&compact).expect("compact incremental state serializes");
        assert!(
            compact_bytes
                .len()
                .checked_mul(2)
                .is_some_and(|twice| twice < legacy_bytes.len()),
            "compact state should remove most repeated file fingerprint bytes"
        );

        let single = compact_incremental_fixture(1);
        let single_compact = DurableIncrementalState::from_prepared(&single, &cancellation)
            .expect("single-file compact state is derivable");
        let single_legacy = LegacyDurableIncrementalState {
            version: LEGACY_INCREMENTAL_STATE_VERSION,
            baseline_files: single_compact.baseline_files.clone(),
            baseline_inputs: durable_input_fingerprints(single.baseline.inputs()),
            analysis_inputs: durable_input_fingerprints(&single.inputs),
            evidence: single.evidence.clone(),
        };
        let single_compact_bytes =
            serde_json::to_vec(&single_compact).expect("single compact state serializes");
        let single_legacy_bytes =
            serde_json::to_vec(&single_legacy).expect("single legacy state serializes");
        let additional_files = 127_u64;
        // Reserve the non-file revision slots retained by this representative state.
        let maximum_files = u64::try_from(MAX_SOURCE_BLOB_ENTRIES / 2 - 8)
            .expect("durable file ceiling is representable");
        let project = |single: usize, sample: usize| {
            let incremental = u64::try_from(sample - single)
                .expect("sample delta is representable")
                .div_ceil(additional_files);
            u64::try_from(single)
                .expect("single state length is representable")
                .checked_add(incremental.saturating_mul(maximum_files - 1))
                .and_then(|bytes| bytes.checked_add(1_024))
                .expect("projected state length is representable")
        };
        let compact_projection = project(single_compact_bytes.len(), compact_bytes.len());
        let legacy_projection = project(single_legacy_bytes.len(), legacy_bytes.len());
        assert!(compact_projection <= MAX_INCREMENTAL_STATE_BYTES);
        assert!(legacy_projection > MAX_INCREMENTAL_STATE_BYTES);

        for (format, encoded) in [("legacy", &legacy_bytes), ("compact", &compact_bytes)] {
            let restored = decode_incremental_state(encoded, &cancellation)
                .unwrap_or_else(|error| panic!("{format} incremental state restores: {error:?}"));
            assert_eq!(restored.baseline, prepared.baseline);
            assert_eq!(restored.inputs, prepared.inputs);
            assert_eq!(restored.evidence, prepared.evidence);
        }
    }

    #[test]
    fn compact_incremental_state_rejects_ambiguous_file_selection() {
        let cancellation = Cancellation::new();
        let prepared = compact_incremental_fixture(2);
        let mut compact = DurableIncrementalState::from_prepared(&prepared, &cancellation)
            .expect("compact incremental state is derivable");
        let file = compact.baseline_files[0].file;
        compact.analysis_files = DurableAnalysisFileSelection::Explicit(vec![file, file]);
        let encoded = serde_json::to_vec(&compact).expect("malformed fixture serializes");

        assert!(matches!(
            decode_incremental_state(&encoded, &cancellation),
            Err(FirstSliceError::CatalogCorrupt)
        ));
        assert!(matches!(
            decode_incremental_state(br#"{"version":65535}"#, &cancellation),
            Err(FirstSliceError::CatalogCorrupt)
        ));
    }

    fn open_test_catalog(
        state_root: &Path,
        maximum_generations_per_repository: usize,
    ) -> Result<DurableCatalog, FirstSliceError> {
        DurableCatalog::open(
            state_root,
            maximum_generations_per_repository,
            usize::try_from(rootlight_config::MAXIMUM_REPOSITORIES)
                .expect("configured repository ceiling is representable"),
        )
    }

    fn assert_inventory_equal_ignoring_available(
        mut left: DurableStorageInventory,
        mut right: DurableStorageInventory,
    ) {
        left.available_bytes = 0;
        right.available_bytes = 0;
        assert_eq!(left, right);
    }

    fn sealed_test_generation(
        durable: &DurableCatalog,
        repository: RepositoryId,
        generation: GenerationId,
        parent: Option<GenerationId>,
        materialized_bytes: u64,
    ) -> DurableSealedGeneration {
        let prepared = durable
            .begin_generation(repository, generation)
            .expect("staging generation opens");
        let payload_bytes =
            usize::try_from(materialized_bytes).expect("test payload size is representable");
        fs::write(
            prepared.path().join("staged.bin"),
            vec![0_u8; payload_bytes],
        )
        .expect("staged payload writes");
        prepared
            .account_external_staging_bytes(materialized_bytes)
            .expect("staged payload is accounted");
        DurableSealedGeneration {
            prepared,
            repository,
            materialized_bytes,
            manifest_written_bytes: 0,
            scanned_generation: ScannedGeneration {
                repository,
                generation,
                parent,
                tree_bytes: materialized_bytes,
                source_blobs: BTreeMap::new(),
            },
        }
    }

    fn test_source_file_catalog(
        repository: RepositoryId,
        generation: GenerationId,
    ) -> SourceFileCatalog {
        let path = "src/file.txt";
        let relative = RelativePath::parse(Path::new(path)).expect("fixture path is canonical");
        let path_identity = relative.identity_bytes().to_vec();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: &path_identity,
        })
        .id();
        let bytes = b"durable source file\n";
        let byte_length = u64::try_from(bytes.len()).expect("fixture length is representable");
        let content_hash = content_hash(bytes);
        let claim = FileIdentityClaim {
            file,
            repository,
            path: path.to_owned(),
            path_identity,
            content_hash,
            byte_length,
        };
        let record = FileRecord {
            id: file,
            repository,
            generation,
            path: path.to_owned(),
            path_locator: Some(relative.to_locator()),
            content_hash,
            byte_length,
            language: "text".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([9; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(file, 0, byte_length).expect("fixture span is valid"),
                    content_hash,
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        SourceFileCatalog::new(vec![
            SourceFileCatalogEntry::new(record, claim).expect("fixture entry is valid"),
        ])
        .expect("fixture catalog is valid")
    }

    #[test]
    fn source_file_catalog_sidecar_round_trips_and_rejects_corruption() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = derive_repository(b"source-file-catalog-sidecar").id();
        let generation = GenerationId::from_bytes([7; 20]);
        let catalog = test_source_file_catalog(repository, generation);
        let cancellation = Cancellation::new();
        let prepared = durable
            .begin_generation(repository, generation)
            .expect("staging generation opens");

        let written = prepared
            .write_source_file_catalog(&catalog, &cancellation)
            .expect("source-file catalog writes");
        let descriptor = prepared
            .source_file_catalog
            .lock()
            .expect("source-file descriptor remains available")
            .expect("source-file descriptor is present");
        assert_eq!(written, descriptor.bytes);
        let current_bytes = fs::read(prepared.path().join(SOURCE_FILE_CATALOG_FILENAME))
            .expect("source-file catalog remains readable");
        let current_json: serde_json::Value =
            serde_json::from_slice(&current_bytes).expect("source-file catalog is JSON");
        assert_eq!(
            current_json
                .get("version")
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(SOURCE_FILE_CATALOG_VERSION))
        );
        let current_entry = &current_json["entries"][0];
        assert!(current_entry.get("path_identity").is_some());
        assert!(current_entry.get("claim").is_none());
        assert!(current_entry.get("locator_components").is_none());
        let encoded_identity = current_entry["path_identity"]
            .as_str()
            .expect("path identity is encoded as text");
        let decoded_identity =
            decode_path_identity(encoded_identity).expect("path identity decodes");
        assert_eq!(decoded_identity, catalog.entries()[0].path_identity());
        assert_eq!(
            RelativePath::from_identity_bytes(&decoded_identity)
                .expect("path identity reconstructs")
                .as_str(),
            catalog.entries()[0].file().path
        );
        let decoded: DurableSourceFileCatalog =
            serde_json::from_slice(&current_bytes).expect("current catalog decodes");
        assert_eq!(
            decoded.into_catalog(repository, generation),
            Ok(catalog.clone())
        );
        assert_eq!(
            restore_source_file_catalog(
                prepared.staging(),
                Some(descriptor),
                repository,
                generation,
                &cancellation,
            ),
            Ok(catalog.clone())
        );

        let entry = &catalog.entries()[0];
        let file = entry.file();
        let locator = file
            .path_locator
            .as_ref()
            .expect("fixture locator is present");
        let legacy = LegacyDurableSourceFileCatalog {
            version: LEGACY_SOURCE_FILE_CATALOG_VERSION,
            entries: vec![LegacyDurableSourceFileEntry {
                claim: entry.identity_claim(),
                locator_encoding: locator.encoding().as_str().to_owned(),
                locator_components: locator.components().to_vec(),
                language: file.language.clone(),
                encoding: file.encoding.clone(),
                generated: file.generated,
                provenance: file.provenance,
            }],
        };
        let legacy_bytes = serde_json::to_vec(&legacy).expect("legacy catalog serializes");
        fs::write(
            prepared.path().join(SOURCE_FILE_CATALOG_FILENAME),
            &legacy_bytes,
        )
        .expect("legacy catalog writes");
        let legacy_descriptor = DurableSidecarDescriptor {
            bytes: u64::try_from(legacy_bytes.len()).expect("legacy catalog length is bounded"),
            digest: content_hash_bytes(&legacy_bytes),
        };
        assert_eq!(
            restore_source_file_catalog(
                prepared.staging(),
                Some(legacy_descriptor),
                repository,
                generation,
                &cancellation,
            ),
            Ok(catalog)
        );

        fs::write(prepared.path().join(SOURCE_FILE_CATALOG_FILENAME), b"{}")
            .expect("catalog corruption writes");
        assert_eq!(
            restore_source_file_catalog(
                prepared.staging(),
                Some(descriptor),
                repository,
                generation,
                &cancellation,
            ),
            Err(FirstSliceError::CatalogCorrupt)
        );
    }

    #[test]
    fn incremental_evidence_round_trip_accepts_normalized_rebind_outcomes() {
        let evidence = FirstSliceIncrementalEvidence {
            strategy: crate::FirstSliceBuildStrategy::DependencyDirected,
            input_changes: Vec::new(),
            file_changes: Vec::new(),
            hashed_files: 1,
            invalidated_domains: Vec::new(),
            invalidated_units: 1,
            fallback_reason: None,
            trace_entries: 0,
            invalidation_trace: Vec::new(),
            parsed_files: 1,
            reused_parser_artifacts: 1,
            reused_parser_artifact_bytes: 128,
            reused_durable_artifact_bytes: 0,
            lowered_files: 1,
            reused_normalized_facts: 12,
            rebuilt_normalized_facts: 9,
            planned_fact_work: Vec::new(),
            normalized_fact_work: Vec::new(),
            structural_cache_retained: true,
        };
        let state = LegacyDurableIncrementalState {
            version: LEGACY_INCREMENTAL_STATE_VERSION,
            baseline_files: Vec::new(),
            baseline_inputs: Vec::new(),
            analysis_inputs: Vec::new(),
            evidence,
        };
        let encoded = serde_json::to_vec(&state).expect("durable incremental state serializes");
        let restored: LegacyDurableIncrementalState =
            serde_json::from_slice(&encoded).expect("durable incremental state deserializes");

        validate_incremental_evidence(&restored.evidence)
            .expect("verified normalized rebind evidence remains valid");
        assert_eq!(restored.evidence.lowered_files, 1);
        assert_eq!(restored.evidence.reused_normalized_facts, 12);

        let mut missing_reuse_proof = restored.evidence.clone();
        missing_reuse_proof.reused_normalized_facts = 0;
        assert_eq!(
            validate_incremental_evidence(&missing_reuse_proof),
            Err(FirstSliceError::CatalogCorrupt)
        );

        let mut semantic_replaced = restored.evidence.clone();
        semantic_replaced.reused_normalized_facts = 0;
        semantic_replaced.planned_fact_work = vec![crate::FirstSlicePlannedFactWork {
            disposition: crate::FirstSliceFactWorkDisposition::Reuse,
            domain: rootlight_incremental::FactDomain::Body,
            provider_pass: crate::LOWERING_PASS_ID.to_owned(),
            cause: crate::FirstSliceFactWorkCause::CompleteDependencyMatch,
            files: 1,
            analysis_units: 1,
            file_ids: BTreeSet::new(),
            analysis_unit_ids: BTreeSet::new(),
        }];
        let semantic_state = LegacyDurableIncrementalState {
            version: LEGACY_INCREMENTAL_STATE_VERSION,
            baseline_files: Vec::new(),
            baseline_inputs: Vec::new(),
            analysis_inputs: Vec::new(),
            evidence: semantic_replaced,
        };
        let encoded =
            serde_json::to_vec(&semantic_state).expect("semantic replacement state serializes");
        let semantic_restored: LegacyDurableIncrementalState =
            serde_json::from_slice(&encoded).expect("semantic replacement state deserializes");
        validate_incremental_evidence(&semantic_restored.evidence)
            .expect("planned structural reuse survives complete semantic replacement");
        assert_eq!(semantic_restored.evidence.reused_normalized_facts, 0);

        let mut unsupported_encoding = restored.evidence;
        unsupported_encoding.parsed_files = 0;
        unsupported_encoding.reused_parser_artifacts = 0;
        unsupported_encoding.reused_parser_artifact_bytes = 0;
        unsupported_encoding.lowered_files = 1;
        unsupported_encoding.reused_normalized_facts = 0;
        validate_incremental_evidence(&unsupported_encoding)
            .expect("bounded unsupported encoding lowering remains valid");
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
        let cancellation = Cancellation::new();
        let mut writer = buffered_recovery_writer(CountingWriter::default(), 10_000, &cancellation);
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
    fn recovery_snapshot_writer_checks_cancellation_between_large_chunks() {
        let cancellation = Cancellation::new();
        let mut writer = RecoverySnapshotWriter::new(
            CancellingWriter {
                bytes: 0,
                cancellation: cancellation.clone(),
            },
            u64::MAX,
            &cancellation,
        );
        let payload = vec![0_u8; RECOVERY_SERIALIZATION_CHECKPOINT_BYTES * 2];

        assert!(writer.write_all(&payload).is_err());
        assert_eq!(
            writer.failure,
            Some(RecoveryWriteFailure::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
        assert_eq!(
            writer.bytes,
            u64::try_from(RECOVERY_SERIALIZATION_CHECKPOINT_BYTES)
                .expect("checkpoint size is representable")
        );
        assert_eq!(writer.inner.bytes, RECOVERY_SERIALIZATION_CHECKPOINT_BYTES);
    }

    #[test]
    fn recovery_snapshot_writer_rejects_limit_before_inner_write() {
        let cancellation = Cancellation::new();
        let mut writer = RecoverySnapshotWriter::new(CountingWriter::default(), 8, &cancellation);

        assert!(writer.write_all(b"123456789").is_err());
        assert_eq!(writer.failure, Some(RecoveryWriteFailure::Limit));
        assert_eq!(writer.bytes, 0);
        assert_eq!(writer.inner.bytes, 0);
        assert_eq!(writer.inner.writes, 0);
    }

    #[test]
    fn recovery_json_count_is_exact_bounded_and_cancellable() {
        let value = vec!["bounded recovery payload"; 128];
        let expected = u64::try_from(
            serde_json::to_vec(&value)
                .expect("test recovery payload serializes")
                .len(),
        )
        .expect("test recovery payload size is representable");
        let cancellation = Cancellation::new();

        assert_eq!(
            recovery_json_serialized_bytes(&value, expected, &cancellation),
            Ok(expected)
        );
        assert_eq!(
            recovery_json_serialized_bytes(&value, expected - 1, &cancellation),
            Err(FirstSliceError::CatalogCorrupt)
        );

        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert_eq!(
            recovery_json_serialized_bytes(&value, expected, &cancelled),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
    }

    #[test]
    fn recovery_snapshot_plan_validates_all_codecs_without_loading_payloads() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = derive_repository(b"recovery-descriptor").id();
        let parent = Some(GenerationId::from_bytes([6; 20]));
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent,
            manifest_hash: content_hash(b"manifest"),
            config_hash: content_hash(b"configuration"),
            provider_set_hash: content_hash(b"providers"),
            format_version: u32::from(GENERATION_CONTRACT_VERSION.major()) << 16
                | u32::from(GENERATION_CONTRACT_VERSION.minor()),
        })
        .id();
        let prepared = durable
            .begin_generation(repository, generation)
            .expect("staging generation opens");
        let directory = prepared.staging();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        assert!(
            read_recovery_snapshot_plan(directory, repository, generation, parent, &cancellation)
                .expect("absent descriptor is compatible with oracle recovery")
                .is_none()
        );
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::Shutdown);
        assert!(matches!(
            read_recovery_snapshot_plan(directory, repository, generation, parent, &cancelled),
            Err(FirstSliceError::Cancelled(CancellationReason::Shutdown))
        ));

        let baseline = serde_json::json!({
            "version": RECOVERY_SNAPSHOT_VERSION,
            "bytes": 128,
            "digest": content_hash(b"encoded"),
            "encoding": "message_pack_gzip",
            "decoded_bytes": 512,
            "decoded_digest": content_hash(b"decoded"),
            "serialized_document_bytes": 640,
            "contract_major": GENERATION_CONTRACT_VERSION.major(),
            "contract_minor": GENERATION_CONTRACT_VERSION.minor(),
            "manifest_hash": content_hash(b"manifest"),
            "configuration_hash": content_hash(b"configuration"),
            "provider_set_hash": content_hash(b"providers"),
        });
        let descriptor_path = prepared.path().join(RECOVERY_MANIFEST_FILENAME);
        drop(
            directory
                .create_file(OsStr::new(RECOVERY_MANIFEST_FILENAME))
                .expect("private descriptor file creates"),
        );
        for (version, snapshot_name, decoded_bytes, serialized_bytes) in [
            (
                LEGACY_RECOVERY_SNAPSHOT_VERSION,
                RECOVERY_SNAPSHOT_FILENAME,
                128,
                128,
            ),
            (
                JSON_GZIP_RECOVERY_SNAPSHOT_VERSION,
                RECOVERY_SNAPSHOT_GZIP_FILENAME,
                512,
                512,
            ),
            (
                RECOVERY_SNAPSHOT_VERSION,
                RECOVERY_SNAPSHOT_MESSAGEPACK_GZIP_FILENAME,
                512,
                640,
            ),
        ] {
            let mut descriptor = baseline.clone();
            descriptor["version"] = serde_json::json!(version);
            if version == LEGACY_RECOVERY_SNAPSHOT_VERSION {
                for field in [
                    "encoding",
                    "decoded_bytes",
                    "decoded_digest",
                    "serialized_document_bytes",
                ] {
                    descriptor
                        .as_object_mut()
                        .expect("descriptor is an object")
                        .remove(field);
                }
            } else if version == JSON_GZIP_RECOVERY_SNAPSHOT_VERSION {
                descriptor["encoding"] = serde_json::json!("gzip");
                descriptor
                    .as_object_mut()
                    .expect("descriptor is an object")
                    .remove("serialized_document_bytes");
            }
            fs::write(
                &descriptor_path,
                serde_json::to_vec(&descriptor).expect("descriptor serializes"),
            )
            .expect("descriptor writes");
            serde_json::from_value::<DurableRecoverySnapshot>(descriptor)
                .expect("valid fixture uses the persisted descriptor schema");
            let plan = read_recovery_snapshot_plan(
                directory,
                repository,
                generation,
                parent,
                &cancellation,
            )
            .expect("bounded descriptor validates without its payload")
            .expect("descriptor is present");
            assert_eq!(plan.snapshot_name, snapshot_name);
            assert_eq!(plan.encoded_bytes, 128);
            assert_eq!(plan.decoded_bytes, decoded_bytes);
            assert_eq!(plan.serialized_document_bytes, serialized_bytes);
            assert_eq!(plan.metadata.repository(), repository);
            assert_eq!(plan.metadata.generation(), generation);
            assert_eq!(plan.metadata.parent(), parent);
            assert!(!prepared.path().join(snapshot_name).exists());
            assert!(matches!(
                restore_recovery_generation(
                    directory,
                    &plan,
                    &SourceFileCatalog::default(),
                    &context,
                    &cancellation
                ),
                Err(FirstSliceError::CatalogCorrupt)
            ));
        }

        for (field, invalid) in [
            ("version", serde_json::json!(0)),
            ("version", serde_json::json!(RECOVERY_SNAPSHOT_VERSION + 1)),
            ("encoding", serde_json::json!("gzip")),
            ("encoding", serde_json::Value::Null),
            ("bytes", serde_json::json!(0)),
            ("bytes", serde_json::json!(MAX_RECOVERY_ENCODED_BYTES + 1)),
            ("decoded_bytes", serde_json::json!(0)),
            (
                "decoded_bytes",
                serde_json::json!(MAX_RECOVERY_SNAPSHOT_BYTES + 1),
            ),
            ("decoded_bytes", serde_json::Value::Null),
            ("decoded_digest", serde_json::Value::Null),
            ("serialized_document_bytes", serde_json::json!(511)),
            (
                "serialized_document_bytes",
                serde_json::json!(MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + 1),
            ),
            ("serialized_document_bytes", serde_json::Value::Null),
            (
                "contract_major",
                serde_json::json!(GENERATION_CONTRACT_VERSION.major() + 1),
            ),
            (
                "contract_minor",
                serde_json::json!(GENERATION_CONTRACT_VERSION.minor() + 1),
            ),
        ] {
            let mut descriptor = baseline.clone();
            descriptor[field] = invalid;
            fs::write(
                &descriptor_path,
                serde_json::to_vec(&descriptor).expect("descriptor serializes"),
            )
            .expect("descriptor writes");
            assert!(
                matches!(
                    read_recovery_snapshot_plan(
                        directory,
                        repository,
                        generation,
                        parent,
                        &cancellation
                    ),
                    Err(FirstSliceError::CatalogCorrupt)
                ),
                "invalid {field} must fail before payload materialization"
            );
        }
    }

    #[test]
    fn restored_document_size_reconciles_verified_sources_fail_closed() {
        assert_eq!(
            reconcile_restored_serialized_document_bytes(Some(512), Some(512)),
            Ok(Some(512))
        );
        assert_eq!(
            reconcile_restored_serialized_document_bytes(Some(512), None),
            Ok(None)
        );
        assert_eq!(
            reconcile_restored_serialized_document_bytes(None, Some(512)),
            Ok(None)
        );
        assert_eq!(
            reconcile_restored_serialized_document_bytes(Some(512), Some(513)),
            Err(FirstSliceError::CatalogCorrupt)
        );
    }

    #[test]
    fn messagepack_recovery_accounting_rejects_charge_below_decoded_payload() {
        assert_eq!(validate_recovery_document_accounting(512, 640), Ok(640));
        assert_eq!(validate_recovery_document_accounting(512, 512), Ok(512));
        assert_eq!(
            validate_recovery_document_accounting(512, 511),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            validate_recovery_document_accounting(0, 512),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            validate_recovery_document_accounting(
                MAX_RECOVERY_SNAPSHOT_BYTES + 1,
                MAX_RECOVERY_SNAPSHOT_BYTES + 1,
            ),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            validate_recovery_document_accounting(
                MAX_RECOVERY_SNAPSHOT_BYTES,
                MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + 1,
            ),
            Err(FirstSliceError::CatalogCorrupt)
        );
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
    fn logical_snapshot_sidecar_is_manifest_compatible_and_fail_closed() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn logical_sidecar_fixture() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let expected = receipt
            .logical_snapshot
            .clone()
            .expect("published generation has a logical identity");
        let mut legacy_receipt = receipt.clone();
        legacy_receipt.logical_snapshot = None;
        assert_eq!(
            serde_json::to_vec(&receipt).expect("current receipt serializes"),
            serde_json::to_vec(&legacy_receipt).expect("legacy receipt serializes"),
            "the in-memory identity must not change frozen manifest receipt bytes"
        );
        let generation_path = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(generation_path.join(MANIFEST_FILENAME)).expect("manifest reads"),
        )
        .expect("manifest is valid JSON");
        assert!(
            manifest["receipt"].get("logical_snapshot").is_none(),
            "manifest-v2 receipt bytes must remain readable by the previous binary"
        );

        let durable = service.durable.as_ref().expect("durable catalog exists");
        let repository = PrivateDirectory::open(
            durable.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .expect("repository directory opens");
        let generation = PrivateDirectory::open(
            repository.capability(),
            OsStr::new(&receipt.generation.to_string()),
        )
        .expect("generation directory opens");
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .expect("published generation resolves");
        let expected_serialized_document_bytes = u64::try_from(
            serde_json::to_vec(snapshot.document())
                .expect("published document serializes")
                .len(),
        )
        .expect("published document size fits u64");
        let sidecar_path = generation_path.join(LOGICAL_SNAPSHOT_FILENAME);
        let original = fs::read(&sidecar_path).expect("logical sidecar reads");
        let current: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&original).expect("current sidecar decodes");
        let current_descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&current.payload).expect("current descriptor decodes");
        assert_eq!(current_descriptor.version, LOGICAL_SNAPSHOT_VERSION);
        assert_eq!(
            current_descriptor.serialized_document_bytes,
            Some(expected_serialized_document_bytes)
        );
        let restored = restore_logical_snapshot_identity(&generation, &snapshot)
            .expect("logical sidecar restores")
            .expect("current logical sidecar is recognized");
        assert_eq!(restored.identity, expected);
        assert_eq!(
            restored.serialized_document_bytes,
            Some(expected_serialized_document_bytes)
        );

        let mut legacy = current;
        let mut legacy_descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&legacy.payload).expect("legacy descriptor decodes");
        legacy_descriptor.version = LEGACY_LOGICAL_SNAPSHOT_VERSION;
        legacy_descriptor.serialized_document_bytes = None;
        legacy.payload =
            serde_json::to_string(&legacy_descriptor).expect("legacy descriptor serializes");
        legacy.digest = content_hash_bytes(legacy.payload.as_bytes());
        let legacy_directory = generation
            .create_directory(OsStr::new("legacy-logical"))
            .expect("legacy-sidecar fixture directory creates");
        let mut legacy_file = legacy_directory
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("legacy-sidecar fixture creates");
        legacy_file
            .write_all(&serde_json::to_vec(&legacy).expect("legacy sidecar serializes"))
            .expect("legacy sidecar writes");
        legacy_file.sync_all().expect("legacy sidecar syncs");
        drop(legacy_file);
        let restored = restore_logical_snapshot_identity(&legacy_directory, &snapshot)
            .expect("legacy logical sidecar restores")
            .expect("legacy logical sidecar is recognized");
        assert_eq!(restored.identity, expected);
        assert_eq!(restored.serialized_document_bytes, None);

        let missing = generation
            .create_directory(OsStr::new("missing-logical"))
            .expect("missing-sidecar fixture directory creates");
        assert_eq!(
            restore_logical_snapshot_identity(&missing, &snapshot)
                .expect("missing sidecar is an observability gap"),
            None
        );

        let mut corrupt: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&original).expect("sidecar decodes");
        corrupt.payload.push(' ');
        let corrupt_directory = generation
            .create_directory(OsStr::new("corrupt-logical"))
            .expect("corrupt-sidecar fixture directory creates");
        let mut corrupt_file = corrupt_directory
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("corrupt-sidecar fixture creates");
        corrupt_file
            .write_all(&serde_json::to_vec(&corrupt).expect("corrupt sidecar serializes"))
            .expect("corrupt sidecar writes");
        corrupt_file.sync_all().expect("corrupt sidecar syncs");
        drop(corrupt_file);
        assert_eq!(
            restore_logical_snapshot_identity(&corrupt_directory, &snapshot),
            Err(FirstSliceError::CatalogCorrupt)
        );

        let mut mismatch: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&original).expect("sidecar decodes");
        let mut descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&mismatch.payload).expect("descriptor decodes");
        descriptor.generation = GenerationId::from_bytes([0x5a; 20]);
        mismatch.payload = serde_json::to_string(&descriptor).expect("descriptor serializes");
        mismatch.digest = content_hash_bytes(mismatch.payload.as_bytes());
        let mismatch_directory = generation
            .create_directory(OsStr::new("mismatch-logical"))
            .expect("mismatch-sidecar fixture directory creates");
        let mut mismatch_file = mismatch_directory
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("mismatch-sidecar fixture creates");
        mismatch_file
            .write_all(&serde_json::to_vec(&mismatch).expect("mismatched sidecar serializes"))
            .expect("mismatched sidecar writes");
        mismatch_file.sync_all().expect("mismatched sidecar syncs");
        drop(mismatch_file);
        assert_eq!(
            restore_logical_snapshot_identity(&mismatch_directory, &snapshot),
            Err(FirstSliceError::CatalogCorrupt)
        );

        let mut invalid: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&original).expect("sidecar decodes");
        let mut descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&invalid.payload).expect("descriptor decodes");
        descriptor.serialized_document_bytes = None;
        invalid.payload = serde_json::to_string(&descriptor).expect("descriptor serializes");
        invalid.digest = content_hash_bytes(invalid.payload.as_bytes());
        let invalid_directory = generation
            .create_directory(OsStr::new("invalid-logical"))
            .expect("invalid-sidecar fixture directory creates");
        let mut invalid_file = invalid_directory
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("invalid-sidecar fixture creates");
        invalid_file
            .write_all(&serde_json::to_vec(&invalid).expect("invalid sidecar serializes"))
            .expect("invalid sidecar writes");
        invalid_file.sync_all().expect("invalid sidecar syncs");
        drop(invalid_file);
        assert_eq!(
            restore_logical_snapshot_identity(&invalid_directory, &snapshot),
            Err(FirstSliceError::CatalogCorrupt)
        );

        let mut future: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&original).expect("sidecar decodes");
        let mut descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&future.payload).expect("descriptor decodes");
        descriptor.version = LOGICAL_SNAPSHOT_VERSION + 1;
        descriptor.schema_version = "2.0".to_owned();
        future.payload = serde_json::to_string(&descriptor).expect("descriptor serializes");
        future.digest = content_hash_bytes(future.payload.as_bytes());
        let future_directory = generation
            .create_directory(OsStr::new("future-logical"))
            .expect("future-sidecar fixture directory creates");
        let mut future_file = future_directory
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("future-sidecar fixture creates");
        future_file
            .write_all(&serde_json::to_vec(&future).expect("future sidecar serializes"))
            .expect("future sidecar writes");
        future_file.sync_all().expect("future sidecar syncs");
        drop(future_file);
        assert_eq!(
            restore_logical_snapshot_identity(&future_directory, &snapshot)
                .expect("future observability descriptor does not strand generation"),
            None
        );
        drop(future_directory);
        drop(invalid_directory);
        drop(mismatch_directory);
        drop(corrupt_directory);
        drop(missing);
        drop(legacy_directory);
        generation
            .capability()
            .remove_file(Path::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("current sidecar removes through its retained directory");
        let mut future_file = generation
            .create_file(OsStr::new(LOGICAL_SNAPSHOT_FILENAME))
            .expect("future sidecar replaces the current descriptor");
        future_file
            .write_all(&serde_json::to_vec(&future).expect("future sidecar serializes"))
            .expect("future sidecar writes");
        future_file.sync_all().expect("future sidecar syncs");
        drop(future_file);
        drop(generation);
        drop(repository);
        drop(service);

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("generation with a future logical descriptor restores");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert!(
            restored
                .repository_status(receipt.repository, None)
                .expect("future descriptor status resolves")
                .logical_snapshot
                .is_none()
        );
        drop(restored);

        fs::remove_file(&sidecar_path).expect("logical sidecar deletes");
        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("generation without a logical sidecar restores");
        assert_eq!(
            restored.active_generation_for(receipt.repository),
            Some(receipt.generation)
        );
        assert!(
            restored
                .repository_status(receipt.repository, None)
                .expect("status without a persisted identity resolves")
                .logical_snapshot
                .is_none()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn logical_snapshot_sidecar_requires_its_incremental_state_on_restore() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        let source = fixture.path().join("lib.rs");
        fs::write(
            &source,
            "pub fn logical_incremental_fixture() -> u32 { 1 }\n",
        )
        .expect("initial fixture source writes");
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
                .expect("predecessor generation publishes");
            fs::write(
                &source,
                "pub fn logical_incremental_fixture() -> u32 { 2 }\n",
            )
            .expect("successor fixture source writes");
            let second = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("active generation publishes");
            (first, second)
        };
        let incremental_path = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(second.repository.to_string())
            .join(second.generation.to_string())
            .join(INCREMENTAL_STATE_FILENAME);
        fs::remove_file(incremental_path).expect("active incremental sidecar deletes");

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("last-good predecessor restores");
        assert_eq!(
            restored.active_generation_for(first.repository),
            Some(first.generation)
        );
        assert!(
            restored
                .repository_status(first.repository, None)
                .expect("predecessor status resolves")
                .logical_snapshot
                .is_some()
        );
        assert!(matches!(
            restored.repository_status(second.repository, Some(second.generation)),
            Err(FirstSliceError::GenerationNotFound)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn logical_snapshot_observability_hash_does_not_block_verified_query_state() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        let source = fixture.path().join("lib.rs");
        fs::write(&source, "pub fn logical_recompute_fixture() -> u32 { 1 }\n")
            .expect("initial fixture source writes");
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
                .expect("predecessor generation publishes");
            fs::write(&source, "pub fn logical_recompute_fixture() -> u32 { 2 }\n")
                .expect("successor fixture source writes");
            let second = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("active generation publishes");
            (first, second)
        };
        let sidecar_path = paths
            .state_dir()
            .join("first-slice/repositories")
            .join(second.repository.to_string())
            .join(second.generation.to_string())
            .join(LOGICAL_SNAPSHOT_FILENAME);
        let mut sidecar: DurableLogicalSnapshotSidecar =
            serde_json::from_slice(&fs::read(&sidecar_path).expect("active logical sidecar reads"))
                .expect("active sidecar decodes");
        let mut descriptor: DurableLogicalSnapshotIdentity =
            serde_json::from_str(&sidecar.payload).expect("active descriptor decodes");
        descriptor.hash = ContentHash::from_bytes([0xa5; 32]);
        sidecar.payload =
            serde_json::to_string(&descriptor).expect("wrong-hash descriptor serializes");
        sidecar.digest = content_hash_bytes(sidecar.payload.as_bytes());
        fs::write(
            &sidecar_path,
            serde_json::to_vec(&sidecar).expect("self-consistent wrong-hash sidecar serializes"),
        )
        .expect("wrong-hash sidecar overwrites");

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("checksummed active generation restores");
        assert_eq!(
            restored.active_generation_for(second.repository),
            Some(second.generation)
        );
        assert_eq!(
            restored
                .repository_status(second.repository, None)
                .expect("active status resolves")
                .logical_snapshot,
            Some(FirstSliceLogicalSnapshotIdentity::new(
                ContentHash::from_bytes([0xa5; 32])
            ))
        );
        assert_eq!(
            restored
                .code_locate(
                    second.generation,
                    "logical_recompute_fixture".to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &cancellation,
                )
                .expect("verified query state remains available")
                .data
                .hits
                .len(),
            1
        );
        assert_ne!(first.generation, second.generation);
    }

    #[test]
    fn storage_admission_reports_budget_and_minimum_free_scopes() {
        let inventory = DurableStorageInventory {
            repositories: vec![DurableRepositoryStorage {
                repository: RepositoryId::from_bytes([1; 16]),
                physical_bytes: 100,
                active_generation_bytes: 50,
                predecessor_generation_bytes: 30,
                other_retained_generation_bytes: 0,
                source_pool_bytes: 20,
                shared_source_bytes: 20,
                temporary_bytes: 0,
                reclaimable_bytes: 0,
                repository_overhead_bytes: 0,
                inflight_reservation_bytes: 0,
            }],
            generations: Vec::new(),
            generation_unique_bytes: 80,
            active_generation_bytes: 50,
            predecessor_generation_bytes: 30,
            other_retained_generation_bytes: 0,
            source_pool_bytes: 20,
            shared_source_bytes: 20,
            temporary_bytes: 0,
            reclaimable_bytes: 0,
            pinned_bytes: 0,
            repository_overhead_bytes: 0,
            quarantine_bytes: 0,
            total_physical_bytes: 100,
            available_bytes: 50,
            inflight_catalog_reservation_bytes: 0,
            inflight_repository_reservation_bytes: 0,
        };

        let repository = RepositoryId::from_bytes([1; 16]);
        let repository_failure = check_storage_admission(
            &inventory,
            repository,
            DurableStorageAdmissionPolicy {
                required_catalog_bytes: 1,
                required_repository_bytes: 31,
                maximum_repository_bytes: 130,
                maximum_storage_bytes: u64::MAX,
                minimum_free_bytes: 0,
                repository_amplification: None,
            },
        )
        .expect_err("retained repository bytes exceed the configured budget");
        assert_eq!(
            repository_failure,
            DurableStorageAdmissionFailure {
                scope: DurableStorageAdmissionScope::RepositoryBudget,
                required_bytes: 31,
                observed_bytes: 100,
                projected_bytes: 131,
                limit_bytes: 130,
                minimum_free_bytes: 0,
                repository_amplification: None,
            }
        );

        let budget_failure = check_storage_admission(
            &inventory,
            repository,
            DurableStorageAdmissionPolicy {
                required_catalog_bytes: 30,
                required_repository_bytes: 0,
                maximum_repository_bytes: u64::MAX,
                maximum_storage_bytes: 120,
                minimum_free_bytes: 10,
                repository_amplification: None,
            },
        )
        .expect_err("projected durable bytes exceed the configured budget");
        assert_eq!(
            budget_failure,
            DurableStorageAdmissionFailure {
                scope: DurableStorageAdmissionScope::CatalogBudget,
                required_bytes: 30,
                observed_bytes: 100,
                projected_bytes: 130,
                limit_bytes: 120,
                minimum_free_bytes: 10,
                repository_amplification: None,
            }
        );

        let free_space_failure = check_storage_admission(
            &inventory,
            repository,
            DurableStorageAdmissionPolicy {
                required_catalog_bytes: 31,
                required_repository_bytes: 0,
                maximum_repository_bytes: u64::MAX,
                maximum_storage_bytes: u64::MAX,
                minimum_free_bytes: 20,
                repository_amplification: None,
            },
        )
        .expect_err("minimum free space is removed before reserving publication bytes");
        assert_eq!(
            free_space_failure,
            DurableStorageAdmissionFailure {
                scope: DurableStorageAdmissionScope::FilesystemFreeSpace,
                required_bytes: 31,
                observed_bytes: 50,
                projected_bytes: 31,
                limit_bytes: 30,
                minimum_free_bytes: 20,
                repository_amplification: None,
            }
        );

        assert_eq!(
            check_storage_admission(
                &inventory,
                repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 10,
                    required_repository_bytes: 10,
                    maximum_repository_bytes: 130,
                    maximum_storage_bytes: 130,
                    minimum_free_bytes: 20,
                    repository_amplification: None,
                },
            ),
            Ok(DurableStorageAdmission {
                required_bytes: 10,
                observed_bytes: 100,
                limit_bytes: 130,
                minimum_free_bytes: 20,
                admission_margin_bytes: 20,
            })
        );
    }

    #[test]
    fn sealed_generation_uses_exact_repository_bytes_and_preserves_catalog_headroom() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = RepositoryId::from_bytes([37; 16]);
        let generation = GenerationId::from_bytes([41; 20]);
        let admitted_worst_case_bytes = 10 * 1024;
        let reservation = durable
            .ensure_staging_capacity(
                repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: admitted_worst_case_bytes,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: 6 * 1024,
                    maximum_storage_bytes: 20 * 1024,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("inventory remains readable")
            .expect("transient headroom is admitted")
            .1;
        let prepared = durable
            .begin_generation(repository, generation)
            .expect("staging generation opens");
        let payload = vec![0_u8; 1024];
        std::fs::write(prepared.path().join("staged.bin"), &payload)
            .expect("staged payload writes");
        prepared
            .account_external_staging_bytes(1024)
            .expect("staged payload is accounted");
        let materialized_bytes = 1024;
        let sealed = DurableSealedGeneration {
            prepared,
            repository,
            materialized_bytes,
            manifest_written_bytes: 0,
            scanned_generation: ScannedGeneration {
                repository,
                generation,
                parent: None,
                tree_bytes: materialized_bytes,
                source_blobs: BTreeMap::new(),
            },
        };

        let (admission, _admitted) = durable
            .finalize_repository_capacity(
                &reservation,
                sealed,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: admitted_worst_case_bytes,
                    required_repository_bytes: DURABLE_PUBLICATION_RESIDUAL_BYTES,
                    maximum_repository_bytes: 6 * 1024,
                    maximum_storage_bytes: 20 * 1024,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("exact inventory remains readable")
            .expect("exact candidate fits the retained repository budget");

        assert_eq!(
            admission.required_bytes,
            1024 + DURABLE_PUBLICATION_RESIDUAL_BYTES
        );
        assert_eq!(admission.observed_bytes, 0);
        let reservations = durable
            .storage_accounting
            .lock()
            .expect("reservation ledger remains available");
        let entry = reservations
            .reservations
            .entries
            .get(&reservation.id)
            .expect("reservation remains held through publication");
        assert_eq!(
            entry.catalog_bytes,
            admitted_worst_case_bytes - materialized_bytes
        );
        assert_eq!(entry.repository_bytes, DURABLE_PUBLICATION_RESIDUAL_BYTES);
    }

    #[test]
    fn amplification_envelope_accounts_for_emitted_facts_and_retention() {
        let policy = DurableRepositoryAmplificationPolicy {
            examined_source_bytes: 2_631_973,
            emitted_fact_bytes: 30_000_000,
            source_factor: 25,
            oracle_factor: 8,
            retained_generations: 2,
            fixed_headroom_bytes: 64 * 1024 * 1024,
        };

        let amplification = policy.limit(8 * 1024 * 1024 * 1024);
        let source_only_limit = policy
            .examined_source_bytes
            .checked_mul(policy.effective_factor())
            .and_then(|bytes| bytes.checked_add(policy.fixed_headroom_bytes))
            .expect("source-only comparison is representable");

        assert_eq!(source_only_limit, 156_595_946);
        assert_eq!(amplification.amplification_limit_bytes, 615_540_162);
        assert!(
            source_only_limit < 311_890_202,
            "the former source-only envelope rejects a valid dense fact set"
        );
        assert!(
            amplification.amplification_limit_bytes >= 311_890_202,
            "the emitted-fact envelope admits the bounded retained set"
        );
    }

    #[test]
    fn sealed_amplification_gate_counts_predecessor_and_shared_source_storage_once() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        let stable = "pub fn stable_amplification_input() -> u32 { 1 }\n";
        fs::write(fixture.path().join("src/stable.rs"), stable).expect("stable source writes");
        let changed_path = fixture.path().join("src/changed.rs");
        fs::write(
            &changed_path,
            "pub fn changed_amplification_input() -> u32 { 1 }\n",
        )
        .expect("initial changed source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("first generation publishes");
        fs::write(
            &changed_path,
            "pub fn changed_amplification_input() -> u32 { 2 }\n",
        )
        .expect("changed source writes");
        let active = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("second generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let inventory = durable
            .storage_inventory()
            .expect("retained inventory scans");
        let repository_storage = inventory
            .repositories
            .iter()
            .find(|storage| storage.repository == active.repository)
            .expect("repository storage is present");
        assert!(repository_storage.predecessor_generation_bytes > 0);
        assert_eq!(
            repository_storage.shared_source_bytes,
            u64::try_from(stable.len()).expect("fixture byte count is representable")
        );
        assert_eq!(
            repository_storage.physical_bytes,
            repository_storage
                .active_generation_bytes
                .checked_add(repository_storage.predecessor_generation_bytes)
                .and_then(|bytes| {
                    bytes.checked_add(repository_storage.other_retained_generation_bytes)
                })
                .and_then(|bytes| bytes.checked_add(repository_storage.source_pool_bytes))
                .and_then(|bytes| bytes.checked_add(repository_storage.temporary_bytes))
                .and_then(|bytes| bytes.checked_add(repository_storage.reclaimable_bytes))
                .and_then(|bytes| bytes.checked_add(repository_storage.repository_overhead_bytes))
                .expect("repository byte total is representable")
        );

        let reserved_catalog_bytes = 16 * 1024;
        let reservation = durable
            .ensure_staging_capacity(
                active.repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: reserved_catalog_bytes,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("inventory remains readable")
            .expect("staging capacity is admitted")
            .1;
        let materialized_bytes = 1024;
        let sealed = sealed_test_generation(
            durable,
            active.repository,
            GenerationId::from_bytes([73; 20]),
            Some(active.generation),
            materialized_bytes,
        );
        let projected_bytes = repository_storage
            .physical_bytes
            .checked_add(materialized_bytes)
            .and_then(|bytes| bytes.checked_add(DURABLE_PUBLICATION_RESIDUAL_BYTES))
            .expect("projected retained bytes are representable");
        let amplification_limit_bytes = projected_bytes - 1;
        let result = durable
            .finalize_repository_capacity(
                &reservation,
                sealed,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: reserved_catalog_bytes,
                    required_repository_bytes: DURABLE_PUBLICATION_RESIDUAL_BYTES,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: Some(DurableRepositoryAmplificationPolicy {
                        examined_source_bytes: 0,
                        emitted_fact_bytes: 0,
                        source_factor: 0,
                        oracle_factor: 0,
                        retained_generations: 1,
                        fixed_headroom_bytes: amplification_limit_bytes,
                    }),
                },
            )
            .expect("exact inventory remains readable");
        let failure = match result {
            Ok(_) => panic!("retained predecessor and source-pool bytes cross the final ceiling"),
            Err(failure) => failure,
        };

        assert_eq!(
            failure,
            DurableStorageAdmissionFailure {
                scope: DurableStorageAdmissionScope::RepositoryAmplification,
                required_bytes: materialized_bytes + DURABLE_PUBLICATION_RESIDUAL_BYTES,
                observed_bytes: repository_storage.physical_bytes,
                projected_bytes,
                limit_bytes: amplification_limit_bytes,
                minimum_free_bytes: 0,
                repository_amplification: Some(DurableRepositoryAmplification {
                    examined_source_bytes: 0,
                    emitted_fact_bytes: 0,
                    effective_factor: 1,
                    retained_generations: 1,
                    absolute_limit_bytes: u64::MAX,
                    amplification_limit_bytes,
                }),
            }
        );
        drop(reservation);
    }

    #[test]
    fn sealed_amplification_failure_restores_concurrent_reservations_and_accounting() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = RepositoryId::from_bytes([79; 16]);
        let baseline = durable
            .storage_inventory()
            .expect("unreserved inventory establishes the physical baseline");
        let current = durable
            .ensure_staging_capacity(
                repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 16 * 1024,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("inventory remains readable")
            .expect("candidate reservation is admitted")
            .1;
        let concurrent = durable
            .ensure_staging_capacity(
                repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 4 * 1024,
                    required_repository_bytes: 2 * 1024,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("inventory remains readable")
            .expect("concurrent reservation is admitted")
            .1;
        let sealed = sealed_test_generation(
            &durable,
            repository,
            GenerationId::from_bytes([83; 20]),
            None,
            1024,
        );
        let result = durable
            .finalize_repository_capacity(
                &current,
                sealed,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 16 * 1024,
                    required_repository_bytes: DURABLE_PUBLICATION_RESIDUAL_BYTES,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: Some(DurableRepositoryAmplificationPolicy {
                        examined_source_bytes: 0,
                        emitted_fact_bytes: 0,
                        source_factor: 0,
                        oracle_factor: 0,
                        retained_generations: 1,
                        fixed_headroom_bytes: 1024,
                    }),
                },
            )
            .expect("target inventory remains readable");
        let failure = match result {
            Ok(_) => panic!("tiny retained-state allowance rejects the sealed candidate"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.scope,
            DurableStorageAdmissionScope::RepositoryAmplification
        );
        assert_eq!(failure.observed_bytes, 2 * 1024);
        assert_eq!(
            failure.projected_bytes,
            3 * 1024 + DURABLE_PUBLICATION_RESIDUAL_BYTES
        );
        assert_eq!(failure.limit_bytes, 1024);
        {
            let accounting = durable
                .storage_accounting
                .lock()
                .expect("reservation ledger remains available");
            assert_eq!(accounting.reservations.entries.len(), 2);
            assert_eq!(
                accounting
                    .reservations
                    .entries
                    .get(&current.id)
                    .expect("candidate reservation is restored")
                    .catalog_bytes,
                16 * 1024
            );
            assert_eq!(
                accounting
                    .reservations
                    .entries
                    .get(&concurrent.id)
                    .expect("concurrent reservation remains")
                    .repository_bytes,
                2 * 1024
            );
        }
        durable
            .release_staging_reservation(current)
            .expect("candidate reservation releases");
        durable
            .release_staging_reservation(concurrent)
            .expect("concurrent reservation releases");
        let verified = durable
            .storage_inventory()
            .expect("post-cleanup inventory verifies");
        assert_eq!(verified.total_physical_bytes, baseline.total_physical_bytes);
        assert_eq!(verified.inflight_catalog_reservation_bytes, 0);
        assert_eq!(verified.inflight_repository_reservation_bytes, 0);
        assert!(
            verified
                .repositories
                .iter()
                .all(|repository| repository.physical_bytes == 0)
        );
    }

    #[test]
    fn unreferenced_source_blobs_remain_physical_and_reclaimable() {
        let repository = RepositoryId::from_bytes([43; 16]);
        let digest = ContentHash::from_bytes([47; 32]);
        let blob_bytes = 1536;
        let mut source_blobs = BTreeMap::new();
        source_blobs.insert(
            digest,
            ScannedSourceBlob {
                bytes: blob_bytes,
                payload_bytes: 1024,
            },
        );
        let inventory = build_storage_inventory(
            vec![ScannedRepositoryStorage {
                repository: Some(repository),
                source_blobs,
                ..ScannedRepositoryStorage::default()
            }],
            0,
            u64::MAX,
        )
        .expect("unreferenced physical inventory remains valid");

        assert_eq!(inventory.total_physical_bytes, blob_bytes);
        assert_eq!(inventory.reclaimable_bytes, blob_bytes);
        assert_eq!(inventory.shared_source_bytes, 0);
        assert_eq!(
            inventory
                .repositories
                .iter()
                .find(|candidate| candidate.repository == repository)
                .expect("repository inventory is present")
                .physical_bytes,
            blob_bytes
        );
    }

    #[test]
    fn concurrent_storage_admissions_share_one_serialized_reservation_ledger() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = Arc::new(open_test_catalog(paths.state_dir(), 2).expect("catalog opens"));
        let observed = durable
            .storage_inventory()
            .expect("cold inventory scans")
            .total_physical_bytes;
        let required_bytes = 1024;
        let policy = DurableStorageAdmissionPolicy {
            required_catalog_bytes: required_bytes,
            required_repository_bytes: 0,
            maximum_repository_bytes: u64::MAX,
            maximum_storage_bytes: observed + required_bytes,
            minimum_free_bytes: 0,
            repository_amplification: None,
        };
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let repository = RepositoryId::from_bytes([29; 16]);
        let workers = (0..2)
            .map(|_| {
                let durable = Arc::clone(&durable);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let admission = durable
                        .ensure_staging_capacity(repository, policy)
                        .expect("inventory remains readable");
                    barrier.wait();
                    admission
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = workers
            .into_iter()
            .map(|worker| worker.join().expect("admission worker joins"))
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(DurableStorageAdmissionFailure {
                        scope: DurableStorageAdmissionScope::CatalogBudget,
                        ..
                    })
                ))
                .count(),
            1
        );
        let reserved = durable
            .storage_inventory_cached()
            .expect("accounted inventory reads")
            .expect("in-flight reservations remain observable");
        assert_eq!(reserved.inflight_catalog_reservation_bytes, required_bytes);
        assert_eq!(
            reserved.inflight_repository_reservation_bytes,
            policy.required_repository_bytes
        );
        assert_eq!(
            reserved.repositories[0].inflight_reservation_bytes,
            policy.required_repository_bytes
        );
        let accounting = durable
            .storage_accounting
            .lock()
            .expect("storage accounting remains available");
        assert_eq!(accounting.full_scan_count, 1);
        assert_eq!(accounting.repository_scan_count, 0);
    }

    #[test]
    fn activation_preflights_accounting_and_dirty_compaction_reconciles() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn activation_accounting() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let repository = PrivateDirectory::open(
            durable.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .expect("repository directory opens");
        mark_storage_accounting_dirty(&durable.storage_accounting);
        let before = private_entry_names(&repository).expect("repository entries read");

        assert_eq!(
            durable.activate_existing(receipt.repository, receipt.generation, 2, 2, 1, None),
            Err(FirstSliceError::Retention)
        );
        assert_eq!(
            private_entry_names(&repository).expect("repository entries remain readable"),
            before,
            "failed accounting preflight must not publish an activation marker"
        );

        durable
            .compact_repository(receipt.repository, &BTreeSet::from([receipt.generation]))
            .expect("dirty accounting reconciles after durable cleanup");
        durable
            .activate_existing(receipt.repository, receipt.generation, 2, 2, 1, None)
            .expect("activation succeeds after reconciliation");
        assert!(
            durable
                .storage_inventory_cached()
                .expect("accounted inventory reads")
                .is_some()
        );
    }

    #[test]
    fn dirty_accounting_with_an_active_reservation_fails_closed() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = RepositoryId::from_bytes([31; 16]);
        let policy = DurableStorageAdmissionPolicy {
            required_catalog_bytes: 1024,
            required_repository_bytes: 0,
            maximum_repository_bytes: u64::MAX,
            maximum_storage_bytes: u64::MAX,
            minimum_free_bytes: 0,
            repository_amplification: None,
        };
        let reservation = durable
            .ensure_staging_capacity(repository, policy)
            .expect("cold accounting reconciles")
            .expect("capacity is admitted")
            .1;
        mark_storage_accounting_dirty(&durable.storage_accounting);

        assert!(matches!(
            durable.ensure_staging_capacity(repository, policy),
            Err(FirstSliceError::Retention)
        ));
        assert!(matches!(
            durable.resize_staging_reservation(&reservation, policy),
            Err(FirstSliceError::Retention)
        ));
        durable
            .release_staging_reservation(reservation)
            .expect("reservation releases");
        let replacement = durable
            .ensure_staging_capacity(repository, policy)
            .expect("dirty accounting reconciles after the ledger drains")
            .expect("capacity is admitted")
            .1;
        drop(replacement);

        let accounting = durable
            .storage_accounting
            .lock()
            .expect("storage accounting remains available");
        assert_eq!(accounting.full_scan_count, 2);
        assert_eq!(accounting.repository_scan_count, 0);
    }

    #[test]
    fn large_generation_uses_bounded_source_packs_and_restores_exact_content() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        for ordinal in 0..PACKED_SOURCE_MIN_FILES {
            fs::write(
                fixture.path().join(format!("source-{ordinal:04}.txt")),
                format!("packed source {ordinal}\n"),
            )
            .expect("source fixture writes");
        }
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(60))
                .expect("deadline is representable"),
        );
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("packed generation publishes")
        };
        let generation = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(receipt.generation.to_string());
        let manifest: DurableGenerationManifest = serde_json::from_slice(
            &fs::read(generation.join(MANIFEST_FILENAME)).expect("manifest reads"),
        )
        .expect("manifest decodes");
        assert!(matches!(
            source_storage_layout(manifest.version, manifest.source_storage),
            Ok(DurableSourceLayout::Packed(_))
        ));
        let source_entries = fs::read_dir(generation.join(SOURCES_DIRECTORY))
            .expect("packed source directory reads")
            .collect::<Result<Vec<_>, _>>()
            .expect("packed source entries read");
        assert!(
            source_entries.len() < PACKED_SOURCE_MIN_FILES,
            "large generations must not materialize one durable file per source"
        );

        let catalog = open_test_catalog(paths.state_dir(), 2).expect("durable catalog reopens");
        let restored = catalog
            .restore_active(&cancellation)
            .expect("packed generation restores");
        let generation = restored.first().expect("one generation restores");
        assert_eq!(
            generation.verified.document().files.len(),
            PACKED_SOURCE_MIN_FILES
        );
        let file = generation
            .verified
            .document()
            .files
            .first()
            .expect("one restored file exists");
        let source = catalog
            .read_source(receipt.repository, receipt.generation, file, &cancellation)
            .expect("packed source reads");
        assert_eq!(source.file(), file.id);
        assert_eq!(source.content_hash(), file.content_hash);
        catalog
            .storage_inventory()
            .expect("verified inventory checks packed content");
        let pack_path = generation.receipt.generation.to_string();
        let pack_path = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(pack_path)
            .join(SOURCES_DIRECTORY)
            .join(source_pack_name(0));
        let mut corrupted = fs::read(&pack_path).expect("source pack reads");
        corrupted[0] ^= 1;
        fs::write(&pack_path, corrupted).expect("source pack corruption writes");
        assert_eq!(
            catalog.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            catalog.read_source(receipt.repository, receipt.generation, file, &cancellation,),
            Err(FirstSliceError::CatalogCorrupt)
        );
    }

    #[test]
    fn admission_reseed_build_failure_remains_dirty() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn admission_reseed() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let blobs = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(SOURCE_BLOBS_DIRECTORY);
        let blob = fs::read_dir(blobs)
            .expect("source blobs read")
            .next()
            .expect("one source blob exists")
            .expect("source blob entry reads");
        fs::remove_dir_all(blob.path()).expect("referenced blob is removed");
        mark_storage_accounting_dirty(&durable.storage_accounting);

        assert!(matches!(
            durable.ensure_staging_capacity(
                RepositoryId::from_bytes([41; 16]),
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 1024,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            ),
            Err(FirstSliceError::CatalogCorrupt)
        ));
        assert_eq!(durable.storage_inventory_cached(), Ok(None));
        assert!(
            durable
                .storage_accounting
                .lock()
                .expect("storage accounting remains available")
                .dirty
        );
    }

    #[test]
    fn dirty_reconcile_rejects_missing_referenced_blob_without_clearing_dirty() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn dirty_reconcile() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let repository = PrivateDirectory::open(
            durable.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .expect("repository directory opens");
        let blobs = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(SOURCE_BLOBS_DIRECTORY);
        let blob = fs::read_dir(blobs)
            .expect("source blobs read")
            .next()
            .expect("one source blob exists")
            .expect("source blob entry reads");
        fs::remove_dir_all(blob.path()).expect("referenced blob is removed");
        mark_storage_accounting_dirty(&durable.storage_accounting);
        let mut accounting = durable
            .storage_accounting
            .lock()
            .expect("storage accounting remains available");

        assert_eq!(
            durable.reconcile_repository_accounting(
                &mut accounting,
                receipt.repository,
                &repository,
            ),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert!(accounting.dirty);
    }

    #[test]
    fn dirty_accounting_with_foreign_reservation_does_not_publish_metadata() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn metadata_preflight() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let reservation = durable
            .ensure_staging_capacity(
                RepositoryId::from_bytes([42; 16]),
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 1024,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("accounting remains readable")
            .expect("foreign reservation is admitted")
            .1;
        mark_storage_accounting_dirty(&durable.storage_accounting);
        let repository = PrivateDirectory::open(
            durable.repositories.capability(),
            OsStr::new(&receipt.repository.to_string()),
        )
        .expect("repository directory opens");
        let before = private_entry_names(&repository).expect("repository entries read");

        assert_eq!(
            durable.write_repository_metadata(DurableRepositoryMetadata {
                version: REPOSITORY_METADATA_VERSION,
                sequence: 1,
                repository: receipt.repository,
                root_path: None,
                alias: Some("blocked rename".to_owned()),
            }),
            Err(FirstSliceError::Retention)
        );
        assert_eq!(
            private_entry_names(&repository).expect("repository entries remain readable"),
            before
        );
        drop(reservation);
    }

    #[test]
    fn successful_generation_uses_one_full_scan_and_bounded_target_reconciles() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn cached_accounting() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");

        let accounted = durable
            .storage_inventory_cached()
            .expect("accounted inventory reads")
            .expect("successful publication leaves clean accounting");
        {
            let accounting = durable
                .storage_accounting
                .lock()
                .expect("storage accounting remains available");
            assert_eq!(accounting.full_scan_count, 1);
            assert_eq!(accounting.repository_scan_count, 2);
        }
        let verified = durable
            .storage_inventory()
            .expect("verified inventory scans");
        assert_inventory_equal_ignoring_available(accounted, verified);
        let accounting = durable
            .storage_accounting
            .lock()
            .expect("storage accounting remains available");
        assert_eq!(accounting.full_scan_count, 2);
        assert_eq!(accounting.repository_scan_count, 2);
    }

    #[test]
    fn failed_finalize_rolls_back_accounting_and_reconciles_after_cleanup() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let durable = open_test_catalog(paths.state_dir(), 2).expect("catalog opens");
        let repository = RepositoryId::from_bytes([53; 16]);
        let generation = GenerationId::from_bytes([59; 20]);
        let prepared = durable
            .begin_generation(repository, generation)
            .expect("staging generation opens");
        let baseline = durable
            .storage_inventory()
            .expect("unreserved inventory establishes the physical baseline");
        assert_eq!(baseline.inflight_catalog_reservation_bytes, 0);
        let reservation = durable
            .ensure_staging_capacity(
                repository,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 16 * 1024,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("inventory remains readable")
            .expect("staging capacity is admitted")
            .1;
        let reserved = durable
            .storage_inventory_cached()
            .expect("reserved accounting remains readable")
            .expect("reservation keeps accounting reconciled");
        assert_eq!(reserved.inflight_catalog_reservation_bytes, 16 * 1024);
        fs::write(prepared.path().join("staged.bin"), vec![0_u8; 1024])
            .expect("staged payload writes");
        prepared
            .account_external_staging_bytes(1024)
            .expect("staged payload is accounted");
        let sealed = DurableSealedGeneration {
            prepared,
            repository,
            materialized_bytes: 1024,
            manifest_written_bytes: 0,
            scanned_generation: ScannedGeneration {
                repository,
                generation,
                parent: None,
                tree_bytes: 1024,
                source_blobs: BTreeMap::new(),
            },
        };
        let result = durable
            .finalize_repository_capacity(
                &reservation,
                sealed,
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 16 * 1024,
                    required_repository_bytes: DURABLE_PUBLICATION_RESIDUAL_BYTES,
                    maximum_repository_bytes: 1024,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: Some(DurableRepositoryAmplificationPolicy {
                        examined_source_bytes: 1024,
                        emitted_fact_bytes: 0,
                        source_factor: 1,
                        oracle_factor: 1,
                        retained_generations: 1,
                        fixed_headroom_bytes: 16 * 1024,
                    }),
                },
            )
            .expect("target inventory remains readable");
        let failure = match result {
            Ok(_) => panic!("absolute repository cap rejects the sealed candidate"),
            Err(failure) => failure,
        };
        assert_eq!(
            failure.scope,
            DurableStorageAdmissionScope::RepositoryBudget
        );
        assert_eq!(failure.limit_bytes, 1024);
        assert_eq!(
            failure.repository_amplification,
            Some(DurableRepositoryAmplification {
                examined_source_bytes: 1024,
                emitted_fact_bytes: 0,
                effective_factor: 3,
                retained_generations: 1,
                absolute_limit_bytes: 1024,
                amplification_limit_bytes: 18 * 1024,
            })
        );
        let rejected = durable
            .storage_inventory()
            .expect("failed finalization retains conservative admission accounting");
        assert_eq!(rejected.inflight_catalog_reservation_bytes, 16 * 1024);
        durable
            .release_staging_reservation(reservation)
            .expect("reservation releases");
        assert_eq!(
            durable
                .storage_inventory_cached()
                .expect("accounted inventory reads"),
            None
        );
        let verified = durable
            .storage_inventory()
            .expect("post-cleanup inventory verifies");
        assert_eq!(verified.inflight_catalog_reservation_bytes, 0);
        assert_inventory_equal_ignoring_available(baseline, verified);
        let accounting = durable
            .storage_accounting
            .lock()
            .expect("storage accounting remains available");
        assert_eq!(accounting.full_scan_count, 3);
        assert_eq!(accounting.repository_scan_count, 1);
    }

    #[test]
    fn cold_storage_inventory_deduplicates_blobs_and_tracks_compaction() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        let stable = "pub fn stable() -> u32 { 1 }\n";
        let first_changed = "pub fn changed() -> u32 { 1 }\n";
        let second_changed = "pub fn changed() -> u32 { 2 }\n";
        let third_changed = "pub fn changed() -> u32 { 3 }\n";
        fs::write(fixture.path().join("src/stable.rs"), stable).expect("stable source writes");
        let changed_path = fixture.path().join("src/changed.rs");
        fs::write(&changed_path, first_changed).expect("initial changed source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );

        let (first, second, third) = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let first = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("first generation publishes");
            fs::write(&changed_path, second_changed).expect("second source writes");
            let second = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("second generation publishes");
            let before_compaction = service
                .durable
                .as_ref()
                .expect("durable catalog exists")
                .storage_inventory()
                .expect("live inventory scans");
            assert_eq!(before_compaction.generations.len(), 2);
            assert_eq!(
                before_compaction.shared_source_bytes,
                u64::try_from(stable.len()).expect("fixture byte count is representable")
            );

            fs::write(&changed_path, third_changed).expect("third source writes");
            let third = service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("third generation publishes");
            let durable = service.durable.as_ref().expect("durable catalog exists");
            let cached_digests = durable
                .storage_accounting
                .lock()
                .expect("storage accounting remains available")
                .verified_source_blobs
                .keys()
                .filter_map(|(repository, digest)| {
                    (*repository == third.repository).then_some(*digest)
                })
                .collect::<BTreeSet<_>>();
            let live_digests = fs::read_dir(
                paths
                    .state_dir()
                    .join(DURABLE_DIRECTORY)
                    .join(REPOSITORIES_DIRECTORY)
                    .join(third.repository.to_string())
                    .join(SOURCE_BLOBS_DIRECTORY),
            )
            .expect("source blobs read")
            .map(|entry| {
                let entry = entry.expect("source blob entry reads");
                ContentHash::from_str(
                    entry
                        .file_name()
                        .to_str()
                        .expect("source blob name is Unicode"),
                )
                .expect("source blob name is a digest")
            })
            .collect::<BTreeSet<_>>();
            assert!(
                cached_digests.is_subset(&live_digests),
                "target accounting retains verification only for live blobs"
            );
            assert!(
                !cached_digests.contains(&content_hash_bytes(first_changed.as_bytes())),
                "target accounting prunes the compacted blob verification entry"
            );
            (first, second, third)
        };

        let catalog =
            open_test_catalog(paths.state_dir(), 2).expect("cold durable catalog reopens");
        let inventory = catalog
            .storage_inventory()
            .expect("cold inventory reconstructs from durable state");
        assert_eq!(inventory.generations.len(), 2);
        assert!(
            inventory
                .generations
                .iter()
                .all(|generation| generation.generation != first.generation)
        );
        let active = inventory
            .generations
            .iter()
            .find(|generation| generation.active)
            .expect("one active generation is identified");
        assert_eq!(active.generation, third.generation);
        assert_eq!(active.parent, Some(second.generation));
        let predecessor = inventory
            .generations
            .iter()
            .find(|generation| generation.predecessor)
            .expect("one predecessor generation is identified");
        assert_eq!(predecessor.generation, second.generation);
        assert_eq!(inventory.active_generation_bytes, active.unique_bytes);
        assert_eq!(
            inventory.predecessor_generation_bytes,
            predecessor.unique_bytes
        );
        assert_eq!(
            inventory.shared_source_bytes,
            u64::try_from(stable.len()).expect("fixture byte count is representable")
        );
        assert_eq!(inventory.temporary_bytes, 0);
        assert_eq!(inventory.reclaimable_bytes, 0);
        assert_eq!(inventory.pinned_bytes, 0);
        assert_eq!(inventory.quarantine_bytes, 0);
        assert_eq!(
            inventory.total_physical_bytes,
            inventory
                .active_generation_bytes
                .checked_add(inventory.predecessor_generation_bytes)
                .and_then(|bytes| bytes.checked_add(inventory.other_retained_generation_bytes))
                .and_then(|bytes| bytes.checked_add(inventory.source_pool_bytes))
                .and_then(|bytes| bytes.checked_add(inventory.temporary_bytes))
                .and_then(|bytes| bytes.checked_add(inventory.reclaimable_bytes))
                .and_then(|bytes| bytes.checked_add(inventory.repository_overhead_bytes))
                .and_then(|bytes| bytes.checked_add(inventory.quarantine_bytes))
                .expect("fixture byte total is representable")
        );
        assert!(inventory.shared_source_bytes < inventory.source_pool_bytes);
        assert_eq!(
            inventory.repositories[0].physical_bytes,
            inventory.total_physical_bytes
        );
    }

    #[test]
    fn storage_inventory_rejects_corrupt_shared_blob_content() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn inventory_corruption() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("generation publishes")
        };
        let blobs = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(SOURCE_BLOBS_DIRECTORY);
        let blob = fs::read_dir(blobs)
            .expect("source blobs read")
            .next()
            .expect("one source blob exists")
            .expect("source blob entry reads");
        let payload_path = blob.path().join(SOURCE_BLOB_PAYLOAD_FILENAME);
        let mut corrupted = fs::read(&payload_path).expect("source blob reads");
        corrupted[0] ^= 1;
        fs::write(&payload_path, corrupted).expect("same-length corruption writes");

        let catalog = open_test_catalog(paths.state_dir(), 2).expect("durable catalog reopens");
        let reservation = catalog
            .ensure_staging_capacity(
                RepositoryId::from_bytes([67; 16]),
                DurableStorageAdmissionPolicy {
                    required_catalog_bytes: 1024,
                    required_repository_bytes: 0,
                    maximum_repository_bytes: u64::MAX,
                    maximum_storage_bytes: u64::MAX,
                    minimum_free_bytes: 0,
                    repository_amplification: None,
                },
            )
            .expect("physical accounting does not claim content verification")
            .expect("unchanged physical size remains admissible")
            .1;
        catalog
            .release_staging_reservation(reservation)
            .expect("reservation releases");
        assert_eq!(
            catalog.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
    }

    #[test]
    fn verified_inventory_build_failure_invalidates_accounting() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn missing_inventory_blob() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let catalog = service.durable.as_ref().expect("durable catalog exists");
        assert!(
            catalog
                .storage_inventory_cached()
                .expect("accounted inventory reads")
                .is_some()
        );
        let blobs = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(SOURCE_BLOBS_DIRECTORY);
        let blob = fs::read_dir(blobs)
            .expect("source blobs read")
            .next()
            .expect("one source blob exists")
            .expect("source blob entry reads");
        fs::remove_dir_all(blob.path()).expect("referenced blob is removed");

        assert_eq!(
            catalog.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            catalog.storage_inventory_cached(),
            Ok(None),
            "failed aggregation invalidates accounted support evidence"
        );
    }

    #[test]
    fn verified_inventory_revalidates_changed_blob_from_disk() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn inventory_cache() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            service
                .index_rust_fixture(fixture.path(), &cancellation)
                .expect("generation publishes")
        };
        let blobs = paths
            .state_dir()
            .join(DURABLE_DIRECTORY)
            .join(REPOSITORIES_DIRECTORY)
            .join(receipt.repository.to_string())
            .join(SOURCE_BLOBS_DIRECTORY);
        let payload_path = fs::read_dir(blobs)
            .expect("source blobs read")
            .next()
            .expect("one source blob exists")
            .expect("source blob entry reads")
            .path()
            .join(SOURCE_BLOB_PAYLOAD_FILENAME);
        let catalog = open_test_catalog(paths.state_dir(), 2).expect("durable catalog reopens");
        catalog
            .storage_inventory()
            .expect("cold inventory verifies source content");
        assert_eq!(
            catalog
                .storage_accounting
                .lock()
                .expect("verified blob cache remains available")
                .verified_source_blobs
                .len(),
            1
        );
        catalog
            .storage_inventory()
            .expect("a repeated full verification remains valid");
        let restored = catalog
            .restore_active(&cancellation)
            .expect("active generation restores");
        let file = restored
            .first()
            .and_then(|generation| generation.verified.document().files.first())
            .expect("restored generation contains one file");
        let mut corrupted = fs::read(&payload_path).expect("source blob reads");
        corrupted[0] ^= 1;
        fs::write(&payload_path, corrupted).expect("same-length corruption writes");

        assert_eq!(
            catalog.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            catalog.storage_inventory_cached(),
            Ok(None),
            "failed verification invalidates accounted support evidence"
        );
        assert_eq!(
            catalog.read_source(receipt.repository, receipt.generation, file, &cancellation),
            Err(FirstSliceError::CatalogCorrupt)
        );
        let reopened =
            open_test_catalog(paths.state_dir(), 2).expect("fresh durable catalog reopens");
        assert_eq!(
            reopened.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
    }

    #[test]
    fn targeted_accounting_defers_changed_blob_verification() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("runtime paths are valid");
        paths.prepare_owner().expect("private paths prepare");
        let fixture = durable_test_tempdir();
        fs::write(
            fixture.path().join("lib.rs"),
            "pub fn targeted_cache() -> u32 { 1 }\n",
        )
        .expect("source writes");
        let cancellation = Cancellation::with_deadline(
            std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("deadline is representable"),
        );
        let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service initializes");
        let receipt = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("generation publishes");
        let durable = service.durable.as_ref().expect("durable catalog exists");
        let payload_path = fs::read_dir(
            paths
                .state_dir()
                .join(DURABLE_DIRECTORY)
                .join(REPOSITORIES_DIRECTORY)
                .join(receipt.repository.to_string())
                .join(SOURCE_BLOBS_DIRECTORY),
        )
        .expect("source blobs read")
        .next()
        .expect("one source blob exists")
        .expect("source blob entry reads")
        .path()
        .join(SOURCE_BLOB_PAYLOAD_FILENAME);
        std::thread::sleep(Duration::from_millis(20));
        let mut corrupted = fs::read(&payload_path).expect("source blob reads");
        corrupted[0] ^= 1;
        fs::write(&payload_path, corrupted).expect("same-length corruption writes");

        durable
            .compact_repository(receipt.repository, &BTreeSet::from([receipt.generation]))
            .expect("physical accounting remains independent of payload integrity");
        assert!(
            durable
                .storage_inventory_cached()
                .expect("accounted inventory reads")
                .is_some()
        );
        assert_eq!(
            durable.storage_inventory(),
            Err(FirstSliceError::CatalogCorrupt)
        );
        assert_eq!(
            durable.storage_inventory_cached(),
            Ok(None),
            "failed verified inventory invalidates physical accounting"
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
