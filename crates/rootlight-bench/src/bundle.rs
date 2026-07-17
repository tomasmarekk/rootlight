//! Immutable result-bundle publication and bounded checksum verification.
//!
//! Publication performs all serialization and size accounting before it
//! creates the staging directory. Verification bounds directory traversal and
//! every read before allocating artifact contents.

use std::{
    borrow::Cow,
    collections::BTreeMap,
    ffi::OsStr,
    fmt,
    io::{self, Read as _, Write as _},
    path::Path,
};

#[cfg(test)]
use std::fs;

use serde::{
    Deserialize, Serialize,
    de::{IgnoredAny, SeqAccess, Visitor},
};
use sha2::{Digest as _, Sha256};

use cap_fs_ext::DirExt as _;

use crate::integrity::{FixedArtifactSource, is_fixed_artifact, validate_fixed_artifacts};
use crate::{
    AgentTrajectory, BenchmarkCommand, BuildProvenance, CoverageEvidence, DatasetManifest,
    EnvironmentEvidence, QualityEvidence, RawSample, ResultSummary,
};

pub(crate) const ENVIRONMENT_FILE: &str = "environment.json";
pub(crate) const DATASET_MANIFEST_FILE: &str = "dataset-manifest.json";
pub(crate) const BUILD_PROVENANCE_FILE: &str = "build-provenance.json";
pub(crate) const COMMAND_FILE: &str = "command.json";
pub(crate) const RAW_SAMPLES_FILE: &str = "raw-samples.jsonl";
pub(crate) const SUMMARY_FILE: &str = "summary.json";
pub(crate) const COVERAGE_FILE: &str = "coverage.json";
pub(crate) const QUALITY_FILE: &str = "quality.json";
pub(crate) const AGENT_TRAJECTORIES_FILE: &str = "agent-trajectories.jsonl";
const CHECKSUMS_FILE: &str = "checksums.txt";
const FIXED_ARTIFACT_COUNT: usize = 9;

const HARD_MAX_RAW_SAMPLES: usize = 250_000;
const HARD_MAX_AGENT_TRAJECTORIES: usize = 25_000;
const HARD_MAX_ARTIFACTS_PER_CLASS: usize = 512;
const HARD_MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_PROFILE_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_LOG_BYTES: u64 = 32 * 1024 * 1024;
const HARD_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_CHECKSUM_LINES: usize = 2_048;
const HARD_MAX_CHECKSUM_BYTES: u64 = 512 * 1024;
const HARD_MAX_DEPTH: usize = 4;
const HARD_MAX_FILE_COUNT: usize = 2_048;
const HARD_MAX_DIRECTORY_ENTRIES: usize = 4_096;
const HARD_MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const HARD_MAX_MANIFEST_ENTRIES: usize = 250_000;
const HARD_MAX_COMMAND_ARGUMENTS: usize = 256;
const HARD_MAX_STRING_BYTES: usize = 4_096;
const HARD_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_DATASET_SOURCE_BYTES: u64 = 1 << 40;
const HARD_MAX_OPERATIONAL_LOG_RECORDS: usize = 100_000;

/// Checked resource ceilings for bundle publication, verification, and inputs.
///
/// Callers may lower fields for a constrained environment. Every public
/// operation rejects zero values and values above the crate's absolute
/// ceilings before performing filesystem or decoding work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleLimits {
    /// Maximum retained raw samples.
    pub max_raw_samples: usize,
    /// Maximum retained agent trajectories.
    pub max_agent_trajectories: usize,
    /// Maximum profile artifacts and maximum log artifacts.
    pub max_artifacts_per_class: usize,
    /// Maximum bytes in any single artifact.
    pub max_artifact_bytes: u64,
    /// Maximum bytes across all profile artifacts.
    pub max_profile_bytes: u64,
    /// Maximum bytes across all source-free log artifacts.
    pub max_log_bytes: u64,
    /// Maximum bytes across every bundle file, including checksums.
    pub max_total_bytes: u64,
    /// Maximum checksum-manifest lines.
    pub max_checksum_lines: usize,
    /// Maximum checksum-manifest bytes.
    pub max_checksum_bytes: u64,
    /// Maximum bundle traversal depth, with the bundle root at depth zero.
    pub max_depth: usize,
    /// Maximum regular files in a bundle, including the checksum manifest.
    pub max_file_count: usize,
    /// Maximum directory entries visited, including directories.
    pub max_directory_entries: usize,
    /// Maximum bytes accepted by a bounded JSON input decoder.
    pub max_input_bytes: usize,
    /// Maximum entries accepted from a dataset manifest.
    pub max_manifest_entries: usize,
    /// Maximum normalized command arguments.
    pub max_command_arguments: usize,
    /// Maximum bytes in any decoded string field.
    pub max_string_bytes: usize,
    /// Maximum declared or observed bytes in one dataset snapshot.
    pub max_snapshot_bytes: u64,
    /// Maximum declared bytes across the dataset.
    pub max_dataset_source_bytes: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_raw_samples: HARD_MAX_RAW_SAMPLES,
            max_agent_trajectories: HARD_MAX_AGENT_TRAJECTORIES,
            max_artifacts_per_class: HARD_MAX_ARTIFACTS_PER_CLASS,
            max_artifact_bytes: HARD_MAX_ARTIFACT_BYTES,
            max_profile_bytes: HARD_MAX_PROFILE_BYTES,
            max_log_bytes: HARD_MAX_LOG_BYTES,
            max_total_bytes: HARD_MAX_TOTAL_BYTES,
            max_checksum_lines: HARD_MAX_CHECKSUM_LINES,
            max_checksum_bytes: HARD_MAX_CHECKSUM_BYTES,
            max_depth: HARD_MAX_DEPTH,
            max_file_count: HARD_MAX_FILE_COUNT,
            max_directory_entries: HARD_MAX_DIRECTORY_ENTRIES,
            max_input_bytes: HARD_MAX_INPUT_BYTES,
            max_manifest_entries: HARD_MAX_MANIFEST_ENTRIES,
            max_command_arguments: HARD_MAX_COMMAND_ARGUMENTS,
            max_string_bytes: HARD_MAX_STRING_BYTES,
            max_snapshot_bytes: HARD_MAX_SNAPSHOT_BYTES,
            max_dataset_source_bytes: HARD_MAX_DATASET_SOURCE_BYTES,
        }
    }
}

impl BundleLimits {
    pub(crate) fn validate(self) -> Result<Self, BundleError> {
        let valid = self.max_raw_samples > 0
            && self.max_raw_samples <= HARD_MAX_RAW_SAMPLES
            && self.max_agent_trajectories > 0
            && self.max_agent_trajectories <= HARD_MAX_AGENT_TRAJECTORIES
            && self.max_artifacts_per_class > 0
            && self.max_artifacts_per_class <= HARD_MAX_ARTIFACTS_PER_CLASS
            && self.max_artifact_bytes > 0
            && self.max_artifact_bytes <= HARD_MAX_ARTIFACT_BYTES
            && self.max_profile_bytes > 0
            && self.max_profile_bytes <= HARD_MAX_PROFILE_BYTES
            && self.max_log_bytes > 0
            && self.max_log_bytes <= HARD_MAX_LOG_BYTES
            && self.max_total_bytes > 0
            && self.max_total_bytes <= HARD_MAX_TOTAL_BYTES
            && self.max_checksum_lines > 0
            && self.max_checksum_lines <= HARD_MAX_CHECKSUM_LINES
            && self.max_checksum_bytes > 0
            && self.max_checksum_bytes <= HARD_MAX_CHECKSUM_BYTES
            && self.max_depth > 0
            && self.max_depth <= HARD_MAX_DEPTH
            && self.max_file_count > FIXED_ARTIFACT_COUNT
            && self.max_file_count <= HARD_MAX_FILE_COUNT
            && self.max_directory_entries >= FIXED_ARTIFACT_COUNT + 3
            && self.max_directory_entries <= HARD_MAX_DIRECTORY_ENTRIES
            && self.max_input_bytes > 0
            && self.max_input_bytes <= HARD_MAX_INPUT_BYTES
            && self.max_manifest_entries > 0
            && self.max_manifest_entries <= HARD_MAX_MANIFEST_ENTRIES
            && self.max_command_arguments > 0
            && self.max_command_arguments <= HARD_MAX_COMMAND_ARGUMENTS
            && self.max_string_bytes > 0
            && self.max_string_bytes <= HARD_MAX_STRING_BYTES
            && self.max_snapshot_bytes > 0
            && self.max_snapshot_bytes <= HARD_MAX_SNAPSHOT_BYTES
            && self.max_dataset_source_bytes > 0
            && self.max_dataset_source_bytes <= HARD_MAX_DATASET_SOURCE_BYTES;
        if !valid {
            return Err(BundleError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Closed source-free event vocabulary for operational benchmark logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationalEvent {
    /// A benchmark run started.
    BenchmarkStarted,
    /// One benchmark sample started.
    SampleStarted,
    /// One benchmark sample completed.
    SampleCompleted,
    /// A benchmark run completed.
    BenchmarkCompleted,
    /// An immutable result bundle was published.
    BundlePublished,
}

impl OperationalEvent {
    /// Parses one allow-listed source-free event label.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::InvalidLog`] for values outside the closed
    /// operational vocabulary.
    pub fn from_label(label: &str) -> Result<Self, BundleError> {
        match label {
            "benchmark_started" => Ok(Self::BenchmarkStarted),
            "sample_started" => Ok(Self::SampleStarted),
            "sample_completed" => Ok(Self::SampleCompleted),
            "benchmark_completed" => Ok(Self::BenchmarkCompleted),
            "bundle_published" => Ok(Self::BundlePublished),
            _ => Err(BundleError::InvalidLog),
        }
    }
}

/// Closed source-free terminal-status vocabulary for operational logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationalStatus {
    /// The operation has started but is not terminal.
    Started,
    /// The operation succeeded.
    Succeeded,
    /// The operation failed.
    Failed,
    /// The operation exceeded its deadline.
    TimedOut,
    /// The operation was cancelled.
    Cancelled,
    /// Required telemetry was unavailable.
    Unavailable,
}

impl OperationalStatus {
    /// Parses one allow-listed source-free status label.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::InvalidLog`] for values outside the closed
    /// operational vocabulary.
    pub fn from_label(label: &str) -> Result<Self, BundleError> {
        match label {
            "started" => Ok(Self::Started),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "timed_out" => Ok(Self::TimedOut),
            "cancelled" => Ok(Self::Cancelled),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(BundleError::InvalidLog),
        }
    }
}

/// One source-free structured operational log record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalLogRecord {
    /// Monotonic record sequence within this artifact.
    pub sequence: u64,
    /// Closed operation event.
    pub event: OperationalEvent,
    /// Closed operation status.
    pub status: OperationalStatus,
    /// Optional sample ordinal.
    pub sample_ordinal: Option<u64>,
    /// Optional elapsed duration in monotonic nanoseconds.
    pub elapsed_ns: Option<u64>,
}

/// Structured operational log without arbitrary string or byte payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalLog {
    records: Vec<OperationalLogRecord>,
}

impl OperationalLog {
    /// Creates a bounded structured operational log.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::InvalidLog`] if records are empty, exceed the
    /// hard ceiling, or do not use strictly increasing sequence numbers.
    pub fn new(records: Vec<OperationalLogRecord>) -> Result<Self, BundleError> {
        if records.is_empty()
            || records.len() > HARD_MAX_OPERATIONAL_LOG_RECORDS
            || records
                .windows(2)
                .any(|pair| pair[0].sequence >= pair[1].sequence)
        {
            return Err(BundleError::InvalidLog);
        }
        Ok(Self { records })
    }

    /// Returns the structured source-free records.
    #[must_use]
    pub fn records(&self) -> &[OperationalLogRecord] {
        &self.records
    }
}

impl Serialize for OperationalLog {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.records.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OperationalLog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(OperationalLogVisitor)
    }
}

struct OperationalLogVisitor;

impl<'de> Visitor<'de> for OperationalLogVisitor {
    type Value = OperationalLog;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded operational log array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let hinted = sequence.size_hint().unwrap_or(0);
        if hinted > HARD_MAX_OPERATIONAL_LOG_RECORDS {
            return Err(serde::de::Error::custom(
                "operational log record limit exceeded",
            ));
        }
        let mut records = Vec::new();
        records
            .try_reserve_exact(hinted)
            .map_err(|_| serde::de::Error::custom("operational log allocation failed"))?;
        loop {
            if records.len() == HARD_MAX_OPERATIONAL_LOG_RECORDS {
                if sequence.next_element::<IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "operational log record limit exceeded",
                    ));
                }
                break;
            }
            let Some(record) = sequence.next_element::<OperationalLogRecord>()? else {
                break;
            };
            if records.len() == records.capacity() {
                records
                    .try_reserve(1)
                    .map_err(|_| serde::de::Error::custom("operational log allocation failed"))?;
            }
            records.push(record);
        }
        OperationalLog::new(records).map_err(serde::de::Error::custom)
    }
}

