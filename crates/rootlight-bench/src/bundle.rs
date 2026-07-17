//! Immutable result-bundle publication and checksum verification.
//!
//! Publication stages all files beside the destination and renames only after
//! every write succeeds, so interrupted runs cannot resemble complete evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write as _},
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
    pub logs: BTreeMap<String, Vec<u8>>,
}

/// Publishes one immutable result bundle at a new destination.
///
/// The destination parent must already exist. The function fails when the
/// destination or its operation-owned staging directory exists.
///
/// # Errors
///
/// Returns [`BundleError`] for invalid artifact names, serialization failures,
/// an existing destination, or bounded filesystem publication failures.
pub fn publish_bundle(bundle: &ResultBundle, destination: &Path) -> Result<(), BundleError> {
    publish_bundle_with_fault(bundle, destination, None)
}

/// Verifies every declared checksum and rejects missing or unexpected files.
///
/// # Errors
///
/// Returns [`BundleError`] for malformed manifests, I/O failures, missing or
/// unexpected artifacts, and checksum mismatches.
pub fn verify_bundle(destination: &Path) -> Result<(), BundleError> {
    let checksum_bytes =
        fs::read(destination.join(CHECKSUMS_FILE)).map_err(|source| BundleError::Io {
            operation: "read checksum manifest",
            source,
        })?;
    let checksum_text =
        std::str::from_utf8(&checksum_bytes).map_err(|_| BundleError::InvalidChecksumManifest)?;
    let expected = parse_checksums(checksum_text)?;
    let mut observed_paths = BTreeSet::new();
    collect_files(destination, destination, &mut observed_paths)?;
    observed_paths.remove(CHECKSUMS_FILE);
    let expected_paths = expected.keys().cloned().collect::<BTreeSet<_>>();
    if observed_paths != expected_paths {
        return Err(BundleError::ArtifactSetMismatch);
    }
    for (relative, checksum) in expected {
        let bytes = fs::read(destination.join(&relative)).map_err(|source| BundleError::Io {
            operation: "read result artifact",
            source,
        })?;
        if sha256_hex(&bytes) != checksum {
            return Err(BundleError::ChecksumMismatch { artifact: relative });
        }
    }
    Ok(())
}

