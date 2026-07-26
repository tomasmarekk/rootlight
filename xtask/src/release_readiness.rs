//! Fail-closed production-readiness evaluation.
//!
//! The report preserves passed, failed, unavailable, and unmeasured evidence
//! separately so missing candidate observations cannot become inferred passes.

#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

use crate::{datasets, incident, package};

const MANIFEST_PATH: &str = "benchmarks/readiness.toml";
const MANIFEST_SCHEMA: &str = "rootlight.release-readiness/1";
const REPORT_SCHEMA: &str = "rootlight.release-readiness-report/1";
const MAX_MANIFEST_BYTES: u64 = 512 * 1024;
const OWNER: &str = "@tomasmarekk";
const EXPECTED_REQUIREMENTS: [&str; 18] = [
    "agent-workflow-quality",
    "baseline-codebase-memory",
    "baseline-file-explorer",
    "benchmark-dataset-integrity",
    "core-offline-security",
    "cross-platform-package-reproducibility",
    "incident-response-controls",
    "language-workspace-scorecards",
    "mandatory-performance-budgets",
    "package-owned-paths",
    "previous-version-compatibility",
    "private-catalog-storage",
    "recovery-migration-scorecards",
    "release-artifact-signatures",
    "release-sbom-provenance",
    "supply-chain-vulnerability-status",
    "support-bundle-privacy",
    "update-rollback",
];

#[derive(Debug)]
pub(crate) struct Options {
    output: PathBuf,
    source_revision: String,
    require_ready: bool,
}

impl Options {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, ReadinessError> {
        let mut output = None;
        let mut source_revision = None;
        let mut require_ready = false;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--require-ready" => {
                    if require_ready {
                        return Err(ReadinessError::DuplicateFlag("--require-ready"));
                    }
                    require_ready = true;
                }
                "--output" | "--source-revision" => {
                    let value = args
                        .next()
                        .ok_or_else(|| ReadinessError::MissingFlagValue(flag.clone()))?;
                    if flag == "--output" {
                        assign_once(&mut output, PathBuf::from(value), "--output")?;
                    } else {
                        assign_once(&mut source_revision, value, "--source-revision")?;
                    }
                }
                _ => return Err(ReadinessError::UnexpectedArgument(flag)),
            }
        }
        Ok(Self {
            output: output.ok_or(ReadinessError::MissingRequiredFlag("--output"))?,
            source_revision: source_revision
                .ok_or(ReadinessError::MissingRequiredFlag("--source-revision"))?,
            require_ready,
        })
    }
}

pub(crate) fn evaluate(options: &Options) -> Result<(), ReadinessError> {
    validate_source_revision(&options.source_revision)?;
    let workspace = workspace_root()?;
    let manifest_bytes = read_regular_bounded(&workspace.join(MANIFEST_PATH), MAX_MANIFEST_BYTES)?;
    let manifest = parse_manifest(&manifest_bytes)?;
    validate_manifest(&manifest)?;
    verify_passed_contracts(&manifest)?;
    let decision = decide(&manifest.requirements);
    let report = build_report(
        &manifest,
        &manifest_bytes,
        &options.source_revision,
        decision,
    );
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(ReadinessError::SerializeReport)?;
    bytes.push(b'\n');
    persist_new_file(&options.output, &bytes)?;

    println!(
        "release readiness decision: {} ({} blockers)",
        decision.as_str(),
        report.blockers.len()
    );
    if options.require_ready && decision != Decision::Ready {
        return Err(ReadinessError::ReleaseBlocked {
            blockers: report.blockers.len(),
        });
    }
    Ok(())
}

