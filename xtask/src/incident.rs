//! Deterministic, source-free incident tabletop evidence.
//!
//! The validator binds operational guidance to exact structured scenarios and
//! fails when a required containment or recovery control disappears.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

const TABLETOP_PATH: &str = "operations/tabletop.toml";
const RUNBOOK_PATH: &str = "operations/INCIDENT_RESPONSE.md";
const TABLETOP_SCHEMA: &str = "rootlight.incident-tabletop/1";
const REPORT_SCHEMA: &str = "rootlight.incident-tabletop-report/1";
const OWNER: &str = "@tomasmarekk";
const MAX_TABLETOP_BYTES: u64 = 256 * 1024;
const MAX_RUNBOOK_BYTES: u64 = 512 * 1024;
const EXPECTED_SCENARIOS: [&str; 6] = [
    "bad-update",
    "corrupt-catalog",
    "sandbox-escape",
    "service-unavailable",
    "signing-key-compromise",
    "source-exfiltration",
];

#[derive(Debug)]
pub(crate) struct Options {
    output: PathBuf,
    source_revision: String,
}

impl Options {
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, IncidentError> {
        let mut output = None;
        let mut source_revision = None;
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| IncidentError::MissingFlagValue(flag.clone()))?;
            match flag.as_str() {
                "--output" => assign_once(&mut output, PathBuf::from(value), "--output")?,
                "--source-revision" => {
                    assign_once(&mut source_revision, value, "--source-revision")?;
                }
                _ => return Err(IncidentError::UnexpectedArgument(flag)),
            }
        }
        Ok(Self {
            output: output.ok_or(IncidentError::MissingRequiredFlag("--output"))?,
            source_revision: source_revision
                .ok_or(IncidentError::MissingRequiredFlag("--source-revision"))?,
        })
    }
}

pub(crate) fn exercise(options: &Options) -> Result<(), IncidentError> {
    validate_source_revision(&options.source_revision)?;
    let workspace = workspace_root()?;
    let tabletop_bytes = read_regular_bounded(&workspace.join(TABLETOP_PATH), MAX_TABLETOP_BYTES)?;
    let runbook_bytes = read_regular_bounded(&workspace.join(RUNBOOK_PATH), MAX_RUNBOOK_BYTES)?;
    let tabletop = parse_tabletop(&tabletop_bytes)?;
    validate_tabletop(&tabletop)?;
    validate_runbook(&runbook_bytes)?;
    let report = build_report(
        &tabletop,
        &tabletop_bytes,
        &runbook_bytes,
        &options.source_revision,
    );
    let mut bytes = serde_json::to_vec_pretty(&report).map_err(IncidentError::SerializeReport)?;
    bytes.push(b'\n');
    persist_new_file(&options.output, &bytes)?;
    println!(
        "incident tabletop passed for {} source-free scenarios",
        report.scenarios.len()
    );
    Ok(())
}

pub(crate) fn verify_contract() -> Result<(), IncidentError> {
    let workspace = workspace_root()?;
    let tabletop_bytes = read_regular_bounded(&workspace.join(TABLETOP_PATH), MAX_TABLETOP_BYTES)?;
    let runbook_bytes = read_regular_bounded(&workspace.join(RUNBOOK_PATH), MAX_RUNBOOK_BYTES)?;
    let tabletop = parse_tabletop(&tabletop_bytes)?;
    validate_tabletop(&tabletop)?;
    validate_runbook(&runbook_bytes)
}

