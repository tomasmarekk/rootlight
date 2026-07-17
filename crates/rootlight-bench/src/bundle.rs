//! Immutable result-bundle publication and bounded checksum verification.
//!
//! Publication performs all serialization and size accounting before it
//! creates the staging directory. Verification bounds directory traversal and
//! every read before allocating artifact contents.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::Path,
};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::{
    AgentTrajectory, BenchmarkCommand, BuildProvenance, CoverageEvidence, DatasetManifest,
    EnvironmentEvidence, QualityEvidence, RawSample, ResultSummary,
};

const ENVIRONMENT_FILE: &str = "environment.json";
const DATASET_MANIFEST_FILE: &str = "dataset-manifest.json";
const BUILD_PROVENANCE_FILE: &str = "build-provenance.json";
const COMMAND_FILE: &str = "command.json";
const RAW_SAMPLES_FILE: &str = "raw-samples.jsonl";
const SUMMARY_FILE: &str = "summary.json";
const COVERAGE_FILE: &str = "coverage.json";
const QUALITY_FILE: &str = "quality.json";
const AGENT_TRAJECTORIES_FILE: &str = "agent-trajectories.jsonl";
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

/// UTF-8 log bytes screened for control characters and path-shaped tokens.
///
/// The type prevents common host-path disclosure through the result-bundle
/// interface. Callers remain responsible for supplying operation summaries,
/// never source excerpts, credentials, or other sensitive payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFreeLog(Vec<u8>);

impl SourceFreeLog {
    /// Creates a source-free log artifact.
    ///
    /// # Errors
    ///
    /// Returns [`BundleError::InvalidLog`] for non-UTF-8, control characters,
    /// path-shaped tokens, or an artifact above the absolute log-file ceiling.
    pub fn new(bytes: Vec<u8>) -> Result<Self, BundleError> {
        validate_log(&bytes, HARD_MAX_ARTIFACT_BYTES)?;
        Ok(Self(bytes))
    }

    /// Returns the validated log bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
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
    pub logs: BTreeMap<String, SourceFreeLog>,
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
/// is created. The destination parent must already exist.
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
    let limits = limits.validate()?;
    let observed = collect_files(destination, limits)?;
    let checksum_size = *observed
        .get(CHECKSUMS_FILE)
        .ok_or(BundleError::ArtifactSetMismatch)?;
    if checksum_size > limits.max_checksum_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "checksum_bytes",
        });
    }
    let checksum_bytes = read_bounded(
        &destination.join(CHECKSUMS_FILE),
        limits.max_checksum_bytes,
        "read checksum manifest",
    )?;
    let checksum_text =
        std::str::from_utf8(&checksum_bytes).map_err(|_| BundleError::InvalidChecksumManifest)?;
    let expected = parse_checksums(checksum_text, limits)?;
    let mut observed_paths = observed.keys().cloned().collect::<BTreeSet<_>>();
    observed_paths.remove(CHECKSUMS_FILE);
    let expected_paths = expected.keys().cloned().collect::<BTreeSet<_>>();
    if observed_paths != expected_paths {
        return Err(BundleError::ArtifactSetMismatch);
    }
    for (relative, checksum) in expected {
        let bytes = read_bounded(
            &destination.join(&relative),
            limits.max_artifact_bytes,
            "read result artifact",
        )?;
        if sha256_hex(&bytes) != checksum {
            return Err(BundleError::ChecksumMismatch);
        }
    }
    Ok(())
}

fn publish_bundle_with_fault(
    bundle: &ResultBundle,
    destination: &Path,
    limits: BundleLimits,
    fail_after_writes: Option<usize>,
) -> Result<(), BundleError> {
    let limits = limits.validate()?;
    if destination.exists() {
        return Err(BundleError::DestinationExists);
    }
    let artifacts = build_artifacts(bundle, limits)?;
    let parent = destination
        .parent()
        .ok_or(BundleError::InvalidDestination)?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(BundleError::InvalidDestination)?;
    let staging = parent.join(format!(".{file_name}.partial-{}", std::process::id()));
    fs::create_dir(&staging).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            BundleError::StagingExists
        } else {
            BundleError::Io {
                operation: "create staging directory",
                source,
            }
        }
    })?;

    let publication = write_bundle(&artifacts, &staging, fail_after_writes).and_then(|()| {
        fs::rename(&staging, destination).map_err(|source| BundleError::Io {
            operation: "publish result bundle",
            source,
        })
    });
    if publication.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    publication
}