fn publish_bundle_with_fault(
    bundle: &ResultBundle,
    destination: &Path,
    fail_after_writes: Option<usize>,
) -> Result<(), BundleError> {
    if destination.exists() {
        return Err(BundleError::DestinationExists);
    }
    validate_artifact_map(&bundle.profiles)?;
    validate_artifact_map(&bundle.logs)?;
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

    let publication = write_bundle(bundle, &staging, fail_after_writes).and_then(|()| {
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

fn write_bundle(
    bundle: &ResultBundle,
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

    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        ENVIRONMENT_FILE.to_owned(),
        json_bytes(&bundle.environment)?,
    );
    artifacts.insert(
        DATASET_MANIFEST_FILE.to_owned(),
        json_bytes(&bundle.dataset_manifest)?,
    );
    artifacts.insert(
        BUILD_PROVENANCE_FILE.to_owned(),
        json_bytes(&bundle.build_provenance)?,
    );
    artifacts.insert(COMMAND_FILE.to_owned(), json_bytes(&bundle.command)?);
    artifacts.insert(
        RAW_SAMPLES_FILE.to_owned(),
        json_lines(&bundle.raw_samples)?,
    );
    artifacts.insert(SUMMARY_FILE.to_owned(), json_bytes(&bundle.summary)?);
    artifacts.insert(COVERAGE_FILE.to_owned(), json_bytes(&bundle.coverage)?);
    artifacts.insert(QUALITY_FILE.to_owned(), json_bytes(&bundle.quality)?);
    artifacts.insert(
        AGENT_TRAJECTORIES_FILE.to_owned(),
        json_lines(&bundle.agent_trajectories)?,
    );
    for (name, bytes) in &bundle.profiles {
        artifacts.insert(format!("profiles/{name}"), bytes.clone());
    }
    for (name, bytes) in &bundle.logs {
        artifacts.insert(format!("logs/{name}"), bytes.clone());
    }

    let mut checksums = String::new();
    for (write_count, (relative, bytes)) in artifacts.iter().enumerate() {
        if fail_after_writes == Some(write_count) {
            return Err(BundleError::InjectedWriteFailure);
        }
        write_new(&staging.join(relative), bytes)?;
        checksums.push_str(&sha256_hex(bytes));
        checksums.push_str("  ");
        checksums.push_str(relative);
        checksums.push('\n');
    }
    write_new(&staging.join(CHECKSUMS_FILE), checksums.as_bytes())?;
    Ok(())
}

fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, BundleError> {
    let mut bytes = serde_json::to_vec(value).map_err(BundleError::Serialize)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn json_lines<T: Serialize>(values: &[T]) -> Result<Vec<u8>, BundleError> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).map_err(BundleError::Serialize)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
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

fn validate_artifact_map(artifacts: &BTreeMap<String, Vec<u8>>) -> Result<(), BundleError> {
    for name in artifacts.keys() {
        let path = Path::new(name);
        if name.is_empty()
            || name.len() > 255
            || path.components().count() != 1
            || name == CHECKSUMS_FILE
        {
            return Err(BundleError::InvalidArtifactName);
        }
    }
    Ok(())
}

fn parse_checksums(text: &str) -> Result<BTreeMap<String, String>, BundleError> {
    let mut checksums = BTreeMap::new();
    for line in text.lines() {
        let (checksum, relative) = line
            .split_once("  ")
            .ok_or(BundleError::InvalidChecksumManifest)?;
        if checksum.len() != 64
            || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
            || checksums
                .insert(relative.to_owned(), checksum.to_ascii_lowercase())
                .is_some()
        {
            return Err(BundleError::InvalidChecksumManifest);
        }
    }
    if checksums.is_empty() {
        return Err(BundleError::InvalidChecksumManifest);
    }
    Ok(checksums)
}

fn collect_files(
    root: &Path,
    current: &Path,
    paths: &mut BTreeSet<String>,
) -> Result<(), BundleError> {
    let entries = fs::read_dir(current).map_err(|source| BundleError::Io {
        operation: "enumerate result bundle",
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| BundleError::Io {
            operation: "read result directory entry",
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| BundleError::Io {
            operation: "inspect result artifact",
            source,
        })?;
        if file_type.is_symlink() {
            return Err(BundleError::UnsupportedArtifactType);
        }
        if file_type.is_dir() {
            collect_files(root, &entry.path(), paths)?;
        } else if file_type.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| BundleError::InvalidDestination)?
                .to_string_lossy()
                .replace('\\', "/");
            paths.insert(relative);
        } else {
            return Err(BundleError::UnsupportedArtifactType);
        }
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
    /// The final destination already exists.
    #[error("result destination already exists")]
    DestinationExists,
    /// The operation-owned staging directory already exists.
    #[error("result staging directory already exists")]
    StagingExists,
    /// The destination cannot be represented safely.
    #[error("result destination is invalid")]
    InvalidDestination,
    /// A profile or log name is not one safe path component.
    #[error("result artifact name is invalid")]
    InvalidArtifactName,
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
    /// The checksum manifest is malformed.
    #[error("checksum manifest is invalid")]
    InvalidChecksumManifest,
    /// The bundle contains missing or unexpected artifacts.
    #[error("result artifact set does not match checksum manifest")]
    ArtifactSetMismatch,
    /// One artifact failed checksum verification.
    #[error("result artifact checksum mismatch: {artifact}")]
    ChecksumMismatch {
        /// Bundle-relative artifact name.
        artifact: String,
    },
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
    fn verification_detects_tampering() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");
        publish_bundle(&fixture(), &destination).expect("bundle publishes");
        fs::write(destination.join(SUMMARY_FILE), b"{}\n").expect("fixture is tampered");

        let error = verify_bundle(&destination).expect_err("tampering is rejected");

        assert!(matches!(error, BundleError::ChecksumMismatch { .. }));
    }

    #[test]
    fn failed_publication_removes_partial_staging_tree() {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        let destination = temporary.path().join("result");

        let error = publish_bundle_with_fault(&fixture(), &destination, Some(2))
            .expect_err("fault interrupts publication");

        assert!(matches!(error, BundleError::InjectedWriteFailure));
        assert!(!destination.exists());
        let staging = temporary
            .path()
            .join(format!(".result.partial-{}", std::process::id()));
        assert!(!staging.exists());
    }
}