/// Complete in-memory contents of one normative result bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultBundle {
    /// Environment evidence.
    pub environment: EnvironmentEvidence,
    /// Immutable dataset manifest.
    pub dataset_manifest: DatasetManifest,
    /// Source and build provenance.
    pub build_provenance: BuildProvenance,
    /// Normalized command and trial policy.
    pub command: BenchmarkCommand,
    /// Retained raw samples.
    pub raw_samples: Vec<RawSample>,
    /// Aggregate summary.
    pub summary: ResultSummary,
    /// Coverage evidence.
    pub coverage: CoverageEvidence,
    /// Quality evidence.
    pub quality: QualityEvidence,
    /// Retained agent trajectories.
    pub agent_trajectories: Vec<AgentTrajectory>,
    /// Profile artifacts keyed by validated relative artifact name.
    pub profiles: BTreeMap<String, Vec<u8>>,
    /// Source-free log artifacts keyed by validated relative artifact name.
    pub logs: BTreeMap<String, OperationalLog>,
}

/// Publishes one immutable result bundle using the crate's hard defaults.
///
/// # Errors
///
/// Returns [`BundleError`] for invalid input, exceeded limits, serialization,
/// existing destinations, or filesystem publication failures.
pub fn publish_bundle(bundle: &ResultBundle, destination: &Path) -> Result<(), BundleError> {
    publish_bundle_with_limits(bundle, destination, BundleLimits::default())
}

/// Publishes one immutable result bundle with checked caller-selected limits.
///
/// All artifacts are serialized and byte-accounted before a staging directory
/// is created. The destination parent must already exist. Publication writes
/// and syncs a complete checksummed sibling directory, then atomically installs
/// that directory without replacing an existing destination. A process crash
/// before installation can leave an inert staging sibling but never exposes a
/// partial destination. On Unix, staging and parent directories are synced
/// around installation. Rust's standard library does not expose portable
/// Windows directory-handle syncing, so Windows retains synced files plus the
/// atomic no-replace directory rename as a best-effort durability fallback.
///
/// # Errors
///
/// Returns [`BundleError`] for invalid input, exceeded limits, serialization,
/// existing destinations, or filesystem publication failures.
pub fn publish_bundle_with_limits(
    bundle: &ResultBundle,
    destination: &Path,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    publish_bundle_with_fault(bundle, destination, limits, None)
}

/// Verifies a result bundle using the crate's hard defaults.
///
/// # Errors
///
/// Returns [`BundleError`] for malformed manifests, exceeded limits, I/O
/// failures, missing or unexpected artifacts, and checksum mismatches.
pub fn verify_bundle(destination: &Path) -> Result<(), BundleError> {
    verify_bundle_with_limits(destination, BundleLimits::default())
}

/// Verifies a result bundle with checked caller-selected resource limits.
///
/// # Errors
///
/// Returns [`BundleError`] for malformed manifests, exceeded limits, I/O
/// failures, missing or unexpected artifacts, and checksum mismatches.
pub fn verify_bundle_with_limits(
    destination: &Path,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    verify_bundle_with_control(destination, limits, || {}, |_| {}, || {})
}

fn verify_bundle_with_control<R, F, C>(
    destination: &Path,
    limits: BundleLimits,
    after_root_open: R,
    before_file_open: F,
    after_collection: C,
) -> Result<(), BundleError>
where
    R: FnOnce(),
    F: FnMut(&str),
    C: FnOnce(),
{
    let limits = limits.validate()?;
    let parent_path = destination_parent(destination)?;
    let destination_name = destination
        .file_name()
        .ok_or(BundleError::InvalidDestination)?;
    let parent = cap_std::fs::Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority())
        .map_err(|source| BundleError::Io {
            operation: "open result parent directory",
            source,
        })?;
    let root = parent
        .open_dir_nofollow(destination_name)
        .map_err(|source| BundleError::Io {
            operation: "open result bundle",
            source,
        })?;
    let root_metadata = root.dir_metadata().map_err(|source| BundleError::Io {
        operation: "inspect result bundle",
        source,
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(BundleError::UnsupportedArtifactType);
    }
    after_root_open();
    let mut observed = collect_files(root, limits, before_file_open)?;
    after_collection();
    let checksum_size = observed
        .size(CHECKSUMS_FILE)
        .ok_or(BundleError::ArtifactSetMismatch)?;
    if checksum_size > limits.max_checksum_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "checksum_bytes",
        });
    }
    let checksum_bytes = read_bounded(
        observed
            .file_mut(CHECKSUMS_FILE)
            .ok_or(BundleError::ArtifactSetMismatch)?,
        limits.max_checksum_bytes,
        "read checksum manifest",
    )?;
    let checksum_text =
        std::str::from_utf8(&checksum_bytes).map_err(|_| BundleError::InvalidChecksumManifest)?;
    let expected = parse_checksums(checksum_text, limits)?;
    if !observed.matches_expected_paths(&expected) {
        return Err(BundleError::ArtifactSetMismatch);
    }
    preflight_artifact_classes(&expected, &observed, limits)?;
    let mut fixed_artifacts = BTreeMap::new();
    let mut profiles = BTreeMap::new();
    let mut logs = BTreeMap::new();
    for (relative, checksum) in expected {
        let bytes = read_bounded(
            observed
                .file_mut(&relative)
                .ok_or(BundleError::ArtifactSetMismatch)?,
            limits.max_artifact_bytes,
            "read result artifact",
        )?;
        if sha256_hex(&bytes) != checksum {
            return Err(BundleError::ChecksumMismatch);
        }
        if is_fixed_artifact(&relative) {
            fixed_artifacts.insert(relative, bytes);
        } else if let Some(name) = relative.strip_prefix("profiles/") {
            profiles.insert(name.to_owned(), bytes);
        } else if let Some(name) = relative.strip_prefix("logs/") {
            let log = decode_operational_log(&bytes, limits)?;
            logs.insert(name.to_owned(), log);
        }
    }
    validate_fixed_artifacts(&fixed_artifacts, limits)?;
    // Keep non-fixed artifacts alive through all semantic verification so
    // class-level validation cannot accidentally regress to checksum-only.
    drop((profiles, logs));
    Ok(())
}

fn preflight_artifact_classes(
    expected: &BTreeMap<String, String>,
    observed: &OpenedArtifacts,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    let mut profile_count = 0_usize;
    let mut log_count = 0_usize;
    let mut profile_bytes = 0_u64;
    let mut log_bytes = 0_u64;
    for relative in expected.keys() {
        let size = observed
            .size(relative)
            .ok_or(BundleError::ArtifactSetMismatch)?;
        if relative.starts_with("profiles/") {
            profile_count = profile_count
                .checked_add(1)
                .ok_or(BundleError::LimitExceeded {
                    resource: "artifact_count",
                })?;
            profile_bytes = profile_bytes
                .checked_add(size)
                .ok_or(BundleError::LimitExceeded {
                    resource: "profile_bytes",
                })?;
        } else if relative.starts_with("logs/") {
            log_count = log_count.checked_add(1).ok_or(BundleError::LimitExceeded {
                resource: "artifact_count",
            })?;
            log_bytes = log_bytes
                .checked_add(size)
                .ok_or(BundleError::LimitExceeded {
                    resource: "log_bytes",
                })?;
        }
    }
    check_count(
        profile_count,
        limits.max_artifacts_per_class,
        "artifact_count",
    )?;
    check_count(log_count, limits.max_artifacts_per_class, "artifact_count")?;
    if profile_bytes > limits.max_profile_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "profile_bytes",
        });
    }
    if log_bytes > limits.max_log_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "log_bytes",
        });
    }
    Ok(())
}

fn decode_operational_log(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<OperationalLog, BundleError> {
    let length = u64::try_from(bytes.len()).map_err(|_| BundleError::LimitExceeded {
        resource: "log_bytes",
    })?;
    if length > limits.max_artifact_bytes
        || bytes.len() <= 1
        || !bytes.ends_with(b"\n")
        || bytes[..bytes.len() - 1]
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return Err(BundleError::InvalidLog);
    }
    let log = serde_json::from_slice::<OperationalLog>(&bytes[..bytes.len() - 1])
        .map_err(|_| BundleError::InvalidLog)?;
    let limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    if json_bytes(&log, limit)? != bytes {
        return Err(BundleError::InvalidLog);
    }
    Ok(log)
}

fn publish_bundle_with_fault(
    bundle: &ResultBundle,
    destination: &Path,
    limits: BundleLimits,
    fail_after_writes: Option<usize>,
) -> Result<(), BundleError> {
    publish_bundle_with_control(bundle, destination, limits, fail_after_writes, || {})
}

fn publish_bundle_with_control<F>(
    bundle: &ResultBundle,
    destination: &Path,
    limits: BundleLimits,
    fail_after_writes: Option<usize>,
    before_install: F,
) -> Result<(), BundleError>
where
    F: FnOnce(),
{
    let limits = limits.validate()?;
    let artifacts = build_artifacts(bundle, limits)?;
    let parent_path = destination_parent(destination)?;
    let destination_name = destination
        .file_name()
        .ok_or(BundleError::InvalidDestination)?;
    let parent = cap_std::fs::Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority())
        .map_err(|source| BundleError::Io {
            operation: "open result parent directory",
            source,
        })?;
    let staging = create_staging_directory(&parent)?;
    let preparation = write_bundle(&artifacts, &staging.directory, fail_after_writes)
        .and_then(|()| {
            sync_cap_directory(
                &staging
                    .directory
                    .open_dir_nofollow("profiles")
                    .map_err(|source| BundleError::Io {
                        operation: "open profiles directory",
                        source,
                    })?,
                "sync profiles directory",
            )
        })
        .and_then(|()| {
            sync_cap_directory(
                &staging
                    .directory
                    .open_dir_nofollow("logs")
                    .map_err(|source| BundleError::Io {
                        operation: "open logs directory",
                        source,
                    })?,
                "sync logs directory",
            )
        })
        .and_then(|()| sync_cap_directory(&staging.directory, "sync staging directory"))
        .and_then(|()| sync_cap_directory(&parent, "sync result parent directory"));
    if let Err(error) = preparation {
        remove_staging_directory(staging)?;
        sync_cap_directory(&parent, "sync result parent directory")?;
        return Err(error);
    }
    before_install();
    if let Err(error) =
        install_staged_bundle(&parent, &parent_path, &staging.name, destination_name)
    {
        remove_staging_directory(staging)?;
        sync_cap_directory(&parent, "sync result parent directory")?;
        return Err(error);
    }
    drop(staging);
    sync_cap_directory(&parent, "sync result parent directory")
}

struct StagingDirectory {
    name: String,
    directory: cap_std::fs::Dir,
}