fn assign_once<T>(
    slot: &mut Option<T>,
    value: T,
    flag: &'static str,
) -> Result<(), ReadinessError> {
    if slot.replace(value).is_some() {
        return Err(ReadinessError::DuplicateFlag(flag));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, ReadinessError> {
    let mut candidate = std::env::current_dir().map_err(ReadinessError::WorkingDir)?;
    for _ in 0..8 {
        if candidate.join("Cargo.toml").is_file() && candidate.join(MANIFEST_PATH).is_file() {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Err(ReadinessError::InvalidManifest(
        "run readiness tooling from within the workspace".to_owned(),
    ))
}

fn parse_manifest(bytes: &[u8]) -> Result<ReadinessManifest, ReadinessError> {
    let text = std::str::from_utf8(bytes).map_err(ReadinessError::InvalidUtf8)?;
    toml::from_str(text).map_err(ReadinessError::ParseManifest)
}

fn validate_manifest(manifest: &ReadinessManifest) -> Result<(), ReadinessError> {
    if manifest.schema != MANIFEST_SCHEMA {
        return invalid_manifest(format!("schema must be {MANIFEST_SCHEMA}"));
    }
    if manifest.candidate_scope != "production" {
        return invalid_manifest("candidate scope must be production");
    }
    if manifest.decision_policy != "all_mandatory_pass" {
        return invalid_manifest("decision policy must require every mandatory pass");
    }
    if manifest.owner != OWNER {
        return invalid_manifest(format!("owner must be {OWNER}"));
    }
    let ids = manifest
        .requirements
        .iter()
        .map(|requirement| requirement.id.as_str())
        .collect::<Vec<_>>();
    if ids != EXPECTED_REQUIREMENTS {
        return invalid_manifest("readiness inventory must be sorted and complete");
    }

    for requirement in &manifest.requirements {
        validate_token(&requirement.id)?;
        validate_token(&requirement.category)?;
        if !requirement.mandatory {
            return invalid_manifest(format!(
                "{} must remain mandatory for production scope",
                requirement.id
            ));
        }
        validate_evidence(&requirement.id, &requirement.evidence)?;
        match requirement.status {
            EvidenceStatus::Passed => {
                if requirement.verification == Verification::None
                    || requirement.evidence.is_empty()
                    || requirement.reason.is_some()
                    || requirement.next_experiment.is_some()
                {
                    return invalid_manifest(format!(
                        "{} passed status lacks executable evidence",
                        requirement.id
                    ));
                }
            }
            EvidenceStatus::Failed | EvidenceStatus::Unavailable | EvidenceStatus::Unmeasured => {
                if requirement.verification != Verification::None {
                    return invalid_manifest(format!(
                        "{} non-passing status cannot use a passing verifier",
                        requirement.id
                    ));
                }
                let reason = requirement.reason.as_deref().ok_or_else(|| {
                    ReadinessError::InvalidManifest(format!(
                        "{} must declare a reason",
                        requirement.id
                    ))
                })?;
                let next = requirement.next_experiment.as_deref().ok_or_else(|| {
                    ReadinessError::InvalidManifest(format!(
                        "{} must declare the next deciding experiment",
                        requirement.id
                    ))
                })?;
                validate_token(reason)?;
                validate_token(next)?;
                if requirement.status == EvidenceStatus::Failed && requirement.evidence.is_empty() {
                    return invalid_manifest(format!(
                        "{} failed status must retain evidence",
                        requirement.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_evidence(id: &str, evidence: &[String]) -> Result<(), ReadinessError> {
    if evidence.len() > 16 {
        return invalid_manifest(format!("{id} has too many evidence references"));
    }
    let mut prior = None;
    for reference in evidence {
        if reference.is_empty()
            || reference.len() > 160
            || reference.contains("..")
            || reference.contains('\\')
            || !reference.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':')
            })
        {
            return invalid_manifest(format!("{id} has an invalid evidence reference"));
        }
        if prior.is_some_and(|candidate: &String| candidate >= reference) {
            return invalid_manifest(format!(
                "{id} evidence references must be sorted and unique"
            ));
        }
        prior = Some(reference);
    }
    Ok(())
}

fn verify_passed_contracts(manifest: &ReadinessManifest) -> Result<(), ReadinessError> {
    let mut observed = BTreeSet::new();
    for requirement in &manifest.requirements {
        if requirement.status != EvidenceStatus::Passed {
            continue;
        }
        if !observed.insert(requirement.verification) {
            return invalid_manifest(format!(
                "verifier {:?} is assigned more than once",
                requirement.verification
            ));
        }
        match requirement.verification {
            Verification::DatasetContract => datasets::check()?,
            Verification::IncidentContract => incident::verify_contract()?,
            Verification::PackageContract => {
                package::check()?;
                package::verify_ownership_contract()?;
            }
            Verification::None => {
                return invalid_manifest("passed requirement cannot omit verification");
            }
        }
    }
    Ok(())
}

fn decide(requirements: &[Requirement]) -> Decision {
    if requirements
        .iter()
        .all(|requirement| !requirement.mandatory || requirement.status == EvidenceStatus::Passed)
    {
        Decision::Ready
    } else {
        Decision::Blocked
    }
}

fn build_report<'a>(
    manifest: &'a ReadinessManifest,
    manifest_bytes: &[u8],
    source_revision: &'a str,
    decision: Decision,
) -> ReadinessReport<'a> {
    let blockers = manifest
        .requirements
        .iter()
        .filter(|requirement| requirement.mandatory && requirement.status != EvidenceStatus::Passed)
        .map(|requirement| Blocker {
            id: &requirement.id,
            category: &requirement.category,
            status: requirement.status,
            evidence: &requirement.evidence,
            reason: requirement.reason.as_deref().unwrap_or_default(),
            next_experiment: requirement.next_experiment.as_deref().unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let counts = StatusCounts {
        passed: count_status(&manifest.requirements, EvidenceStatus::Passed),
        failed: count_status(&manifest.requirements, EvidenceStatus::Failed),
        unavailable: count_status(&manifest.requirements, EvidenceStatus::Unavailable),
        unmeasured: count_status(&manifest.requirements, EvidenceStatus::Unmeasured),
    };
    ReadinessReport {
        schema: REPORT_SCHEMA,
        source_revision,
        manifest_sha256: sha256(manifest_bytes),
        candidate_scope: &manifest.candidate_scope,
        decision_policy: &manifest.decision_policy,
        owner: &manifest.owner,
        decision,
        counts,
        requirements: &manifest.requirements,
        blockers,
    }
}

fn count_status(requirements: &[Requirement], status: EvidenceStatus) -> usize {
    requirements
        .iter()
        .filter(|requirement| requirement.status == status)
        .count()
}

fn validate_token(value: &str) -> Result<(), ReadinessError> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return invalid_manifest(format!("invalid readiness token {value:?}"));
    }
    Ok(())
}

fn validate_source_revision(value: &str) -> Result<(), ReadinessError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ReadinessError::InvalidSourceRevision);
    }
    Ok(())
}

fn persist_new_file(path: &Path, bytes: &[u8]) -> Result<(), ReadinessError> {
    if path.exists() {
        return Err(ReadinessError::OutputExists(path.to_path_buf()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ReadinessError::InvalidManifest("report output has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|error| ReadinessError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| ReadinessError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return invalid_manifest("report parent must be a non-symlink directory");
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| ReadinessError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| ReadinessError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| ReadinessError::FileIo {
            path: path.to_path_buf(),
            error: error.error,
        })?;
    Ok(())
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, ReadinessError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| ReadinessError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return invalid_manifest(format!(
            "{} must be a regular non-symlink file",
            path.display()
        ));
    }
    if metadata.len() > maximum {
        return Err(ReadinessError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    let file = File::open(path).map_err(|error| ReadinessError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ReadinessError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(ReadinessError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

fn invalid_manifest<T>(detail: impl Into<String>) -> Result<T, ReadinessError> {
    Err(ReadinessError::InvalidManifest(detail.into()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadinessManifest {
    schema: String,
    candidate_scope: String,
    decision_policy: String,
    owner: String,
    requirements: Vec<Requirement>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Requirement {
    id: String,
    category: String,
    mandatory: bool,
    status: EvidenceStatus,
    verification: Verification,
    #[serde(default)]
    evidence: Vec<String>,
    reason: Option<String>,
    next_experiment: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceStatus {
    Passed,
    Failed,
    Unavailable,
    Unmeasured,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verification {
    DatasetContract,
    IncidentContract,
    PackageContract,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Ready,
    Blocked,
}

impl Decision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Serialize)]
struct ReadinessReport<'a> {
    schema: &'static str,
    source_revision: &'a str,
    manifest_sha256: String,
    candidate_scope: &'a str,
    decision_policy: &'a str,
    owner: &'a str,
    decision: Decision,
    counts: StatusCounts,
    requirements: &'a [Requirement],
    blockers: Vec<Blocker<'a>>,
}

#[derive(Debug, Serialize)]
struct StatusCounts {
    passed: usize,
    failed: usize,
    unavailable: usize,
    unmeasured: usize,
}

#[derive(Debug, Serialize)]
struct Blocker<'a> {
    id: &'a str,
    category: &'a str,
    status: EvidenceStatus,
    evidence: &'a [String],
    reason: &'a str,
    next_experiment: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadinessError {
    #[error("failed to determine the working directory")]
    WorkingDir(#[source] std::io::Error),
    #[error("readiness file {path} could not be accessed")]
    FileIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("readiness file {path} exceeds {maximum} bytes")]
    InputTooLarge { path: PathBuf, maximum: u64 },
    #[error("readiness manifest must be UTF-8")]
    InvalidUtf8(#[source] std::str::Utf8Error),
    #[error("readiness manifest is not valid TOML")]
    ParseManifest(#[source] toml::de::Error),
    #[error("readiness manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("readiness report serialization failed")]
    SerializeReport(#[source] serde_json::Error),
    #[error("source revision must be a canonical 40- or 64-character lowercase hex digest")]
    InvalidSourceRevision,
    #[error("required argument {0} is missing")]
    MissingRequiredFlag(&'static str),
    #[error("argument {0} requires a value")]
    MissingFlagValue(String),
    #[error("argument {0} was provided more than once")]
    DuplicateFlag(&'static str),
    #[error("unexpected argument: {0}")]
    UnexpectedArgument(String),
    #[error("immutable readiness output already exists: {0}")]
    OutputExists(PathBuf),
    #[error(transparent)]
    Dataset(#[from] datasets::DatasetError),
    #[error(transparent)]
    Incident(#[from] incident::IncidentError),
    #[error(transparent)]
    Package(#[from] package::PackageError),
    #[error("production release remains blocked by {blockers} mandatory evidence rows")]
    ReleaseBlocked { blockers: usize },
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn manifest_preserves_missing_evidence_and_blocks_release() {
        let workspace = workspace_root().expect("workspace root");
        let bytes = read_regular_bounded(&workspace.join(MANIFEST_PATH), MAX_MANIFEST_BYTES)
            .expect("manifest reads");
        let manifest = parse_manifest(&bytes).expect("manifest parses");

        validate_manifest(&manifest).expect("manifest validates");
        verify_passed_contracts(&manifest).expect("passed contracts verify");
        assert_eq!(decide(&manifest.requirements), Decision::Blocked);
        assert!(
            manifest
                .requirements
                .iter()
                .any(|requirement| { requirement.status == EvidenceStatus::Unavailable })
        );
        assert!(
            manifest
                .requirements
                .iter()
                .any(|requirement| { requirement.status == EvidenceStatus::Unmeasured })
        );
    }

    #[test]
    fn decision_requires_every_mandatory_row() {
        let requirements = vec![
            requirement("first", EvidenceStatus::Passed, true),
            requirement("second", EvidenceStatus::Unmeasured, true),
        ];
        assert_eq!(decide(&requirements), Decision::Blocked);

        let ready = vec![
            requirement("first", EvidenceStatus::Passed, true),
            requirement("second", EvidenceStatus::Passed, true),
        ];
        assert_eq!(decide(&ready), Decision::Ready);
    }

    #[test]
    fn report_is_deterministic_and_fail_closed() {
        let workspace = workspace_root().expect("workspace root");
        let bytes = read_regular_bounded(&workspace.join(MANIFEST_PATH), MAX_MANIFEST_BYTES)
            .expect("manifest reads");
        let manifest = parse_manifest(&bytes).expect("manifest parses");
        let revision = "1111111111111111111111111111111111111111";
        let decision = decide(&manifest.requirements);

        let first = serde_json::to_vec(&build_report(&manifest, &bytes, revision, decision))
            .expect("first report");
        let second = serde_json::to_vec(&build_report(&manifest, &bytes, revision, decision))
            .expect("second report");

        assert_eq!(first, second);
        assert!(
            std::str::from_utf8(&first)
                .expect("report is UTF-8")
                .contains("\"decision\":\"blocked\"")
        );
    }

    #[test]
    fn strict_mode_writes_evidence_before_rejecting_release() {
        let output = tempdir().expect("output");
        let path = output.path().join("readiness.json");
        let options = Options {
            output: path.clone(),
            source_revision: "1111111111111111111111111111111111111111".to_owned(),
            require_ready: true,
        };

        assert!(matches!(
            evaluate(&options),
            Err(ReadinessError::ReleaseBlocked { .. })
        ));
        assert!(path.is_file());
    }

    fn requirement(id: &str, status: EvidenceStatus, mandatory: bool) -> Requirement {
        Requirement {
            id: id.to_owned(),
            category: "test".to_owned(),
            mandatory,
            status,
            verification: Verification::None,
            evidence: Vec::new(),
            reason: None,
            next_experiment: None,
        }
    }
}