fn build_artifacts(
    bundle: &ResultBundle,
    limits: BundleLimits,
) -> Result<BTreeMap<String, Vec<u8>>, BundleError> {
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
    validate_log_map(&bundle.logs, limits)?;

    let serialized_limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ENVIRONMENT_FILE.to_owned(),
        json_bytes(&bundle.environment, serialized_limit)?,
    );
    artifacts.insert(
        DATASET_MANIFEST_FILE.to_owned(),
        json_bytes(&bundle.dataset_manifest, serialized_limit)?,
    );
    artifacts.insert(
        BUILD_PROVENANCE_FILE.to_owned(),
        json_bytes(&bundle.build_provenance, serialized_limit)?,
    );
    artifacts.insert(
        COMMAND_FILE.to_owned(),
        json_bytes(&bundle.command, serialized_limit)?,
    );
    artifacts.insert(
        RAW_SAMPLES_FILE.to_owned(),
        json_lines(&bundle.raw_samples, serialized_limit)?,
    );
    artifacts.insert(
        SUMMARY_FILE.to_owned(),
        json_bytes(&bundle.summary, serialized_limit)?,
    );
    artifacts.insert(
        COVERAGE_FILE.to_owned(),
        json_bytes(&bundle.coverage, serialized_limit)?,
    );
    artifacts.insert(
        QUALITY_FILE.to_owned(),
        json_bytes(&bundle.quality, serialized_limit)?,
    );
    artifacts.insert(
        AGENT_TRAJECTORIES_FILE.to_owned(),
        json_lines(&bundle.agent_trajectories, serialized_limit)?,
    );

    let mut profile_bytes = 0_u64;
    for (name, bytes) in &bundle.profiles {
        check_bytes(bytes.len(), limits.max_artifact_bytes, "artifact_bytes")?;
        add_bytes(
            &mut profile_bytes,
            bytes.len(),
            limits.max_profile_bytes,
            "profile_bytes",
        )?;
        artifacts.insert(format!("profiles/{name}"), bytes.clone());
    }
    let mut log_bytes = 0_u64;
    for (name, log) in &bundle.logs {
        add_bytes(
            &mut log_bytes,
            log.as_bytes().len(),
            limits.max_log_bytes,
            "log_bytes",
        )?;
        artifacts.insert(format!("logs/{name}"), log.as_bytes().to_vec());
    }

    check_count(artifacts.len() + 1, limits.max_file_count, "file_count")?;
    check_count(
        artifacts.len(),
        limits.max_checksum_lines,
        "checksum_line_count",
    )?;
    let checksums = checksum_manifest(&artifacts, limits)?;
    let mut total = u64::try_from(checksums.len()).map_err(|_| BundleError::LimitExceeded {
        resource: "total_bytes",
    })?;
    for bytes in artifacts.values() {
        add_bytes(
            &mut total,
            bytes.len(),
            limits.max_total_bytes,
            "total_bytes",
        )?;
    }
    if total > limits.max_total_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "total_bytes",
        });
    }
    artifacts.insert(CHECKSUMS_FILE.to_owned(), checksums);
    Ok(artifacts)
}