fn create_staging_directory(parent: &cap_std::fs::Dir) -> Result<StagingDirectory, BundleError> {
    const PREFIX: &str = ".rootlight-result-partial-";
    for _ in 0..16 {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(BundleError::Randomness)?;
        let mut name = String::new();
        name.try_reserve_exact(PREFIX.len() + random.len() * 2)
            .map_err(|_| BundleError::AllocationFailed)?;
        name.push_str(PREFIX);
        for byte in random {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            name.push(char::from(HEX[usize::from(byte >> 4)]));
            name.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        match parent.create_dir(&name) {
            Ok(()) => {
                let directory = open_staging_directory(parent, &name)?;
                return Ok(StagingDirectory { name, directory });
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(BundleError::Io {
                    operation: "create staging directory",
                    source,
                });
            }
        }
    }
    Err(BundleError::StagingExists)
}

#[cfg(not(windows))]
fn open_staging_directory(
    parent: &cap_std::fs::Dir,
    name: &str,
) -> Result<cap_std::fs::Dir, BundleError> {
    parent
        .open_dir_nofollow(name)
        .map_err(|source| BundleError::Io {
            operation: "open staging directory",
            source,
        })
}

#[cfg(windows)]
fn open_staging_directory(
    parent: &cap_std::fs::Dir,
    name: &str,
) -> Result<cap_std::fs::Dir, BundleError> {
    use cap_std::fs::OpenOptionsExt as _;

    // FILE_FLAG_BACKUP_SEMANTICS opens the directory while preserving the
    // default delete-sharing mode needed for an atomic rename by another handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    cap_fs_ext::OpenOptionsFollowExt::follow(&mut options, cap_fs_ext::FollowSymlinks::No);
    parent
        .open_with(name, &options)
        .map(|file| cap_std::fs::Dir::from_std_file(file.into_std()))
        .map_err(|source| BundleError::Io {
            operation: "open staging directory",
            source,
        })
}

fn remove_staging_directory(staging: StagingDirectory) -> Result<(), BundleError> {
    staging
        .directory
        .remove_open_dir_all()
        .map_err(|source| BundleError::Io {
            operation: "remove staging directory",
            source,
        })
}

fn install_staged_bundle(
    parent: &cap_std::fs::Dir,
    parent_path: &Path,
    staging_name: &str,
    destination_name: &OsStr,
) -> Result<(), BundleError> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    let result = rustix::fs::renameat_with(
        parent,
        staging_name,
        parent,
        Path::new(destination_name),
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from);

    #[cfg(windows)]
    // MoveFileExW has no directory-handle-relative form. The parent remains
    // open, the source name is cryptographically unguessable, and the
    // exclusive operation still guarantees that the destination is untouched.
    let result = renamore::rename_exclusive(
        parent_path.join(staging_name),
        parent_path.join(destination_name),
    );

    #[cfg(not(windows))]
    let _ = parent_path;

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )))]
    let result = Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-replace directory installation is unsupported",
    ));

    match result {
        Ok(()) => Ok(()),
        Err(source) => match parent.symlink_metadata(Path::new(destination_name)) {
            Ok(_) => Err(BundleError::DestinationExists),
            Err(inspect) if inspect.kind() == io::ErrorKind::NotFound => Err(BundleError::Io {
                operation: "install result bundle",
                source,
            }),
            Err(source) => Err(BundleError::Io {
                operation: "inspect result destination",
                source,
            }),
        },
    }
}

fn destination_parent(destination: &Path) -> Result<std::path::PathBuf, BundleError> {
    let parent = destination
        .parent()
        .ok_or(BundleError::InvalidDestination)?;
    if parent.as_os_str().is_empty() {
        std::env::current_dir().map_err(|source| BundleError::Io {
            operation: "resolve result parent directory",
            source,
        })
    } else {
        Ok(parent.to_owned())
    }
}

#[cfg(unix)]
fn sync_cap_directory(
    directory: &cap_std::fs::Dir,
    operation: &'static str,
) -> Result<(), BundleError> {
    directory
        .try_clone()
        .and_then(|directory| directory.into_std_file().sync_all())
        .map_err(|source| BundleError::Io { operation, source })
}

#[cfg(not(unix))]
fn sync_cap_directory(
    _directory: &cap_std::fs::Dir,
    _operation: &'static str,
) -> Result<(), BundleError> {
    Ok(())
}

struct ArtifactSet<'a> {
    entries: Vec<(String, Cow<'a, [u8]>)>,
}

impl FixedArtifactSource for ArtifactSet<'_> {
    fn artifact_bytes(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(relative, _)| relative == name)
            .map(|(_, bytes)| bytes.as_ref())
    }
}

impl<'a> ArtifactSet<'a> {
    fn with_capacity(capacity: usize) -> Result<Self, BundleError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(capacity)
            .map_err(|_| BundleError::AllocationFailed)?;
        Ok(Self { entries })
    }

    fn push(
        &mut self,
        relative: String,
        bytes: Cow<'a, [u8]>,
        retained_bytes: &mut u64,
        limits: BundleLimits,
    ) -> Result<(), BundleError> {
        add_bytes(
            retained_bytes,
            bytes.as_ref().len(),
            limits.max_total_bytes,
            "total_bytes",
        )?;
        self.entries.push((relative, bytes));
        Ok(())
    }

    fn sort(&mut self) -> Result<(), BundleError> {
        self.entries
            .sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        if self.entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        Ok(())
    }
}

fn build_artifacts<'a>(
    bundle: &'a ResultBundle,
    limits: BundleLimits,
) -> Result<ArtifactSet<'a>, BundleError> {
    check_count(
        bundle.raw_samples.len(),
        limits.max_raw_samples,
        "raw_sample_count",
    )?;
    check_count(
        bundle.agent_trajectories.len(),
        limits.max_agent_trajectories,
        "agent_trajectory_count",
    )?;
    validate_artifact_map(&bundle.profiles, limits.max_artifacts_per_class)?;
    validate_log_map(&bundle.logs, limits.max_artifacts_per_class)?;
    let artifact_count = FIXED_ARTIFACT_COUNT
        .checked_add(bundle.profiles.len())
        .and_then(|count| count.checked_add(bundle.logs.len()))
        .ok_or(BundleError::LimitExceeded {
            resource: "file_count",
        })?;
    check_count(
        artifact_count
            .checked_add(1)
            .ok_or(BundleError::LimitExceeded {
                resource: "file_count",
            })?,
        limits.max_file_count,
        "file_count",
    )?;
    check_count(
        artifact_count,
        limits.max_checksum_lines,
        "checksum_line_count",
    )?;
    let directory_entries = artifact_count
        .checked_add(3)
        .ok_or(BundleError::LimitExceeded {
            resource: "directory_entry_count",
        })?;
    check_count(
        directory_entries,
        limits.max_directory_entries,
        "directory_entry_count",
    )?;
    let checksum_bytes = checksum_manifest_size(bundle)?;
    if checksum_bytes > limits.max_checksum_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "checksum_bytes",
        });
    }
    let mut retained_bytes = checksum_bytes;
    if retained_bytes > limits.max_total_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "total_bytes",
        });
    }
    let mut profile_bytes = 0_u64;
    for bytes in bundle.profiles.values() {
        check_bytes(bytes.len(), limits.max_artifact_bytes, "artifact_bytes")?;
        add_bytes(
            &mut profile_bytes,
            bytes.len(),
            limits.max_profile_bytes,
            "profile_bytes",
        )?;
    }

    let mut artifacts = ArtifactSet::with_capacity(artifact_count.checked_add(1).ok_or(
        BundleError::LimitExceeded {
            resource: "file_count",
        },
    )?)?;
    macro_rules! push_owned {
        ($name:expr, $serializer:ident, $value:expr) => {
            artifacts.push(
                fallible_string($name)?,
                Cow::Owned(serialize_with_budget(
                    retained_bytes,
                    limits,
                    None,
                    |limit| $serializer($value, limit),
                )?),
                &mut retained_bytes,
                limits,
            )?
        };
    }
    push_owned!(ENVIRONMENT_FILE, json_bytes, &bundle.environment);
    push_owned!(DATASET_MANIFEST_FILE, json_bytes, &bundle.dataset_manifest);
    push_owned!(BUILD_PROVENANCE_FILE, json_bytes, &bundle.build_provenance);
    push_owned!(COMMAND_FILE, json_bytes, &bundle.command);
    push_owned!(RAW_SAMPLES_FILE, json_lines, &bundle.raw_samples);
    push_owned!(SUMMARY_FILE, json_bytes, &bundle.summary);
    push_owned!(COVERAGE_FILE, json_bytes, &bundle.coverage);
    push_owned!(QUALITY_FILE, json_bytes, &bundle.quality);
    push_owned!(
        AGENT_TRAJECTORIES_FILE,
        json_lines,
        &bundle.agent_trajectories
    );

    for (name, bytes) in &bundle.profiles {
        artifacts.push(
            class_artifact_path("profiles/", name)?,
            Cow::Borrowed(bytes),
            &mut retained_bytes,
            limits,
        )?;
    }
    let mut log_bytes = 0_u64;
    for (name, log) in &bundle.logs {
        let remaining_log =
            limits
                .max_log_bytes
                .checked_sub(log_bytes)
                .ok_or(BundleError::LimitExceeded {
                    resource: "log_bytes",
                })?;
        let bytes = serialize_with_budget(
            retained_bytes,
            limits,
            Some((remaining_log, "log_bytes")),
            |limit| json_bytes(log, limit),
        )?;
        decode_operational_log(&bytes, limits)?;
        add_bytes(
            &mut log_bytes,
            bytes.len(),
            limits.max_log_bytes,
            "log_bytes",
        )?;
        artifacts.push(
            class_artifact_path("logs/", name)?,
            Cow::Owned(bytes),
            &mut retained_bytes,
            limits,
        )?;
    }

    artifacts.sort()?;
    validate_fixed_artifacts(&artifacts, limits)?;
    let checksums = checksum_manifest(&artifacts, limits)?;
    if u64::try_from(checksums.len()).ok() != Some(checksum_bytes) {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    let mut checksum_retained = 0_u64;
    artifacts.push(
        fallible_string(CHECKSUMS_FILE)?,
        Cow::Owned(checksums),
        &mut checksum_retained,
        limits,
    )?;
    artifacts.sort()?;
    Ok(artifacts)
}

fn serialize_with_budget(
    retained_bytes: u64,
    limits: BundleLimits,
    additional_limit: Option<(u64, &'static str)>,
    serialize: impl FnOnce(usize) -> Result<Vec<u8>, BundleError>,
) -> Result<Vec<u8>, BundleError> {
    let remaining =
        limits
            .max_total_bytes
            .checked_sub(retained_bytes)
            .ok_or(BundleError::LimitExceeded {
                resource: "total_bytes",
            })?;
    let additional = additional_limit.map_or(u64::MAX, |(limit, _)| limit);
    let cap_bytes = remaining.min(limits.max_artifact_bytes).min(additional);
    if cap_bytes == 0 {
        let resource = if remaining <= additional {
            "total_bytes"
        } else {
            additional_limit
                .map(|(_, resource)| resource)
                .unwrap_or("total_bytes")
        };
        return Err(BundleError::LimitExceeded { resource });
    }
    let cap = usize::try_from(cap_bytes).map_err(|_| BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    match serialize(cap) {
        Err(BundleError::LimitExceeded {
            resource: "serialized_artifact_bytes",
        }) if cap_bytes < limits.max_artifact_bytes => {
            let resource = if remaining <= additional {
                "total_bytes"
            } else {
                additional_limit
                    .map(|(_, resource)| resource)
                    .unwrap_or("total_bytes")
            };
            Err(BundleError::LimitExceeded { resource })
        }
        result => result,
    }
}

fn checksum_manifest(
    artifacts: &ArtifactSet<'_>,
    limits: BundleLimits,
) -> Result<Vec<u8>, BundleError> {
    let checksum_limit =
        usize::try_from(limits.max_checksum_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "checksum_bytes",
        })?;
    let mut checksums = BoundedBuffer::new(checksum_limit);
    for (relative, bytes) in &artifacts.entries {
        let digest = Sha256::digest(bytes.as_ref());
        for byte in digest {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            if checksums
                .write_all(&[HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]])
                .is_err()
            {
                return Err(checksum_buffer_error(&checksums));
            }
        }
        if checksums.write_all(b"  ").is_err()
            || checksums.write_all(relative.as_bytes()).is_err()
            || checksums.write_all(b"\n").is_err()
        {
            return Err(checksum_buffer_error(&checksums));
        }
    }
    Ok(checksums.into_inner())
}

fn checksum_buffer_error(checksums: &BoundedBuffer) -> BundleError {
    if checksums.allocation_failed() {
        BundleError::AllocationFailed
    } else {
        BundleError::LimitExceeded {
            resource: "checksum_bytes",
        }
    }
}

fn checksum_manifest_size(bundle: &ResultBundle) -> Result<u64, BundleError> {
    let mut size = 0_u64;
    for relative in [
        ENVIRONMENT_FILE,
        DATASET_MANIFEST_FILE,
        BUILD_PROVENANCE_FILE,
        COMMAND_FILE,
        RAW_SAMPLES_FILE,
        SUMMARY_FILE,
        COVERAGE_FILE,
        QUALITY_FILE,
        AGENT_TRAJECTORIES_FILE,
    ] {
        add_checksum_line_size(&mut size, relative.len())?;
    }
    for name in bundle.profiles.keys() {
        let length =
            "profiles/"
                .len()
                .checked_add(name.len())
                .ok_or(BundleError::LimitExceeded {
                    resource: "checksum_bytes",
                })?;
        add_checksum_line_size(&mut size, length)?;
    }
    for name in bundle.logs.keys() {
        let length = "logs/"
            .len()
            .checked_add(name.len())
            .ok_or(BundleError::LimitExceeded {
                resource: "checksum_bytes",
            })?;
        add_checksum_line_size(&mut size, length)?;
    }
    Ok(size)
}