fn assign_once<T>(slot: &mut Option<T>, value: T, flag: &'static str) -> Result<(), IncidentError> {
    if slot.replace(value).is_some() {
        return Err(IncidentError::DuplicateFlag(flag));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, IncidentError> {
    let mut candidate = std::env::current_dir().map_err(IncidentError::WorkingDir)?;
    for _ in 0..8 {
        if candidate.join("Cargo.toml").is_file()
            && candidate.join(TABLETOP_PATH).is_file()
            && candidate.join(RUNBOOK_PATH).is_file()
        {
            return Ok(candidate);
        }
        if !candidate.pop() {
            break;
        }
    }
    Err(IncidentError::InvalidTabletop(
        "run incident tooling from within the workspace".to_owned(),
    ))
}

fn parse_tabletop(bytes: &[u8]) -> Result<Tabletop, IncidentError> {
    let text = std::str::from_utf8(bytes).map_err(IncidentError::InvalidUtf8)?;
    toml::from_str(text).map_err(IncidentError::ParseTabletop)
}

fn validate_tabletop(tabletop: &Tabletop) -> Result<(), IncidentError> {
    if tabletop.schema != TABLETOP_SCHEMA {
        return invalid_tabletop(format!("schema must be {TABLETOP_SCHEMA}"));
    }
    if tabletop.owner != OWNER {
        return invalid_tabletop(format!("owner must be {OWNER}"));
    }
    if tabletop.exercise_mode != "deterministic_control_walkthrough" {
        return invalid_tabletop("exercise mode must describe a deterministic control walkthrough");
    }
    if tabletop.privacy != "source_free" {
        return invalid_tabletop("tabletop evidence must be source-free");
    }
    let observed_ids = tabletop
        .scenarios
        .iter()
        .map(|scenario| scenario.id.as_str())
        .collect::<Vec<_>>();
    if observed_ids != EXPECTED_SCENARIOS {
        return invalid_tabletop("scenario inventory must be sorted and complete");
    }

    let required = required_controls();
    for scenario in &tabletop.scenarios {
        validate_token(&scenario.id)?;
        if !matches!(scenario.severity.as_str(), "critical" | "high" | "medium") {
            return invalid_tabletop(format!("{} has an unsupported severity", scenario.id));
        }
        let expected_rotation = scenario.id == "signing-key-compromise";
        if scenario.key_rotation != expected_rotation {
            return invalid_tabletop(format!(
                "{} has an invalid key-rotation decision",
                scenario.id
            ));
        }
        for (field, values) in [
            ("detection", &scenario.detection),
            ("evidence", &scenario.evidence),
            ("containment", &scenario.containment),
            ("recovery", &scenario.recovery),
            ("communication", &scenario.communication),
            ("post_incident", &scenario.post_incident),
            ("controls", &scenario.controls),
        ] {
            validate_sorted_tokens(&scenario.id, field, values)?;
        }
        let expected = required
            .get(scenario.id.as_str())
            .ok_or_else(|| IncidentError::InvalidTabletop("unknown scenario".to_owned()))?;
        if scenario.controls.as_slice() != *expected {
            return invalid_tabletop(format!("{} required controls differ", scenario.id));
        }
        let declared = scenario
            .containment
            .iter()
            .chain(&scenario.recovery)
            .chain(&scenario.communication)
            .collect::<BTreeSet<_>>();
        if scenario
            .controls
            .iter()
            .any(|control| !declared.contains(control))
        {
            return invalid_tabletop(format!(
                "{} controls must be exercised by containment, recovery, or communication",
                scenario.id
            ));
        }
    }
    Ok(())
}

fn validate_sorted_tokens(
    scenario: &str,
    field: &str,
    values: &[String],
) -> Result<(), IncidentError> {
    if values.is_empty() || values.len() > 16 {
        return invalid_tabletop(format!(
            "{scenario} {field} must contain 1 through 16 tokens"
        ));
    }
    let mut prior = None;
    for value in values {
        validate_token(value)?;
        if prior.is_some_and(|candidate: &String| candidate >= value) {
            return invalid_tabletop(format!("{scenario} {field} must be sorted and unique"));
        }
        prior = Some(value);
    }
    Ok(())
}

fn validate_token(value: &str) -> Result<(), IncidentError> {
    if value.is_empty()
        || value.len() > 80
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        return invalid_tabletop(format!("invalid evidence token {value:?}"));
    }
    Ok(())
}

fn validate_runbook(bytes: &[u8]) -> Result<(), IncidentError> {
    let runbook = std::str::from_utf8(bytes).map_err(IncidentError::InvalidUtf8)?;
    for required in [
        "Owner: `@tomasmarekk`",
        "## Detection and triage",
        "## Evidence preservation",
        "## Containment",
        "## Recovery and verification",
        "## Communication",
        "### Source exfiltration",
        "### Sandbox escape",
        "### Corrupt catalog",
        "### Bad update",
        "### Signing-key compromise",
        "### Service unavailable",
        "## Post-incident actions",
    ] {
        if !runbook.contains(required) {
            return Err(IncidentError::InvalidRunbook(required.to_owned()));
        }
    }
    Ok(())
}

fn build_report<'a>(
    tabletop: &'a Tabletop,
    tabletop_bytes: &[u8],
    runbook_bytes: &[u8],
    source_revision: &'a str,
) -> TabletopReport<'a> {
    TabletopReport {
        schema: REPORT_SCHEMA,
        source_revision,
        exercise_mode: &tabletop.exercise_mode,
        privacy: &tabletop.privacy,
        owner: &tabletop.owner,
        tabletop_sha256: sha256(tabletop_bytes),
        runbook_sha256: sha256(runbook_bytes),
        scenarios: tabletop
            .scenarios
            .iter()
            .map(|scenario| ScenarioReport {
                id: &scenario.id,
                severity: &scenario.severity,
                outcome: "passed",
                key_rotation_exercised: scenario.key_rotation,
                controls: &scenario.controls,
            })
            .collect(),
    }
}