fn checksum_manifest(
    artifacts: &BTreeMap<String, Vec<u8>>,
    limits: BundleLimits,
) -> Result<Vec<u8>, BundleError> {
    let checksum_limit =
        usize::try_from(limits.max_checksum_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "checksum_bytes",
        })?;
    let mut checksums = BoundedBuffer::new(checksum_limit);
    for (relative, bytes) in artifacts {
        let line = format!("{}  {relative}\n", sha256_hex(bytes));
        checksums
            .write_all(line.as_bytes())
            .map_err(|_| BundleError::LimitExceeded {
                resource: "checksum_bytes",
            })?;
    }
    Ok(checksums.into_inner())
}

fn write_bundle(
    artifacts: &BTreeMap<String, Vec<u8>>,
    staging: &Path,
    fail_after_writes: Option<usize>,
) -> Result<(), BundleError> {
    fs::create_dir(staging.join("profiles")).map_err(|source| BundleError::Io {
        operation: "create profiles directory",
        source,
    })?;
    fs::create_dir(staging.join("logs")).map_err(|source| BundleError::Io {
        operation: "create logs directory",
        source,
    })?;

    for (write_count, (relative, bytes)) in artifacts.iter().enumerate() {
        if fail_after_writes == Some(write_count) {
            return Err(BundleError::InjectedWriteFailure);
        }
        write_new(&staging.join(relative), bytes)?;
    }
    Ok(())
}