fn add_checksum_line_size(total: &mut u64, path_length: usize) -> Result<(), BundleError> {
    let line = path_length
        .checked_add(67)
        .ok_or(BundleError::LimitExceeded {
            resource: "checksum_bytes",
        })?;
    *total = total
        .checked_add(u64::try_from(line).map_err(|_| BundleError::LimitExceeded {
            resource: "checksum_bytes",
        })?)
        .ok_or(BundleError::LimitExceeded {
            resource: "checksum_bytes",
        })?;
    Ok(())
}

fn fallible_string(value: &str) -> Result<String, BundleError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| BundleError::AllocationFailed)?;
    owned.push_str(value);
    Ok(owned)
}

fn class_artifact_path(prefix: &str, name: &str) -> Result<String, BundleError> {
    let length = prefix
        .len()
        .checked_add(name.len())
        .ok_or(BundleError::InvalidArtifactName)?;
    let mut relative = String::new();
    relative
        .try_reserve_exact(length)
        .map_err(|_| BundleError::AllocationFailed)?;
    relative.push_str(prefix);
    relative.push_str(name);
    Ok(relative)
}

fn write_bundle(
    artifacts: &ArtifactSet<'_>,
    staging: &cap_std::fs::Dir,
    fail_after_writes: Option<usize>,
) -> Result<(), BundleError> {
    staging
        .create_dir("profiles")
        .map_err(|source| BundleError::Io {
            operation: "create profiles directory",
            source,
        })?;
    staging
        .create_dir("logs")
        .map_err(|source| BundleError::Io {
            operation: "create logs directory",
            source,
        })?;

    for (write_count, (relative, bytes)) in artifacts.entries.iter().enumerate() {
        if fail_after_writes == Some(write_count) {
            return Err(BundleError::InjectedWriteFailure);
        }
        write_new(staging, relative, bytes.as_ref())?;
    }
    Ok(())
}

pub(crate) fn json_bytes(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, BundleError> {
    let mut bytes = BoundedBuffer::new(limit);
    let result = serde_json::to_writer(&mut bytes, value);
    if bytes.allocation_failed() {
        return Err(BundleError::AllocationFailed);
    }
    if bytes.exceeded() {
        return Err(BundleError::LimitExceeded {
            resource: "serialized_artifact_bytes",
        });
    }
    result.map_err(BundleError::Serialize)?;
    if bytes.write_all(b"\n").is_err() {
        return Err(if bytes.allocation_failed() {
            BundleError::AllocationFailed
        } else {
            BundleError::LimitExceeded {
                resource: "serialized_artifact_bytes",
            }
        });
    }
    Ok(bytes.into_inner())
}

pub(crate) fn json_lines<T: Serialize>(values: &[T], limit: usize) -> Result<Vec<u8>, BundleError> {
    let mut bytes = BoundedBuffer::new(limit);
    for value in values {
        let result = serde_json::to_writer(&mut bytes, value);
        if bytes.allocation_failed() {
            return Err(BundleError::AllocationFailed);
        }
        if bytes.exceeded() {
            return Err(BundleError::LimitExceeded {
                resource: "serialized_artifact_bytes",
            });
        }
        result.map_err(BundleError::Serialize)?;
        if bytes.write_all(b"\n").is_err() {
            return Err(if bytes.allocation_failed() {
                BundleError::AllocationFailed
            } else {
                BundleError::LimitExceeded {
                    resource: "serialized_artifact_bytes",
                }
            });
        }
    }
    Ok(bytes.into_inner())
}

#[derive(Debug)]
struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
    allocation_failed: bool,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
            allocation_failed: false,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn allocation_failed(&self) -> bool {
        self.allocation_failed
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for BoundedBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let new_len = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .filter(|length| *length <= self.limit);
        if new_len.is_none() {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "bounded buffer limit exceeded",
            ));
        }
        if self.bytes.try_reserve(buffer.len()).is_err() {
            self.allocation_failed = true;
            return Err(io::Error::other("bounded buffer allocation failed"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_new(directory: &cap_std::fs::Dir, path: &str, bytes: &[u8]) -> Result<(), BundleError> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    cap_fs_ext::OpenOptionsFollowExt::follow(&mut options, cap_fs_ext::FollowSymlinks::No);
    let mut file = directory
        .open_with(path, &options)
        .map_err(|source| BundleError::Io {
            operation: "create result artifact",
            source,
        })?;
    file.write_all(bytes).map_err(|source| BundleError::Io {
        operation: "write result artifact",
        source,
    })?;
    file.sync_all().map_err(|source| BundleError::Io {
        operation: "sync result artifact",
        source,
    })
}

fn validate_artifact_map(
    artifacts: &BTreeMap<String, Vec<u8>>,
    max_count: usize,
) -> Result<(), BundleError> {
    check_count(artifacts.len(), max_count, "artifact_count")?;
    for name in artifacts.keys() {
        validate_artifact_name(name)?;
    }
    Ok(())
}

fn validate_log_map(
    artifacts: &BTreeMap<String, OperationalLog>,
    max_count: usize,
) -> Result<(), BundleError> {
    check_count(artifacts.len(), max_count, "artifact_count")?;
    for name in artifacts.keys() {
        validate_artifact_name(name)?;
    }
    Ok(())
}

fn validate_artifact_name(name: &str) -> Result<(), BundleError> {
    let normalized_stem = name
        .split_once('.')
        .map_or(name, |(stem, _extension)| stem)
        .to_ascii_uppercase();
    let reserved = matches!(normalized_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || normalized_stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || normalized_stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    let valid_characters = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if name.is_empty()
        || name.len() > 128
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || !valid_characters
        || reserved
        || name.eq_ignore_ascii_case(CHECKSUMS_FILE)
    {
        return Err(BundleError::InvalidArtifactName);
    }
    Ok(())
}

fn parse_checksums(
    text: &str,
    limits: BundleLimits,
) -> Result<BTreeMap<String, String>, BundleError> {
    if text.is_empty() || !text.ends_with('\n') || text.as_bytes().contains(&b'\r') {
        return Err(BundleError::InvalidChecksumManifest);
    }
    let mut checksums = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for (index, line) in text.lines().enumerate() {
        if index >= limits.max_checksum_lines {
            return Err(BundleError::LimitExceeded {
                resource: "checksum_line_count",
            });
        }
        let (checksum, relative) = line
            .split_once("  ")
            .ok_or(BundleError::InvalidChecksumManifest)?;
        let canonical_checksum = checksum.len() == 64
            && checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        if !canonical_checksum
            || !valid_checksum_path(relative)
            || previous.is_some_and(|prior| relative <= prior)
            || checksums
                .insert(relative.to_owned(), checksum.to_owned())
                .is_some()
        {
            return Err(BundleError::InvalidChecksumManifest);
        }
        previous = Some(relative);
    }
    if checksums.is_empty() {
        return Err(BundleError::InvalidChecksumManifest);
    }
    Ok(checksums)
}

fn valid_checksum_path(relative: &str) -> bool {
    if matches!(
        relative,
        ENVIRONMENT_FILE
            | DATASET_MANIFEST_FILE
            | BUILD_PROVENANCE_FILE
            | COMMAND_FILE
            | RAW_SAMPLES_FILE
            | SUMMARY_FILE
            | COVERAGE_FILE
            | QUALITY_FILE
            | AGENT_TRAJECTORIES_FILE
    ) {
        return true;
    }
    for prefix in ["profiles/", "logs/"] {
        if let Some(name) = relative.strip_prefix(prefix) {
            return validate_artifact_name(name).is_ok();
        }
    }
    false
}

struct OpenedArtifact {
    relative: String,
    size: u64,
    file: cap_std::fs::File,
}

struct OpenedArtifacts {
    entries: Vec<OpenedArtifact>,
}

impl OpenedArtifacts {
    fn size(&self, relative: &str) -> Option<u64> {
        self.entries
            .binary_search_by_key(&relative, |entry| entry.relative.as_str())
            .ok()
            .map(|index| self.entries[index].size)
    }

    fn file_mut(&mut self, relative: &str) -> Option<&mut cap_std::fs::File> {
        let index = self
            .entries
            .binary_search_by_key(&relative, |entry| entry.relative.as_str())
            .ok()?;
        Some(&mut self.entries[index].file)
    }

    fn matches_expected_paths(&self, expected: &BTreeMap<String, String>) -> bool {
        self.entries
            .iter()
            .filter(|entry| entry.relative != CHECKSUMS_FILE)
            .map(|entry| entry.relative.as_str())
            .eq(expected.keys().map(String::as_str))
    }
}

struct PendingDirectory {
    directory: cap_std::fs::Dir,
    prefix: String,
    depth: usize,
}

fn collect_files<F>(
    root: cap_std::fs::Dir,
    limits: BundleLimits,
    mut before_file_open: F,
) -> Result<OpenedArtifacts, BundleError>
where
    F: FnMut(&str),
{
    let mut pending = Vec::new();
    pending
        .try_reserve_exact(limits.max_depth.saturating_add(1))
        .map_err(|_| BundleError::AllocationFailed)?;
    pending.push(PendingDirectory {
        directory: root,
        prefix: String::new(),
        depth: 0,
    });
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(limits.max_file_count)
        .map_err(|_| BundleError::AllocationFailed)?;
    let mut visited_entries = 0_usize;
    let mut visited_files = 0_usize;
    let mut total_bytes = 0_u64;
    let mut unexpected_directory = false;
    while let Some(current) = pending.pop() {
        if current.depth > limits.max_depth {
            return Err(BundleError::LimitExceeded {
                resource: "directory_depth",
            });
        }
        let directory_entries = current
            .directory
            .entries()
            .map_err(|source| BundleError::Io {
                operation: "enumerate result bundle",
                source,
            })?;
        for entry in directory_entries {
            let entry = entry.map_err(|source| BundleError::Io {
                operation: "read result directory entry",
                source,
            })?;
            visited_entries = visited_entries
                .checked_add(1)
                .ok_or(BundleError::LimitExceeded {
                    resource: "directory_entry_count",
                })?;
            check_count(
                visited_entries,
                limits.max_directory_entries,
                "directory_entry_count",
            )?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(BundleError::InvalidChecksumManifest)?;
            let relative = join_artifact_path(&current.prefix, name, limits.max_string_bytes)?;
            let file_type = entry.file_type().map_err(|source| BundleError::Io {
                operation: "inspect result artifact",
                source,
            })?;
            if file_type.is_symlink() {
                return Err(BundleError::UnsupportedArtifactType);
            }
            if file_type.is_dir() {
                let next_depth =
                    current
                        .depth
                        .checked_add(1)
                        .ok_or(BundleError::LimitExceeded {
                            resource: "directory_depth",
                        })?;
                if next_depth > limits.max_depth {
                    return Err(BundleError::LimitExceeded {
                        resource: "directory_depth",
                    });
                }
                if current.depth != 0 || !matches!(name, "profiles" | "logs") {
                    unexpected_directory = true;
                }
                let directory = current
                    .directory
                    .open_dir_nofollow(name)
                    .map_err(|source| BundleError::Io {
                        operation: "open result directory",
                        source,
                    })?;
                let metadata = directory.dir_metadata().map_err(|source| BundleError::Io {
                    operation: "inspect result directory",
                    source,
                })?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(BundleError::UnsupportedArtifactType);
                }
                if pending.len() == pending.capacity() {
                    pending
                        .try_reserve(1)
                        .map_err(|_| BundleError::AllocationFailed)?;
                }
                pending.push(PendingDirectory {
                    directory,
                    prefix: relative,
                    depth: next_depth,
                });
            } else if file_type.is_file() {
                before_file_open(&relative);
                let mut options = cap_std::fs::OpenOptions::new();
                options.read(true);
                cap_fs_ext::OpenOptionsFollowExt::follow(
                    &mut options,
                    cap_fs_ext::FollowSymlinks::No,
                );
                let file = current
                    .directory
                    .open_with(name, &options)
                    .map_err(|source| BundleError::Io {
                        operation: "open result artifact",
                        source,
                    })?;
                let metadata = file.metadata().map_err(|source| BundleError::Io {
                    operation: "inspect result artifact",
                    source,
                })?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(BundleError::UnsupportedArtifactType);
                }
                check_artifact_size(metadata.len(), limits.max_artifact_bytes)?;
                visited_files = visited_files
                    .checked_add(1)
                    .ok_or(BundleError::LimitExceeded {
                        resource: "file_count",
                    })?;
                check_count(visited_files, limits.max_file_count, "file_count")?;
                total_bytes =
                    total_bytes
                        .checked_add(metadata.len())
                        .ok_or(BundleError::LimitExceeded {
                            resource: "total_bytes",
                        })?;
                if total_bytes > limits.max_total_bytes {
                    return Err(BundleError::LimitExceeded {
                        resource: "total_bytes",
                    });
                }
                entries.push(OpenedArtifact {
                    relative,
                    size: metadata.len(),
                    file,
                });
            } else {
                return Err(BundleError::UnsupportedArtifactType);
            }
        }
    }
    if unexpected_directory {
        return Err(BundleError::ArtifactSetMismatch);
    }
    entries.sort_unstable_by(|left, right| left.relative.cmp(&right.relative));
    if entries
        .windows(2)
        .any(|pair| pair[0].relative == pair[1].relative)
    {
        return Err(BundleError::InvalidChecksumManifest);
    }
    Ok(OpenedArtifacts { entries })
}

fn join_artifact_path(prefix: &str, name: &str, limit: usize) -> Result<String, BundleError> {
    let separator = usize::from(!prefix.is_empty());
    let length = prefix
        .len()
        .checked_add(separator)
        .and_then(|length| length.checked_add(name.len()))
        .filter(|length| *length <= limit)
        .ok_or(BundleError::InvalidChecksumManifest)?;
    let mut relative = String::new();
    relative
        .try_reserve_exact(length)
        .map_err(|_| BundleError::AllocationFailed)?;
    if !prefix.is_empty() {
        relative.push_str(prefix);
        relative.push('/');
    }
    relative.push_str(name);
    Ok(relative)
}

fn check_artifact_size(size: u64, limit: u64) -> Result<(), BundleError> {
    if size > limit {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
    }
    Ok(())
}

fn read_bounded(
    file: &mut cap_std::fs::File,
    limit: u64,
    operation: &'static str,
) -> Result<Vec<u8>, BundleError> {
    let metadata = file
        .metadata()
        .map_err(|source| BundleError::Io { operation, source })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(BundleError::UnsupportedArtifactType);
    }
    check_artifact_size(metadata.len(), limit)?;
    let capacity = usize::try_from(metadata.len()).map_err(|_| BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    let read_limit = limit.checked_add(1).ok_or(BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| BundleError::AllocationFailed)?;
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| BundleError::Io { operation, source })?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
    }
    if u64::try_from(bytes.len()).ok() != Some(metadata.len()) {
        return Err(BundleError::ChecksumMismatch);
    }
    Ok(bytes)
}