fn required_controls() -> BTreeMap<&'static str, &'static [&'static str]> {
    BTreeMap::from([
        (
            "bad-update",
            &[
                "restore_last_good_binary",
                "revoke_metadata",
                "stop_rollout",
                "verify_catalog_compatibility",
            ][..],
        ),
        (
            "corrupt-catalog",
            &[
                "preserve_last_good_generation",
                "quarantine_corrupt_state",
                "rebuild_before_activation",
                "run_repair_dry_run",
            ][..],
        ),
        (
            "sandbox-escape",
            &[
                "audit_host_boundary",
                "deny_egress",
                "revoke_adapter_trust",
                "terminate_adapter_process_tree",
            ][..],
        ),
        (
            "service-unavailable",
            &[
                "collect_source_free_diagnostics",
                "preserve_local_data",
                "restore_per_user_daemon",
                "use_standalone_fallback",
            ][..],
        ),
        (
            "signing-key-compromise",
            &[
                "reverify_published_artifacts",
                "revoke_signing_key",
                "rotate_trust_metadata",
                "stop_release_publication",
            ][..],
        ),
        (
            "source-exfiltration",
            &[
                "disable_suspected_output_path",
                "notify_affected_users",
                "preserve_redacted_evidence",
                "verify_no_source_in_support_bundle",
            ][..],
        ),
    ])
}

fn validate_source_revision(value: &str) -> Result<(), IncidentError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(IncidentError::InvalidSourceRevision);
    }
    Ok(())
}

fn persist_new_file(path: &Path, bytes: &[u8]) -> Result<(), IncidentError> {
    if path.exists() {
        return Err(IncidentError::OutputExists(path.to_path_buf()));
    }
    let parent = path.parent().ok_or_else(|| {
        IncidentError::InvalidTabletop("output has no parent directory".to_owned())
    })?;
    fs::create_dir_all(parent).map_err(|error| IncidentError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| IncidentError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(IncidentError::InvalidTabletop(
            "output parent must be a non-symlink directory".to_owned(),
        ));
    }
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| IncidentError::FileIo {
        path: parent.to_path_buf(),
        error,
    })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| IncidentError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| IncidentError::FileIo {
            path: path.to_path_buf(),
            error: error.error,
        })?;
    Ok(())
}