fn json_bytes(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, BundleError> {
    let mut bytes = BoundedBuffer::new(limit);
    let result = serde_json::to_writer(&mut bytes, value);
    if bytes.exceeded() {
        return Err(BundleError::LimitExceeded {
            resource: "serialized_artifact_bytes",
        });
    }
    result.map_err(BundleError::Serialize)?;
    bytes
        .write_all(b"\n")
        .map_err(|_| BundleError::LimitExceeded {
            resource: "serialized_artifact_bytes",
        })?;
    Ok(bytes.into_inner())
}

fn json_lines<T: Serialize>(values: &[T], limit: usize) -> Result<Vec<u8>, BundleError> {
    let mut bytes = BoundedBuffer::new(limit);
    for value in values {
        let result = serde_json::to_writer(&mut bytes, value);
        if bytes.exceeded() {
            return Err(BundleError::LimitExceeded {
                resource: "serialized_artifact_bytes",
            });
        }
        result.map_err(BundleError::Serialize)?;
        bytes
            .write_all(b"\n")
            .map_err(|_| BundleError::LimitExceeded {
                resource: "serialized_artifact_bytes",
            })?;
    }
    Ok(bytes.into_inner())
}

#[derive(Debug)]
struct BoundedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
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
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
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
    artifacts: &BTreeMap<String, SourceFreeLog>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    check_count(
        artifacts.len(),
        limits.max_artifacts_per_class,
        "artifact_count",
    )?;
    for (name, log) in artifacts {
        validate_artifact_name(name)?;
        validate_log(log.as_bytes(), limits.max_artifact_bytes)?;
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

fn validate_log(bytes: &[u8], max_bytes: u64) -> Result<(), BundleError> {
    check_bytes(bytes.len(), max_bytes, "artifact_bytes")?;
    let text = std::str::from_utf8(bytes).map_err(|_| BundleError::InvalidLog)?;
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(BundleError::InvalidLog);
    }
    for token in text.split_whitespace() {
        let path_shaped = token.starts_with('/')
            || token.starts_with('\\')
            || token.starts_with("~/")
            || token.contains('\\')
            || token.contains("://")
            || token.contains("../")
            || token.as_bytes().get(1).is_some_and(|byte| *byte == b':');
        if path_shaped {
            return Err(BundleError::InvalidLog);
        }
    }
    Ok(())
}

fn parse_checksums(
    text: &str,
    limits: BundleLimits,
) -> Result<BTreeMap<String, String>, BundleError> {
    if text.is_empty() || !text.ends_with('\n') {
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

fn collect_files(root: &Path, limits: BundleLimits) -> Result<BTreeMap<String, u64>, BundleError> {
    let root_metadata = fs::symlink_metadata(root).map_err(|source| BundleError::Io {
        operation: "inspect result bundle",
        source,
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(BundleError::UnsupportedArtifactType);
    }
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut paths = BTreeMap::new();
    let mut visited_entries = 0_usize;
    let mut total_bytes = 0_u64;
    while let Some((current, depth)) = pending.pop() {
        if depth > limits.max_depth {
            return Err(BundleError::LimitExceeded {
                resource: "directory_depth",
            });
        }
        let entries = fs::read_dir(current).map_err(|source| BundleError::Io {
            operation: "enumerate result bundle",
            source,
        })?;
        for entry in entries {
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
            let entry = entry.map_err(|source| BundleError::Io {
                operation: "read result directory entry",
                source,
            })?;
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| BundleError::Io {
                    operation: "inspect result artifact",
                    source,
                })?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                return Err(BundleError::UnsupportedArtifactType);
            }
            if file_type.is_dir() {
                let next_depth = depth.checked_add(1).ok_or(BundleError::LimitExceeded {
                    resource: "directory_depth",
                })?;
                if next_depth > limits.max_depth {
                    return Err(BundleError::LimitExceeded {
                        resource: "directory_depth",
                    });
                }
                pending.push((entry.path(), next_depth));
            } else if file_type.is_file() {
                check_count(paths.len() + 1, limits.max_file_count, "file_count")?;
                if metadata.len() > limits.max_artifact_bytes {
                    return Err(BundleError::LimitExceeded {
                        resource: "artifact_bytes",
                    });
                }
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
                let entry_path = entry.path();
                let relative = entry_path
                    .strip_prefix(root)
                    .map_err(|_| BundleError::InvalidDestination)?
                    .to_str()
                    .ok_or(BundleError::InvalidChecksumManifest)?
                    .replace('\\', "/");
                if relative.len() > limits.max_string_bytes
                    || paths.insert(relative, metadata.len()).is_some()
                {
                    return Err(BundleError::InvalidChecksumManifest);
                }
            } else {
                return Err(BundleError::UnsupportedArtifactType);
            }
        }
    }
    Ok(paths)
}

fn read_bounded(path: &Path, limit: u64, operation: &'static str) -> Result<Vec<u8>, BundleError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| BundleError::Io { operation, source })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(BundleError::UnsupportedArtifactType);
    }
    if metadata.len() > limit {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
    }
    let file = File::open(path).map_err(|source| BundleError::Io { operation, source })?;
    let read_limit = limit.checked_add(1).ok_or(BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| BundleError::Io { operation, source })?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
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
    /// The verifier encountered a link or special file.
    #[error("result bundle contains an unsupported artifact type")]
    UnsupportedArtifactType,
    /// Test-only failure after a bounded number of writes.
    #[error("injected result write failure")]
    InjectedWriteFailure,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Availability, BenchmarkCommand, BuildProvenance, CoverageEvidence, DatasetManifest,
        EnvironmentEvidence, EvidenceValue, QualityEvidence, ResultSummary,
    };

    fn fixture() -> ResultBundle {
        ResultBundle {
            environment: EnvironmentEvidence {
                schema_version: "1.0".to_owned(),
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
                adapter_versions: BTreeMap::new(),
                grammar_hashes: BTreeMap::new(),
                locale: EvidenceValue::unavailable("not_sampled"),
                background_process_policy: EvidenceValue::unavailable("not_sampled"),
                clock_source: EvidenceValue::observed("std_instant".to_owned()),
                process_tree_accounting: Availability::Unavailable {
                    reason_code: "no_platform_sampler".to_owned(),
                },
            },
            dataset_manifest: DatasetManifest {
                schema_version: "1.0".to_owned(),
                dataset_id: "fixture".to_owned(),
                revision: "sha256:00".to_owned(),
                scope_rule: "listed_entries".to_owned(),
                loc_counting_rule: "physical_newlines".to_owned(),
                entries: Vec::new(),
            },
            build_provenance: BuildProvenance {
                schema_version: "1.0".to_owned(),
                source_revision: "source".to_owned(),
                binary_revision: "binary".to_owned(),
                build_profile: "test".to_owned(),
                features: Vec::new(),
                target: "test-target".to_owned(),
            },
            command: BenchmarkCommand {
                schema_version: "1.0".to_owned(),
                subcommand: "m05-parser".to_owned(),
                arguments: Vec::new(),
                seed: 7,
                warmup_rounds: 1,
                trial_rounds: 1,
                timeout_ms: 100,
            },
            raw_samples: Vec::new(),
            summary: ResultSummary {
                schema_version: "1.0".to_owned(),
                benchmark_id: "BENCH-PARSE-001".to_owned(),
                semantic_eligibility: Availability::Unavailable {
                    reason_code: "extraction_not_integrated".to_owned(),
                },
                families: BTreeMap::new(),
                failed_samples: 0,
                timed_out_samples: 0,
                cancelled_samples: 0,
                confidence_intervals: Availability::Unavailable {
                    reason_code: "insufficient_samples".to_owned(),
                },
            },
            coverage: CoverageEvidence {
                schema_version: "1.0".to_owned(),
                attempted_entries: 0,
                committed_entries: 0,
                skipped: BTreeMap::new(),
                parser_status: BTreeMap::new(),
            },
            quality: QualityEvidence {
                schema_version: "1.0".to_owned(),
                rubric_id: "m05-parser-1.0".to_owned(),
                semantic_eligibility: Availability::Unavailable {
                    reason_code: "extraction_not_integrated".to_owned(),
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
    fn publication_never_overwrites_existing_evidence() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("initial bundle publishes");

        let error = publish_bundle(&fixture(), &destination).expect_err("overwrite is rejected");

        assert!(matches!(error, BundleError::DestinationExists));
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
    fn failed_publication_removes_partial_staging_tree() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");

        let error =
            publish_bundle_with_fault(&fixture(), &destination, BundleLimits::default(), Some(2))
                .expect_err("fault interrupts publication");

        assert!(matches!(error, BundleError::InjectedWriteFailure));
        assert!(!destination.exists());
        let staging = temporary
            .path()
            .join(format!(".result.partial-{}", std::process::id()));
        assert!(!staging.exists());
    }

    #[test]
    fn publication_rejects_each_bounded_collection() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let limits = constrained_limits();
        let mut bundle = fixture();
        bundle.raw_samples = vec![
            RawSample {
                schema_version: "1.0".to_owned(),
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
                schema_version: "1.0".to_owned(),
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

        let mut limits = constrained_limits();
        limits.max_total_bytes = 256;
        let error = publish_bundle_with_limits(&fixture(), &temporary.path().join("total"), limits)
            .expect_err("total byte bound is enforced");
        assert!(matches!(
            error,
            BundleError::LimitExceeded {
                resource: "total_bytes"
            }
        ));
        assert!(!temporary.path().join("total").exists());

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
        bundle.logs.insert(
            "first.log".to_owned(),
            SourceFreeLog::new(vec![b'x'; 40]).expect("source-free fixture log is valid"),
        );
        bundle.logs.insert(
            "second.log".to_owned(),
            SourceFreeLog::new(vec![b'x'; 40]).expect("source-free fixture log is valid"),
        );
        let mut limits = constrained_limits();
        limits.max_log_bytes = 64;
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
            publish_bundle_with_limits(&fixture(), &temporary.path().join("checksums"), limits)
                .expect_err("checksum byte bound is enforced");
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
    fn log_type_rejects_host_paths_and_controls() {
        for bytes in [
            b"failed at C:\\Users\\person\\source.rs".as_slice(),
            b"failed at /home/person/source.rs".as_slice(),
            b"failed at ../source.rs".as_slice(),
            b"invalid\0log".as_slice(),
        ] {
            assert!(matches!(
                SourceFreeLog::new(bytes.to_vec()),
                Err(BundleError::InvalidLog)
            ));
        }
        SourceFreeLog::new(b"parse_timeout reason_code=deadline_elapsed\n".to_vec())
            .expect("source-free operation summary is accepted");
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
}