fn check_count(count: usize, limit: usize, resource: &'static str) -> Result<(), BundleError> {
    if count > limit {
        return Err(BundleError::LimitExceeded { resource });
    }
    Ok(())
}

fn check_bytes(length: usize, limit: u64, resource: &'static str) -> Result<(), BundleError> {
    let length = u64::try_from(length).map_err(|_| BundleError::LimitExceeded { resource })?;
    if length > limit {
        return Err(BundleError::LimitExceeded { resource });
    }
    Ok(())
}

fn add_bytes(
    total: &mut u64,
    length: usize,
    limit: u64,
    resource: &'static str,
) -> Result<(), BundleError> {
    let length = u64::try_from(length).map_err(|_| BundleError::LimitExceeded { resource })?;
    *total = total
        .checked_add(length)
        .ok_or(BundleError::LimitExceeded { resource })?;
    if *total > limit {
        return Err(BundleError::LimitExceeded { resource });
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Immutable result publication or verification failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BundleError {
    /// A supplied limit is zero, inconsistent, or above a hard ceiling.
    #[error("result bundle limits are invalid")]
    InvalidLimits,
    /// A bounded resource exceeded its checked ceiling.
    #[error("result bundle limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// A bounded in-memory reservation could not be satisfied.
    #[error("result bundle allocation failed")]
    AllocationFailed,
    /// Secure randomness for an operation-owned staging name was unavailable.
    #[error("result staging randomness failed")]
    Randomness(#[source] getrandom::Error),
    /// The final destination already exists.
    #[error("result destination already exists")]
    DestinationExists,
    /// The operation-owned staging directory already exists.
    #[error("result staging directory already exists")]
    StagingExists,
    /// The destination cannot be represented safely.
    #[error("result destination is invalid")]
    InvalidDestination,
    /// A profile or log name is not one canonical safe path component.
    #[error("result artifact name is invalid")]
    InvalidArtifactName,
    /// A log is not valid source-free UTF-8 operational text.
    #[error("result log is invalid")]
    InvalidLog,
    /// JSON serialization failed.
    #[error("result serialization failed")]
    Serialize(#[source] serde_json::Error),
    /// A bounded filesystem operation failed.
    #[error("{operation} failed")]
    Io {
        /// Source-free operation label.
        operation: &'static str,
        /// Underlying I/O failure without a stored host path.
        #[source]
        source: io::Error,
    },
    /// The checksum manifest is malformed or non-canonical.
    #[error("checksum manifest is invalid")]
    InvalidChecksumManifest,
    /// The bundle contains missing or unexpected artifacts.
    #[error("result artifact set does not match checksum manifest")]
    ArtifactSetMismatch,
    /// One artifact failed checksum verification.
    #[error("result artifact checksum mismatch")]
    ChecksumMismatch,
    /// A fixed artifact is not strict canonical JSON for its schema.
    #[error("result artifact encoding is invalid")]
    InvalidArtifactEncoding,
    /// The bundle schema is recognized but unsupported by this verifier.
    #[error("result bundle schema version is unsupported")]
    UnsupportedSchemaVersion,
    /// The quality rubric is incompatible with the current bundle schema.
    #[error("result bundle quality rubric is unsupported")]
    UnsupportedRubricVersion,
    /// Fixed artifacts contradict one another or their recorded run policy.
    #[error("result artifact invariants are invalid")]
    ArtifactInvariantViolation,
    /// The verifier encountered a link or special file.
    #[error("result bundle contains an unsupported artifact type")]
    UnsupportedArtifactType,
    /// Test-only failure after a bounded number of writes.
    #[error("injected result write failure")]
    InjectedWriteFailure,
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    use super::*;
    use crate::{
        Availability, BenchmarkCommand, BuildProvenance, CoverageEvidence, DatasetManifest,
        EnvironmentEvidence, EvidenceValue, QualityEvidence, ResultSummary,
    };

    fn fixture() -> ResultBundle {
        ResultBundle {
            environment: EnvironmentEvidence {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                cpu_model: EvidenceValue::unavailable("not_sampled"),
                cpu_topology: EvidenceValue::unavailable("not_sampled"),
                ram_bytes: EvidenceValue::unavailable("not_sampled"),
                operating_system: EvidenceValue::observed("test".to_owned()),
                kernel: EvidenceValue::unavailable("not_sampled"),
                filesystem: EvidenceValue::unavailable("not_sampled"),
                storage_device: EvidenceValue::unavailable("not_sampled"),
                power_mode: EvidenceValue::unavailable("not_sampled"),
                container_limits: EvidenceValue::unavailable("not_sampled"),
                compiler: EvidenceValue::observed("rustc-test".to_owned()),
                binary_sha256: EvidenceValue::observed("00".repeat(32)),
                feature_profile: "test".to_owned(),
                sqlite: EvidenceValue::unavailable("not_in_scope"),
                adapter_versions: EvidenceValue::unavailable("not_sampled"),
                grammar_versions: EvidenceValue::unavailable("not_sampled"),
                grammar_source_package_checksums: EvidenceValue::unavailable("not_sampled"),
                grammar_hashes: EvidenceValue::unavailable("not_sampled"),
                locale: EvidenceValue::unavailable("not_sampled"),
                background_process_policy: EvidenceValue::unavailable("not_sampled"),
                clock_source: EvidenceValue::observed("std_instant".to_owned()),
                process_tree_accounting: Availability::Unavailable {
                    reason_code: "no_platform_sampler".to_owned(),
                },
            },
            dataset_manifest: DatasetManifest {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                dataset_id: "fixture".to_owned(),
                revision: format!("sha256:{}", sha256_hex(&[])),
                scope_rule: "listed_entries".to_owned(),
                loc_counting_rule: "physical_newlines".to_owned(),
                entries: Vec::new(),
            },
            build_provenance: BuildProvenance {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                source_revision: "00".repeat(20),
                binary_revision: format!("sha256:{}", "00".repeat(32)),
                build_profile: "test".to_owned(),
                features: Vec::new(),
                target: "test-target".to_owned(),
            },
            command: BenchmarkCommand {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                subcommand: "m05-parser".to_owned(),
                arguments: Vec::new(),
                seed: 7,
                warmup_rounds: 1,
                trial_rounds: 1,
                timeout_ms: 100,
            },
            raw_samples: Vec::new(),
            summary: ResultSummary {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                benchmark_id: "BENCH-PARSE-001".to_owned(),
                semantic_eligibility: Availability::Failed {
                    reason_code: "no_measured_samples".to_owned(),
                },
                families: BTreeMap::new(),
                failed_samples: 0,
                timed_out_samples: 0,
                cancelled_samples: 0,
                confidence_intervals: Availability::Unavailable {
                    reason_code: "bootstrap_confidence_interval_not_integrated".to_owned(),
                },
            },
            coverage: CoverageEvidence {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                attempted_entries: 0,
                committed_entries: 0,
                skipped: BTreeMap::new(),
                parser_status: BTreeMap::new(),
            },
            quality: QualityEvidence {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                rubric_id: crate::SEMANTIC_QUALITY_RUBRIC_ID.to_owned(),
                semantic_eligibility: Availability::Failed {
                    reason_code: "no_measured_samples".to_owned(),
                },
                precision_ppm: EvidenceValue::unavailable("not_measured"),
                recall_ppm: EvidenceValue::unavailable("not_measured"),
                expected_calibration_error_ppm: EvidenceValue::unavailable("not_measured"),
                unsupported_cases: BTreeMap::new(),
            },
            agent_trajectories: Vec::new(),
            profiles: BTreeMap::new(),
            logs: BTreeMap::new(),
        }
    }

    fn scheduled_fixture() -> ResultBundle {
        let mut bundle = fixture();
        bundle.dataset_manifest.entries = vec![
            crate::DatasetEntry {
                id: "entry-a".to_owned(),
                grammar_family: "rust".to_owned(),
                language: "rust".to_owned(),
                relative_path: "a.rs".to_owned(),
                source_sha256: "11".repeat(32),
                source_bytes: 10,
                physical_lines: 1,
                generated: false,
            },
            crate::DatasetEntry {
                id: "entry-b".to_owned(),
                grammar_family: "python".to_owned(),
                language: "python".to_owned(),
                relative_path: "b.py".to_owned(),
                source_sha256: "22".repeat(32),
                source_bytes: 20,
                physical_lines: 2,
                generated: false,
            },
        ];
        let mut revision = Sha256::new();
        for entry in &bundle.dataset_manifest.entries {
            revision.update(
                u64::try_from(entry.id.len())
                    .expect("test ID length fits")
                    .to_be_bytes(),
            );
            revision.update(entry.id.as_bytes());
            revision.update(
                u64::try_from(entry.source_sha256.len())
                    .expect("test digest length fits")
                    .to_be_bytes(),
            );
            revision.update(entry.source_sha256.as_bytes());
        }
        let mut revision_hex = String::from("sha256:");
        for byte in revision.finalize() {
            use std::fmt::Write as _;
            write!(revision_hex, "{byte:02x}").expect("writing to a string succeeds");
        }
        bundle.dataset_manifest.revision = revision_hex;
        bundle.command.seed = 17;
        bundle.command.warmup_rounds = 1;
        bundle.command.trial_rounds = 2;
        let schedule = crate::parser::build_schedule(
            bundle.dataset_manifest.entries.len(),
            bundle.command.warmup_rounds,
            bundle.command.trial_rounds,
            bundle.command.seed,
            BundleLimits::default().max_raw_samples,
        )
        .expect("test schedule is valid");
        bundle.raw_samples = schedule
            .iter()
            .enumerate()
            .map(|(ordinal, scheduled)| {
                let entry = &bundle.dataset_manifest.entries[scheduled.input_index];
                RawSample {
                    schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                    ordinal: u64::try_from(ordinal).expect("test ordinal fits"),
                    phase: scheduled.phase.as_str().to_owned(),
                    dataset_entry_id: entry.id.clone(),
                    grammar_family: entry.grammar_family.clone(),
                    elapsed_ns: u64::try_from(ordinal + 1).expect("test elapsed time fits") * 100,
                    source_bytes: entry.source_bytes,
                    physical_lines: entry.physical_lines,
                    syntax_nodes: 4,
                    syntax_facts: 2,
                    semantic_facts: EvidenceValue::unavailable(
                        "semantic_extraction_not_integrated",
                    ),
                    process_tree_cpu_ns: EvidenceValue::unavailable("not_measured"),
                    process_tree_peak_rss_bytes: EvidenceValue::unavailable("not_measured"),
                    outcome: crate::SampleOutcome::Succeeded,
                    is_outlier: false,
                }
            })
            .collect();
        crate::parser::mark_outliers(&mut bundle.raw_samples)
            .expect("test outliers are representable");
        let semantic_eligibility = crate::parser::semantic_fact_eligibility(&bundle.raw_samples);
        bundle.summary =
            crate::parser::summarize(&bundle.raw_samples, semantic_eligibility.clone())
                .expect("test summary is representable");
        bundle.coverage = CoverageEvidence {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            attempted_entries: 2,
            committed_entries: 2,
            skipped: BTreeMap::new(),
            parser_status: BTreeMap::from([
                ("entry-a".to_owned(), "succeeded".to_owned()),
                ("entry-b".to_owned(), "succeeded".to_owned()),
            ]),
        };
        bundle.quality.semantic_eligibility = semantic_eligibility;
        bundle
    }

    fn constrained_limits() -> BundleLimits {
        BundleLimits {
            max_raw_samples: 4,
            max_agent_trajectories: 4,
            max_artifacts_per_class: 4,
            max_artifact_bytes: 64 * 1024,
            max_profile_bytes: 64 * 1024,
            max_log_bytes: 64 * 1024,
            max_total_bytes: 512 * 1024,
            max_checksum_lines: 32,
            max_checksum_bytes: 8 * 1024,
            max_depth: 3,
            max_file_count: 32,
            max_directory_entries: 64,
            max_input_bytes: 64 * 1024,
            max_manifest_entries: 4,
            max_command_arguments: 4,
            max_string_bytes: 256,
            max_snapshot_bytes: 64 * 1024,
            max_dataset_source_bytes: 256 * 1024,
        }
    }

    fn rewrite_artifact_and_checksum(destination: &Path, artifact: &str, bytes: &[u8]) {
        fs::write(destination.join(artifact), bytes).expect("tampered artifact is written");
        let checksums = fs::read_to_string(destination.join(CHECKSUMS_FILE))
            .expect("checksum manifest is readable");
        let mut updated = String::new();
        for line in checksums.lines() {
            let (_, relative) = line
                .split_once("  ")
                .expect("fixture checksum line is canonical");
            if relative == artifact {
                use std::fmt::Write as _;
                writeln!(updated, "{}  {artifact}", sha256_hex(bytes))
                    .expect("writing to a string succeeds");
            } else {
                updated.push_str(line);
                updated.push('\n');
            }
        }
        fs::write(destination.join(CHECKSUMS_FILE), updated)
            .expect("updated checksum manifest is written");
    }

    #[test]
    fn equivalent_bundles_publish_identical_artifacts() {
        let first = tempfile::tempdir().expect("temporary root is available");
        let second = tempfile::tempdir().expect("temporary root is available");
        let first_result = first.path().join("result");
        let second_result = second.path().join("result");

        publish_bundle(&fixture(), &first_result).expect("first bundle publishes");
        publish_bundle(&fixture(), &second_result).expect("second bundle publishes");

        let first_checksums = fs::read(first_result.join(CHECKSUMS_FILE)).expect("checksums exist");
        let second_checksums =
            fs::read(second_result.join(CHECKSUMS_FILE)).expect("checksums exist");
        assert_eq!(first_checksums, second_checksums);
        verify_bundle(&first_result).expect("first bundle verifies");
        verify_bundle(&second_result).expect("second bundle verifies");
    }

    #[test]
    fn relative_destination_publishes_and_verifies_from_current_directory() {
        static CURRENT_DIRECTORY: Mutex<()> = Mutex::new(());
        let _lock = CURRENT_DIRECTORY
            .lock()
            .expect("current-directory test lock is available");
        let original = std::env::current_dir().expect("current directory is available");
        let temporary = tempfile::tempdir().expect("temporary root is available");
        std::env::set_current_dir(temporary.path()).expect("temporary root becomes current");
        let _restore = CurrentDirectoryGuard(original);

        let destination = Path::new("result");
        publish_bundle(&fixture(), destination).expect("relative bundle publishes");
        verify_bundle(destination).expect("relative bundle verifies");
    }

    #[test]
    fn publication_never_overwrites_existing_evidence() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("initial bundle publishes");

        let error = publish_bundle(&fixture(), &destination).expect_err("overwrite is rejected");

        assert!(matches!(error, BundleError::DestinationExists));
    }

    #[test]
    fn concurrent_publishers_have_one_winner_and_leave_no_partial_results() {
        const PUBLISHER_COUNT: usize = 8;

        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let barrier = Arc::new(Barrier::new(PUBLISHER_COUNT));
        let mut publishers = Vec::new();
        for index in 0..PUBLISHER_COUNT {
            let mut bundle = fixture();
            bundle.environment.operating_system =
                EvidenceValue::observed(format!("publisher-{index}"));
            let destination = destination.clone();
            let barrier = Arc::clone(&barrier);
            publishers.push(thread::spawn(move || {
                publish_bundle_with_control(
                    &bundle,
                    &destination,
                    BundleLimits::default(),
                    None,
                    || {
                        barrier.wait();
                    },
                )
            }));
        }

        let mut successes = 0;
        for publisher in publishers {
            match publisher.join().expect("publisher thread does not panic") {
                Ok(()) => successes += 1,
                Err(BundleError::DestinationExists) => {}
                Err(error) => panic!("unexpected publication error: {error:?}"),
            }
        }

        assert_eq!(successes, 1);
        verify_bundle(&destination).expect("winning bundle verifies");
        let remaining = fs::read_dir(temporary.path())
            .expect("temporary root is readable")
            .count();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn noncooperating_destination_created_before_atomic_claim_is_not_replaced() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");

        let error = publish_bundle_with_control(
            &fixture(),
            &destination,
            BundleLimits::default(),
            None,
            || {
                fs::create_dir(&destination).expect("racing destination is created");
                fs::write(destination.join("sentinel"), b"not rootlight evidence")
                    .expect("racing destination content is written");
            },
        )
        .expect_err("racing destination is rejected");

        assert!(matches!(error, BundleError::DestinationExists));
        assert_eq!(
            fs::read(destination.join("sentinel")).expect("racing content remains readable"),
            b"not rootlight evidence"
        );
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary root is readable")
                .count(),
            1
        );
    }

    #[test]
    fn preinstall_crash_orphan_does_not_block_publication() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let orphan = temporary.path().join(".rootlight-result-partial-abandoned");
        fs::create_dir(&orphan).expect("orphan staging directory is created");
        fs::write(orphan.join("partial"), b"incomplete").expect("orphan content is written");

        publish_bundle(&fixture(), &destination).expect("new publication succeeds");

        verify_bundle(&destination).expect("new publication verifies");
        assert_eq!(
            fs::read(orphan.join("partial")).expect("orphan is not deleted"),
            b"incomplete"
        );
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary root is readable")
                .count(),
            2
        );
    }

    #[test]
    fn partial_destination_is_never_recovered_or_deleted() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        fs::create_dir(&destination).expect("partial destination is created");
        fs::write(destination.join("partial"), b"incomplete").expect("partial artifact is written");

        let error =
            publish_bundle(&fixture(), &destination).expect_err("partial destination is preserved");

        assert!(matches!(error, BundleError::DestinationExists));
        assert_eq!(
            fs::read(destination.join("partial")).expect("partial artifact remains"),
            b"incomplete"
        );
    }

    #[test]
    fn obsolete_publication_marker_is_an_unexpected_artifact() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        let marker = destination.join(".rootlight-publication");
        fs::write(&marker, []).expect("obsolete marker file is written");

        let error = verify_bundle(&destination).expect_err("marker file is rejected");
        assert!(matches!(error, BundleError::ArtifactSetMismatch));

        fs::remove_file(&marker).expect("obsolete marker file is removed");
        fs::create_dir(&marker).expect("obsolete marker directory is created");
        let error = verify_bundle(&destination).expect_err("marker directory is rejected");
        assert!(matches!(error, BundleError::ArtifactSetMismatch));
    }

    #[test]
    fn verification_detects_tampering_without_echoing_artifact_names() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        fs::write(destination.join(SUMMARY_FILE), b"{}\n").expect("fixture is tampered");

        let error = verify_bundle(&destination).expect_err("tampering is rejected");

        assert!(matches!(error, BundleError::ChecksumMismatch));
        assert_eq!(error.to_string(), "result artifact checksum mismatch");
    }

    #[test]
    fn verifier_keeps_using_the_opened_root_after_a_path_swap() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let displaced = temporary.path().join("displaced");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        let swapped = Cell::new(false);
        let blocked = Cell::new(false);

        verify_bundle_with_control(
            &destination,
            BundleLimits::default(),
            || match fs::rename(&destination, &displaced) {
                Ok(()) => {
                    fs::create_dir(&destination).expect("replacement root is created");
                    swapped.set(true);
                }
                #[cfg(windows)]
                Err(_) => blocked.set(true),
                #[cfg(not(windows))]
                Err(error) => panic!("opened root can be renamed: {error}"),
            },
            |_| {},
            || {},
        )
        .expect("verification stays bound to the original root handle");

        assert!(swapped.get() || blocked.get());
        if swapped.get() {
            assert_eq!(
                fs::read_dir(&destination)
                    .expect("replacement root is readable")
                    .count(),
                0
            );
        }
    }

    #[test]
    fn verifier_keeps_using_opened_files_after_path_replacement() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let summary = destination.join(SUMMARY_FILE);
        let displaced = destination.join("summary.displaced");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        let swapped = Cell::new(false);
        let blocked = Cell::new(false);

        verify_bundle_with_control(
            &destination,
            BundleLimits::default(),
            || {},
            |_| {},
            || match fs::rename(&summary, &displaced) {
                Ok(()) => {
                    fs::write(&summary, b"replacement").expect("replacement summary is written");
                    swapped.set(true);
                }
                #[cfg(windows)]
                Err(_) => blocked.set(true),
                #[cfg(not(windows))]
                Err(error) => panic!("opened summary can be renamed: {error}"),
            },
        )
        .expect("verification stays bound to opened artifact handles");

        assert!(swapped.get() || blocked.get());
    }

    #[cfg(unix)]
    #[test]
    fn verifier_never_follows_a_file_swapped_to_a_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let outside = temporary.path().join("outside");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        fs::write(&outside, b"outside").expect("outside content is written");
        let swapped = Cell::new(false);

        let error = verify_bundle_with_control(
            &destination,
            BundleLimits::default(),
            || {},
            |relative| {
                if relative == SUMMARY_FILE {
                    fs::remove_file(destination.join(SUMMARY_FILE))
                        .expect("summary is removed before open");
                    symlink(&outside, destination.join(SUMMARY_FILE))
                        .expect("summary symlink is installed");
                    swapped.set(true);
                }
            },
            || {},
        )
        .expect_err("a swapped symlink is rejected");

        assert!(swapped.get());
        assert!(matches!(
            error,
            BundleError::Io {
                operation: "open result artifact",
                ..
            } | BundleError::UnsupportedArtifactType
        ));
    }

    #[test]
    fn every_fixed_artifact_is_strictly_decoded_after_checksum_verification() {
        for artifact in [
            ENVIRONMENT_FILE,
            DATASET_MANIFEST_FILE,
            BUILD_PROVENANCE_FILE,
            COMMAND_FILE,
            RAW_SAMPLES_FILE,
            SUMMARY_FILE,
            COVERAGE_FILE,
            QUALITY_FILE,
            AGENT_TRAJECTORIES_FILE,
        ] {
            let temporary = tempfile::tempdir().expect("temporary root is available");
            let destination = temporary.path().join("result");
            publish_bundle(&fixture(), &destination).expect("bundle publishes");
            rewrite_artifact_and_checksum(&destination, artifact, b"{}\n");

            let error =
                verify_bundle(&destination).expect_err("invalid fixed artifact is rejected");

            if artifact == RAW_SAMPLES_FILE {
                assert!(
                    matches!(
                        error,
                        BundleError::LimitExceeded {
                            resource: "raw_sample_count"
                        }
                    ),
                    "{artifact} returned {error:?}"
                );
            } else {
                assert!(
                    matches!(error, BundleError::InvalidArtifactEncoding),
                    "{artifact} returned {error:?}"
                );
            }
        }
    }

    #[test]
    fn verifier_preflights_profile_and_log_class_limits() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let mut bundle = fixture();
        bundle.profiles.insert("first.pb".to_owned(), vec![0; 40]);
        bundle.profiles.insert("second.pb".to_owned(), vec![0; 40]);
        bundle
            .logs
            .insert("first.json".to_owned(), large_operational_log(0, 2));
        bundle
            .logs
            .insert("second.json".to_owned(), large_operational_log(2, 2));
        publish_bundle(&bundle, &destination).expect("bundle publishes");

        let count_limits = BundleLimits {
            max_artifacts_per_class: 1,
            ..constrained_limits()
        };
        assert!(matches!(
            verify_bundle_with_limits(&destination, count_limits),
            Err(BundleError::LimitExceeded {
                resource: "artifact_count"
            })
        ));

        let profile_limits = BundleLimits {
            max_profile_bytes: 79,
            ..constrained_limits()
        };
        assert!(matches!(
            verify_bundle_with_limits(&destination, profile_limits),
            Err(BundleError::LimitExceeded {
                resource: "profile_bytes"
            })
        ));

        let log_limits = BundleLimits {
            max_log_bytes: 1,
            ..constrained_limits()
        };
        assert!(matches!(
            verify_bundle_with_limits(&destination, log_limits),
            Err(BundleError::LimitExceeded {
                resource: "log_bytes"
            })
        ));
    }

    #[test]
    fn verifier_strictly_decodes_operational_logs() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let mut bundle = fixture();
        bundle
            .logs
            .insert("run.json".to_owned(), large_operational_log(0, 1));
        publish_bundle(&bundle, &destination).expect("bundle publishes");
        let bytes = fs::read(destination.join("logs").join("run.json"))
            .expect("operational log is readable");
        let mut text = String::from_utf8(bytes).expect("operational log is UTF-8");
        text = text.replace(
            "\"elapsed_ns\":1",
            "\"elapsed_ns\":1,\"host_path\":\"C:/source\"",
        );
        rewrite_artifact_and_checksum(&destination, "logs/run.json", text.as_bytes());

        let error = verify_bundle(&destination).expect_err("unknown log payload is rejected");

        assert!(matches!(error, BundleError::InvalidLog));
    }

    #[test]
    fn verifier_requires_canonical_json_and_jsonl_bytes() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let json_destination = temporary.path().join("json-result");
        publish_bundle(&fixture(), &json_destination).expect("bundle publishes");
        let environment =
            fs::read(json_destination.join(ENVIRONMENT_FILE)).expect("environment is readable");
        let mut noncanonical = Vec::with_capacity(environment.len() + 1);
        noncanonical.push(b' ');
        noncanonical.extend_from_slice(&environment);
        rewrite_artifact_and_checksum(&json_destination, ENVIRONMENT_FILE, &noncanonical);
        assert!(matches!(
            verify_bundle(&json_destination),
            Err(BundleError::InvalidArtifactEncoding)
        ));

        let jsonl_destination = temporary.path().join("jsonl-result");
        let mut bundle = fixture();
        bundle.agent_trajectories.push(AgentTrajectory {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            task_id: "task".to_owned(),
            eligibility: Availability::Unavailable {
                reason_code: "not_measured".to_owned(),
            },
            tool_calls: Vec::new(),
            total_tokens: EvidenceValue::unavailable("not_measured"),
        });
        publish_bundle(&bundle, &jsonl_destination).expect("trajectory bundle publishes");
        let trajectory = fs::read(jsonl_destination.join(AGENT_TRAJECTORIES_FILE))
            .expect("trajectory is readable");
        let mut noncanonical = Vec::with_capacity(trajectory.len() + 1);
        noncanonical.push(b' ');
        noncanonical.extend_from_slice(&trajectory);
        rewrite_artifact_and_checksum(&jsonl_destination, AGENT_TRAJECTORIES_FILE, &noncanonical);
        assert!(matches!(
            verify_bundle(&jsonl_destination),
            Err(BundleError::InvalidArtifactEncoding)
        ));
    }

    #[test]
    fn version_one_bundle_is_explicitly_unsupported() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("legacy-result");
        publish_bundle(&fixture(), &destination).expect("current bundle publishes");
        let manifest =
            fs::read(destination.join(DATASET_MANIFEST_FILE)).expect("manifest is readable");
        let legacy = String::from_utf8(manifest)
            .expect("manifest is UTF-8")
            .replace("\"schema_version\":\"2.0\"", "\"schema_version\":\"1.0\"");
        rewrite_artifact_and_checksum(&destination, DATASET_MANIFEST_FILE, legacy.as_bytes());

        let error = verify_bundle(&destination).expect_err("legacy bundle is rejected explicitly");

        assert!(matches!(error, BundleError::UnsupportedSchemaVersion));
    }

    #[test]
    fn version_two_bundle_rejects_the_version_one_quality_rubric() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let mut bundle = fixture();
        bundle.quality.rubric_id = "m05-parser-semantic-eligibility-1.0".to_owned();

        let error = publish_bundle(&bundle, &temporary.path().join("result"))
            .expect_err("legacy rubric is incompatible with schema version two");

        assert!(matches!(error, BundleError::UnsupportedRubricVersion));
    }

    #[test]
    fn publication_rejects_schema_revision_count_and_availability_contradictions() {
        let temporary = tempfile::tempdir().expect("temporary root is available");

        let mut invalid_schema = fixture();
        invalid_schema.environment.schema_version = "3.0".to_owned();
        assert!(matches!(
            publish_bundle(&invalid_schema, &temporary.path().join("schema")),
            Err(BundleError::UnsupportedSchemaVersion)
        ));

        let mut invalid_dataset_revision = fixture();
        invalid_dataset_revision.dataset_manifest.revision = format!("sha256:{}", "11".repeat(32));
        assert!(matches!(
            publish_bundle(
                &invalid_dataset_revision,
                &temporary.path().join("dataset-revision")
            ),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut invalid_binary_revision = fixture();
        invalid_binary_revision.build_provenance.binary_revision =
            format!("sha256:{}", "11".repeat(32));
        assert!(matches!(
            publish_bundle(
                &invalid_binary_revision,
                &temporary.path().join("binary-revision")
            ),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut invalid_count = fixture();
        invalid_count.summary.failed_samples = 1;
        assert!(matches!(
            publish_bundle(&invalid_count, &temporary.path().join("count")),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut invalid_availability = fixture();
        invalid_availability.summary.semantic_eligibility = Availability::Available;
        assert!(matches!(
            publish_bundle(
                &invalid_availability,
                &temporary.path().join("availability")
            ),
            Err(BundleError::ArtifactInvariantViolation)
        ));
    }

    #[test]
    fn publication_reconstructs_seeded_schedule_order() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let bundle = scheduled_fixture();
        publish_bundle(&bundle, &temporary.path().join("valid"))
            .expect("scheduled fixture publishes");
        let mut reordered = bundle;
        reordered.raw_samples.swap(0, 1);
        reordered.raw_samples[0].ordinal = 0;
        reordered.raw_samples[1].ordinal = 1;

        let error = publish_bundle(&reordered, &temporary.path().join("reordered"))
            .expect_err("reordered samples are rejected");

        assert!(matches!(error, BundleError::ArtifactInvariantViolation));
    }

    #[test]
    fn publication_recomputes_distributions_rates_outliers_and_confidence() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let bundle = scheduled_fixture();

        let mut percentile = bundle.clone();
        let distribution = percentile
            .summary
            .families
            .values_mut()
            .next()
            .expect("fixture has a family");
        distribution.p50_ns = EvidenceValue::observed(999_999);
        assert!(matches!(
            publish_bundle(&percentile, &temporary.path().join("percentile")),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut rate = bundle.clone();
        let distribution = rate
            .summary
            .families
            .values_mut()
            .next()
            .expect("fixture has a family");
        distribution.files_per_second = EvidenceValue::observed(999_999);
        assert!(matches!(
            publish_bundle(&rate, &temporary.path().join("rate")),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut outlier = bundle.clone();
        let trial = outlier
            .raw_samples
            .iter_mut()
            .find(|sample| sample.phase == "trial")
            .expect("fixture has a measured sample");
        trial.is_outlier = !trial.is_outlier;
        assert!(matches!(
            publish_bundle(&outlier, &temporary.path().join("outlier")),
            Err(BundleError::ArtifactInvariantViolation)
        ));

        let mut confidence = bundle;
        confidence.summary.confidence_intervals = Availability::Available;
        assert!(matches!(
            publish_bundle(&confidence, &temporary.path().join("confidence")),
            Err(BundleError::ArtifactInvariantViolation)
        ));
    }

    #[test]
    fn unsafe_reason_labels_are_rejected_before_publication() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let mut bundle = fixture();
        bundle.quality.precision_ppm = EvidenceValue::unavailable("../../host-path");

        let error = publish_bundle(&bundle, &temporary.path().join("result"))
            .expect_err("unsafe reason is rejected");

        assert!(matches!(error, BundleError::InvalidArtifactEncoding));
        assert!(!temporary.path().join("result").exists());
    }

    #[test]
    fn verifier_rejects_checksum_valid_cross_artifact_contradictions() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        let mut bundle = fixture();
        publish_bundle(&bundle, &destination).expect("bundle publishes");
        bundle.summary.semantic_eligibility = Availability::Available;
        let bytes = json_bytes(&bundle.summary, 64 * 1024).expect("summary serializes");
        rewrite_artifact_and_checksum(&destination, SUMMARY_FILE, &bytes);

        let error = verify_bundle(&destination).expect_err("contradiction is rejected");

        assert!(matches!(error, BundleError::ArtifactInvariantViolation));
    }

    #[test]
    fn fixed_jsonl_decode_enforces_collection_limits_before_invariants() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        let sample = RawSample {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            ordinal: 0,
            phase: "trial".to_owned(),
            dataset_entry_id: "entry".to_owned(),
            grammar_family: "rust".to_owned(),
            elapsed_ns: 1,
            source_bytes: 1,
            physical_lines: 1,
            syntax_nodes: 1,
            syntax_facts: 1,
            semantic_facts: EvidenceValue::unavailable("not_measured"),
            process_tree_cpu_ns: EvidenceValue::unavailable("not_measured"),
            process_tree_peak_rss_bytes: EvidenceValue::unavailable("not_measured"),
            outcome: crate::SampleOutcome::Succeeded,
            is_outlier: false,
        };
        let bytes = json_lines(&[sample.clone(), sample], 64 * 1024).expect("samples serialize");
        rewrite_artifact_and_checksum(&destination, RAW_SAMPLES_FILE, &bytes);
        let limits = BundleLimits {
            max_raw_samples: 1,
            ..constrained_limits()
        };

        let error = verify_bundle_with_limits(&destination, limits)
            .expect_err("raw sample decode limit is enforced");

        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "raw_sample_count"
            }
        ));
    }

    #[test]
    fn nested_collection_limits_precede_checksum_valid_decode_errors() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let summary_destination = temporary.path().join("summary");
        publish_bundle(&fixture(), &summary_destination).expect("summary bundle publishes");
        let summary = fs::read_to_string(summary_destination.join(SUMMARY_FILE))
            .expect("summary is readable")
            .replace("\"families\":{}", "\"families\":{\"a\":0,\"b\":0}");
        rewrite_artifact_and_checksum(&summary_destination, SUMMARY_FILE, summary.as_bytes());
        let limits = BundleLimits {
            max_manifest_entries: 1,
            ..constrained_limits()
        };
        assert!(matches!(
            verify_bundle_with_limits(&summary_destination, limits),
            Err(BundleError::LimitExceeded {
                resource: "summary_family_count"
            })
        ));

        let environment_destination = temporary.path().join("environment");
        let mut environment_bundle = fixture();
        environment_bundle.environment.adapter_versions =
            EvidenceValue::observed(BTreeMap::from([
                ("a".to_owned(), "one".to_owned()),
                ("b".to_owned(), "two".to_owned()),
            ]));
        let environment =
            json_bytes(&environment_bundle.environment, 64 * 1024).expect("environment serializes");
        publish_bundle(&fixture(), &environment_destination).expect("environment bundle publishes");
        let environment = String::from_utf8(environment)
            .expect("environment is UTF-8")
            .replace("\"b\":\"two\"", "\"b\":7");
        rewrite_artifact_and_checksum(
            &environment_destination,
            ENVIRONMENT_FILE,
            environment.as_bytes(),
        );
        assert!(matches!(
            verify_bundle_with_limits(&environment_destination, limits),
            Err(BundleError::LimitExceeded {
                resource: "evidence_map_entry_count"
            })
        ));

        let trajectory_destination = temporary.path().join("trajectory");
        publish_bundle(&fixture(), &trajectory_destination).expect("trajectory bundle publishes");
        let trajectory = AgentTrajectory {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            task_id: "task".to_owned(),
            eligibility: Availability::Available,
            tool_calls: vec!["first".to_owned(), "second".to_owned()],
            total_tokens: EvidenceValue::unavailable("not_measured"),
        };
        let trajectory =
            String::from_utf8(json_lines(&[trajectory], 64 * 1024).expect("trajectory serializes"))
                .expect("trajectory is UTF-8")
                .replace("\"second\"", "7");
        rewrite_artifact_and_checksum(
            &trajectory_destination,
            AGENT_TRAJECTORIES_FILE,
            trajectory.as_bytes(),
        );
        let limits = BundleLimits {
            max_command_arguments: 1,
            ..constrained_limits()
        };
        assert!(matches!(
            verify_bundle_with_limits(&trajectory_destination, limits),
            Err(BundleError::LimitExceeded {
                resource: "trajectory_tool_call_count"
            })
        ));
    }

    #[test]
    fn failed_publication_removes_partial_staging_tree() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");

        let error =
            publish_bundle_with_fault(&fixture(), &destination, BundleLimits::default(), Some(2))
                .expect_err("fault interrupts publication");

        assert!(matches!(error, BundleError::InjectedWriteFailure));
        assert!(!destination.exists());
        let remaining = fs::read_dir(temporary.path())
            .expect("temporary root is readable")
            .count();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn publication_rejects_each_bounded_collection() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let limits = constrained_limits();
        let mut bundle = fixture();
        bundle.raw_samples = vec![
            RawSample {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                ordinal: 0,
                phase: "measured".to_owned(),
                dataset_entry_id: "entry".to_owned(),
                grammar_family: "rust".to_owned(),
                elapsed_ns: 1,
                source_bytes: 1,
                physical_lines: 1,
                syntax_nodes: 1,
                syntax_facts: 1,
                semantic_facts: EvidenceValue::unavailable("not_measured"),
                process_tree_cpu_ns: EvidenceValue::unavailable("not_measured"),
                process_tree_peak_rss_bytes: EvidenceValue::unavailable("not_measured"),
                outcome: crate::SampleOutcome::Succeeded,
                is_outlier: false,
            };
            limits.max_raw_samples + 1
        ];
        let error = publish_bundle_with_limits(&bundle, &temporary.path().join("samples"), limits)
            .expect_err("raw sample bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "raw_sample_count"
            }
        ));

        let mut bundle = fixture();
        for index in 0..=limits.max_artifacts_per_class {
            bundle.profiles.insert(format!("p{index}"), vec![0]);
        }
        let error = publish_bundle_with_limits(&bundle, &temporary.path().join("profiles"), limits)
            .expect_err("artifact count bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "artifact_count"
            }
        ));

        let mut bundle = fixture();
        bundle.agent_trajectories = vec![
            AgentTrajectory {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                task_id: "task".to_owned(),
                eligibility: Availability::Unavailable {
                    reason_code: "not_measured".to_owned(),
                },
                tool_calls: Vec::new(),
                total_tokens: EvidenceValue::unavailable("not_measured"),
            };
            limits.max_agent_trajectories + 1
        ];
        let error =
            publish_bundle_with_limits(&bundle, &temporary.path().join("trajectories"), limits)
                .expect_err("trajectory count bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "agent_trajectory_count"
            }
        ));
    }

    #[test]
    fn publication_preflights_class_file_checksum_and_total_bytes() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let mut limits = constrained_limits();
        limits.max_artifact_bytes = 8;
        let error = publish_bundle_with_limits(&fixture(), &temporary.path().join("file"), limits)
            .expect_err("serialized file bound is enforced");
        assert!(matches!(error, BundleError::LimitExceeded { .. }));
        assert!(!temporary.path().join("file").exists());

        let mut invalid_later = fixture();
        invalid_later.quality.precision_ppm = EvidenceValue::unavailable("../../invalid");
        let mut limits = constrained_limits();
        limits.max_total_bytes = 256;
        let error =
            publish_bundle_with_limits(&invalid_later, &temporary.path().join("total"), limits)
                .expect_err("total bytes are preflighted before later artifact validation");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "total_bytes"
            }
        ));
        assert!(!temporary.path().join("total").exists());

        let checksum_bytes =
            checksum_manifest_size(&invalid_later).expect("checksum size is computable");
        let mut limits = constrained_limits();
        limits.max_total_bytes = checksum_bytes + 32;
        let error = publish_bundle_with_limits(
            &invalid_later,
            &temporary.path().join("rolling-total"),
            limits,
        )
        .expect_err("serialization is capped by the remaining total budget");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "total_bytes"
            }
        ));

        let mut bundle = fixture();
        bundle.profiles.insert("first.pb".to_owned(), vec![0; 40]);
        bundle.profiles.insert("second.pb".to_owned(), vec![0; 40]);
        let mut limits = constrained_limits();
        limits.max_profile_bytes = 64;
        let error =
            publish_bundle_with_limits(&bundle, &temporary.path().join("profile-bytes"), limits)
                .expect_err("profile byte bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "profile_bytes"
            }
        ));

        let mut bundle = fixture();
        bundle.profiles.insert("extra.pb".to_owned(), vec![0]);
        let mut limits = constrained_limits();
        limits.max_directory_entries = FIXED_ARTIFACT_COUNT + 3;
        let error = publish_bundle_with_limits(
            &bundle,
            &temporary.path().join("directory-entries"),
            limits,
        )
        .expect_err("checksum and class directories count toward directory entries");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "directory_entry_count"
            }
        ));

        let mut bundle = fixture();
        bundle
            .logs
            .insert("first.log".to_owned(), large_operational_log(0, 8));
        bundle
            .logs
            .insert("second.log".to_owned(), large_operational_log(100, 8));
        let mut limits = constrained_limits();
        limits.max_log_bytes = 512;
        let error =
            publish_bundle_with_limits(&bundle, &temporary.path().join("log-bytes"), limits)
                .expect_err("log byte bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "log_bytes"
            }
        ));

        let mut limits = constrained_limits();
        limits.max_checksum_bytes = 128;
        let error =
            publish_bundle_with_limits(&invalid_later, &temporary.path().join("checksums"), limits)
                .expect_err("checksum bytes are preflighted before later artifact validation");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "checksum_bytes"
            }
        ));
    }

    #[test]
    fn verification_bounds_file_count_depth_size_and_total_bytes() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");

        let mut limits = constrained_limits();
        limits.max_file_count = 10;
        verify_bundle_with_limits(&destination, limits).expect("ten normative files fit");
        fs::write(destination.join("extra"), b"x").expect("extra file is created");
        let error = verify_bundle_with_limits(&destination, limits)
            .expect_err("file-count bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "file_count"
            }
        ));
        fs::remove_file(destination.join("extra")).expect("extra file is removed");

        let deep = destination.join("profiles").join("a").join("b");
        fs::create_dir_all(&deep).expect("deep fixture is created");
        let mut depth_limits = constrained_limits();
        depth_limits.max_depth = 2;
        let error = verify_bundle_with_limits(&destination, depth_limits)
            .expect_err("depth bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "directory_depth"
            }
        ));
        fs::remove_dir_all(destination.join("profiles").join("a"))
            .expect("deep fixture is removed");

        fs::create_dir(destination.join("empty")).expect("extra directory is created");
        let mut entry_limits = constrained_limits();
        entry_limits.max_directory_entries = FIXED_ARTIFACT_COUNT + 3;
        let error = verify_bundle_with_limits(&destination, entry_limits)
            .expect_err("directory-entry bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "directory_entry_count"
            }
        ));
        fs::remove_dir(destination.join("empty")).expect("extra directory is removed");

        let mut size_limits = constrained_limits();
        size_limits.max_artifact_bytes = 16;
        let error = verify_bundle_with_limits(&destination, size_limits)
            .expect_err("file-size bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "artifact_bytes"
            }
        ));

        let mut total_limits = constrained_limits();
        total_limits.max_total_bytes = 256;
        let error = verify_bundle_with_limits(&destination, total_limits)
            .expect_err("total-size bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "total_bytes"
            }
        ));
    }

    #[test]
    fn checksum_manifest_requires_lowercase_and_bounded_lines() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        let checksum_path = destination.join(CHECKSUMS_FILE);
        let mut bytes = fs::read(&checksum_path).expect("checksum fixture is readable");
        let hex_index = bytes[..64]
            .iter()
            .position(u8::is_ascii_lowercase)
            .expect("SHA-256 fixture contains a hexadecimal letter");
        bytes[hex_index] = bytes[hex_index].to_ascii_uppercase();
        fs::write(&checksum_path, bytes).expect("uppercase fixture is written");
        let error = verify_bundle(&destination).expect_err("uppercase checksum is rejected");
        assert!(matches!(error, BundleError::InvalidChecksumManifest));

        publish_bundle(&fixture(), &temporary.path().join("line-result"))
            .expect("second bundle publishes");
        let mut limits = constrained_limits();
        limits.max_checksum_lines = 8;
        let error = verify_bundle_with_limits(&temporary.path().join("line-result"), limits)
            .expect_err("checksum line bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "checksum_line_count"
            }
        ));

        let crlf_destination = temporary.path().join("crlf-result");
        publish_bundle(&fixture(), &crlf_destination).expect("CRLF fixture publishes");
        let checksum_path = crlf_destination.join(CHECKSUMS_FILE);
        let checksums = fs::read_to_string(&checksum_path).expect("checksums are readable");
        fs::write(&checksum_path, checksums.replace('\n', "\r\n"))
            .expect("CRLF checksums are written");
        assert!(matches!(
            verify_bundle(&crlf_destination),
            Err(BundleError::InvalidChecksumManifest)
        ));
    }

    #[test]
    fn artifact_names_reject_paths_controls_and_windows_aliases() {
        for name in [
            ".",
            "..",
            "nested/name",
            "nested\\name",
            "bad\nname",
            "CON",
            "con.txt",
            "LPT9.log",
            "checksums.txt",
            "trailing.",
        ] {
            assert!(
                matches!(
                    validate_artifact_name(name),
                    Err(BundleError::InvalidArtifactName)
                ),
                "{name:?} must be rejected"
            );
        }
        validate_artifact_name("cpu-profile.pb").expect("canonical artifact name is accepted");
    }

    #[test]
    fn operational_logs_reject_arbitrary_source_and_secret_labels() {
        for value in [
            "fn secret() {}",
            "api_key=super-secret",
            "C:\\Users\\person\\source.rs",
            "/home/person/source.rs",
        ] {
            assert!(matches!(
                OperationalEvent::from_label(value),
                Err(BundleError::InvalidLog)
            ));
            assert!(matches!(
                OperationalStatus::from_label(value),
                Err(BundleError::InvalidLog)
            ));
        }
        let log = OperationalLog::new(vec![OperationalLogRecord {
            sequence: 0,
            event: OperationalEvent::SampleCompleted,
            status: OperationalStatus::TimedOut,
            sample_ordinal: Some(4),
            elapsed_ns: Some(1_000),
        }])
        .expect("closed operational record is accepted");
        assert_eq!(log.records().len(), 1);
    }

    #[test]
    fn caller_cannot_raise_absolute_ceiling() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let mut limits = BundleLimits::default();
        limits.max_raw_samples += 1;
        let error =
            publish_bundle_with_limits(&fixture(), &temporary.path().join("result"), limits)
                .expect_err("hard ceiling cannot be raised");
        assert!(matches!(error, BundleError::InvalidLimits));
    }

    fn large_operational_log(first_sequence: u64, count: u64) -> OperationalLog {
        let records = (0..count)
            .map(|offset| OperationalLogRecord {
                sequence: first_sequence + offset,
                event: OperationalEvent::SampleCompleted,
                status: OperationalStatus::Succeeded,
                sample_ordinal: Some(offset),
                elapsed_ns: Some(1),
            })
            .collect();
        OperationalLog::new(records).expect("bounded operational log is valid")
    }

    struct CurrentDirectoryGuard(std::path::PathBuf);

    impl Drop for CurrentDirectoryGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
}