fn read_regular_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, IncidentError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| IncidentError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(IncidentError::InvalidTabletop(format!(
            "{} must be a regular non-symlink file",
            path.display()
        )));
    }
    if metadata.len() > maximum {
        return Err(IncidentError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    let file = File::open(path).map_err(|error| IncidentError::FileIo {
        path: path.to_path_buf(),
        error,
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| IncidentError::FileIo {
            path: path.to_path_buf(),
            error,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(IncidentError::InputTooLarge {
            path: path.to_path_buf(),
            maximum,
        });
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(bytes))
}

fn invalid_tabletop<T>(detail: impl Into<String>) -> Result<T, IncidentError> {
    Err(IncidentError::InvalidTabletop(detail.into()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tabletop {
    schema: String,
    owner: String,
    exercise_mode: String,
    privacy: String,
    scenarios: Vec<Scenario>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    id: String,
    severity: String,
    key_rotation: bool,
    detection: Vec<String>,
    evidence: Vec<String>,
    containment: Vec<String>,
    recovery: Vec<String>,
    communication: Vec<String>,
    post_incident: Vec<String>,
    controls: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TabletopReport<'a> {
    schema: &'static str,
    source_revision: &'a str,
    exercise_mode: &'a str,
    privacy: &'a str,
    owner: &'a str,
    tabletop_sha256: String,
    runbook_sha256: String,
    scenarios: Vec<ScenarioReport<'a>>,
}

#[derive(Debug, Serialize)]
struct ScenarioReport<'a> {
    id: &'a str,
    severity: &'a str,
    outcome: &'static str,
    key_rotation_exercised: bool,
    controls: &'a [String],
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IncidentError {
    #[error("failed to determine the working directory")]
    WorkingDir(#[source] std::io::Error),
    #[error("incident evidence file {path} could not be accessed")]
    FileIo {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("incident evidence file {path} exceeds {maximum} bytes")]
    InputTooLarge { path: PathBuf, maximum: u64 },
    #[error("incident evidence input must be UTF-8")]
    InvalidUtf8(#[source] std::str::Utf8Error),
    #[error("incident tabletop is not valid TOML")]
    ParseTabletop(#[source] toml::de::Error),
    #[error("incident tabletop is invalid: {0}")]
    InvalidTabletop(String),
    #[error("incident runbook is missing required section {0}")]
    InvalidRunbook(String),
    #[error("incident tabletop report serialization failed")]
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
    #[error("immutable incident evidence output already exists: {0}")]
    OutputExists(PathBuf),
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn tabletop_is_complete_and_source_free() {
        let workspace = workspace_root().expect("workspace root");
        let bytes = read_regular_bounded(&workspace.join(TABLETOP_PATH), MAX_TABLETOP_BYTES)
            .expect("tabletop reads");
        let tabletop = parse_tabletop(&bytes).expect("tabletop parses");

        validate_tabletop(&tabletop).expect("tabletop validates");
        assert_eq!(tabletop.scenarios.len(), EXPECTED_SCENARIOS.len());
    }

    #[test]
    fn tabletop_report_is_deterministic_and_bound() {
        let workspace = workspace_root().expect("workspace root");
        let tabletop_bytes =
            read_regular_bounded(&workspace.join(TABLETOP_PATH), MAX_TABLETOP_BYTES)
                .expect("tabletop reads");
        let runbook_bytes = read_regular_bounded(&workspace.join(RUNBOOK_PATH), MAX_RUNBOOK_BYTES)
            .expect("runbook reads");
        let tabletop = parse_tabletop(&tabletop_bytes).expect("tabletop parses");
        let revision = "1111111111111111111111111111111111111111";

        let first = serde_json::to_vec(&build_report(
            &tabletop,
            &tabletop_bytes,
            &runbook_bytes,
            revision,
        ))
        .expect("first report");
        let second = serde_json::to_vec(&build_report(
            &tabletop,
            &tabletop_bytes,
            &runbook_bytes,
            revision,
        ))
        .expect("second report");

        assert_eq!(first, second);
        let text = std::str::from_utf8(&first).expect("report is UTF-8");
        for forbidden in [
            "credential",
            "private_key",
            "repository_content",
            "source_body",
        ] {
            assert!(!text.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn missing_control_and_unsorted_evidence_are_rejected() {
        let workspace = workspace_root().expect("workspace root");
        let bytes = read_regular_bounded(&workspace.join(TABLETOP_PATH), MAX_TABLETOP_BYTES)
            .expect("tabletop reads");
        let tabletop = parse_tabletop(&bytes).expect("tabletop parses");

        let mut missing = tabletop.scenarios[0].clone();
        missing.controls.pop();
        let mut changed = tabletop;
        changed.scenarios[0] = missing;
        assert!(validate_tabletop(&changed).is_err());

        let mut unsorted = changed.scenarios[1].clone();
        unsorted.evidence.swap(0, 1);
        changed.scenarios[0] = parse_tabletop(&bytes)
            .expect("tabletop reparses")
            .scenarios
            .remove(0);
        changed.scenarios[1] = unsorted;
        assert!(validate_tabletop(&changed).is_err());
    }

    #[test]
    fn exercise_writes_one_immutable_report() {
        let output = tempdir().expect("output");
        let path = output.path().join("tabletop.json");
        let options = Options {
            output: path.clone(),
            source_revision: "1111111111111111111111111111111111111111".to_owned(),
        };

        exercise(&options).expect("exercise passes");
        assert!(path.is_file());
        assert!(matches!(
            exercise(&options),
            Err(IncidentError::OutputExists(_))
        ));
    }
}
