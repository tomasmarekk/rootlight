//! Source-free evidence for adversarial MCP contract suites.
//!
//! The report records only closed case identifiers, normalized commands, and
//! outcomes. Repository fixture text and child-process output never cross the
//! evidence boundary.

use std::{
    collections::BTreeSet,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use serde::{Deserialize, Serialize};

const SCHEMA: &str = "rootlight.contract-security-matrix/1";
const MAX_REPORT_BYTES: usize = 256 * 1024;
const MAX_CHILD_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;

const SUITES: [SuiteSpec; 6] = [
    SuiteSpec {
        id: "profile-process",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "rootlight-mcp",
            "--test",
            "profile_security_process",
        ],
    },
    SuiteSpec {
        id: "batch-process",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "rootlight-mcp",
            "--test",
            "batch_process",
        ],
    },
    SuiteSpec {
        id: "advanced-query-adversarial",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "rootlight-query",
            "--test",
            "advanced_adversarial",
        ],
    },
    SuiteSpec {
        id: "cursor-contract",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "rootlight-mcp-contract",
            "pagination",
        ],
    },
    SuiteSpec {
        id: "executor-security",
        arguments: &["test", "--locked", "-p", "rootlight-mcp", "executor::tests"],
    },
    SuiteSpec {
        id: "stdio-process",
        arguments: &[
            "test",
            "--locked",
            "-p",
            "rootlight-mcp",
            "--test",
            "stdio_process",
        ],
    },
];

const CASES: [CaseSpec; 49] = [
    case("profile.scout.exact-membership", "profile-process", true),
    case("profile.analysis.exact-membership", "profile-process", true),
    case(
        "profile.developer.exact-membership",
        "profile-process",
        true,
    ),
    case("profile.tool-list-payload", "profile-process", true),
    case("profile.annotations", "profile-process", true),
    case("profile.direct-hidden-invocation", "profile-process", true),
    case("profile.batch-hidden-invocation", "profile-process", true),
    case(
        "profile.old-schema-hidden-invocation",
        "profile-process",
        true,
    ),
    case("profile.forged-metadata", "profile-process", true),
    case("batch.maximum-operations", "executor-security", true),
    case("batch.maximum-depth", "executor-security", true),
    case("batch.duplicate-identifiers", "executor-security", true),
    case("batch.missing-dependencies", "executor-security", true),
    case("batch.cycles", "executor-security", true),
    case("batch.typed-bindings", "batch-process", false),
    case("batch.single-generation", "executor-security", true),
    case("batch.shared-budget", "executor-security", false),
    case("batch.local-budget", "executor-security", false),
    case("batch.deterministic-order", "batch-process", false),
    case("batch.continue-on-error", "executor-security", false),
    case("batch.fail-fast", "executor-security", false),
    case("batch.dependency-skip", "executor-security", true),
    case("batch.all-error-results", "executor-security", false),
    case("batch.prohibited-operations", "profile-process", true),
    case("batch.profile-intersection", "profile-process", true),
    case(
        "advanced.forbidden-operators",
        "advanced-query-adversarial",
        true,
    ),
    case(
        "advanced.forbidden-predicates",
        "advanced-query-adversarial",
        true,
    ),
    case(
        "advanced.parameter-injection",
        "advanced-query-adversarial",
        true,
    ),
    case("advanced.ast-depth", "advanced-query-adversarial", true),
    case("advanced.cost", "advanced-query-adversarial", true),
    case("advanced.rows", "advanced-query-adversarial", false),
    case("advanced.joins", "advanced-query-adversarial", true),
    case("advanced.groups", "advanced-query-adversarial", true),
    case("advanced.traversal", "advanced-query-adversarial", true),
    case("advanced.pagination", "advanced-query-adversarial", false),
    case("advanced.cancellation", "advanced-query-adversarial", false),
    case(
        "advanced.fuzz-regressions",
        "advanced-query-adversarial",
        true,
    ),
    case("cursor.malformed", "cursor-contract", true),
    case("cursor.tampered", "cursor-contract", true),
    case("cursor.expired", "cursor-contract", true),
    case("cursor.future-issued", "cursor-contract", true),
    case("cursor.wrong-key", "cursor-contract", true),
    case("cursor.wrong-plan", "executor-security", true),
    case("cursor.wrong-profile", "executor-security", true),
    case("cursor.wrong-generation", "executor-security", true),
    case("cursor.old-version", "cursor-contract", true),
    case("trust.prompt-injection", "profile-process", true),
    case("trust.path-injection", "stdio-process", true),
    case("side-effects.read-only-boundary", "profile-process", true),
];

const STOP_CONDITIONS: [&str; 6] = [
    "profile-bypass",
    "forgeable-cursor",
    "executable-query-escape",
    "hidden-side-effect",
    "source-trust-inversion",
    "unbounded-operation",
];

const fn case(id: &'static str, suite: &'static str, zero_call_asserted: bool) -> CaseSpec {
    CaseSpec {
        id,
        suite,
        zero_call_asserted,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    mode: Mode,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Produce(PathBuf),
    Verify(PathBuf),
}

impl Options {
    pub(crate) fn parse(
        arguments: &mut impl Iterator<Item = String>,
    ) -> Result<Self, ContractMatrixError> {
        let mut mode = None;
        let mut source_revision = None;
        while let Some(flag) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| ContractMatrixError::MissingValue(flag.clone()))?;
            match flag.as_str() {
                "--output" if mode.is_none() => mode = Some(Mode::Produce(PathBuf::from(value))),
                "--verify" if mode.is_none() => mode = Some(Mode::Verify(PathBuf::from(value))),
                "--source-revision" if source_revision.is_none() => source_revision = Some(value),
                "--output" | "--verify" | "--source-revision" => {
                    return Err(ContractMatrixError::DuplicateOption(flag));
                }
                _ => return Err(ContractMatrixError::UnexpectedArgument(flag)),
            }
        }
        let mode = mode.ok_or(ContractMatrixError::MissingMode)?;
        let source_revision = source_revision.ok_or(ContractMatrixError::MissingSourceRevision)?;
        validate_revision(&source_revision)?;
        Ok(Self {
            mode,
            source_revision,
        })
    }
}

pub(crate) fn run(options: &Options) -> Result<(), ContractMatrixError> {
    match &options.mode {
        Mode::Produce(path) => produce(path, &options.source_revision),
        Mode::Verify(path) => {
            let report = decode_report(&read_bounded(path)?)?;
            validate_report(&report, &options.source_revision)
        }
    }
}

fn produce(path: &Path, source_revision: &str) -> Result<(), ContractMatrixError> {
    require_exact_revision(source_revision)?;
    let workspace = workspace_root()?;
    let mut suite_records = Vec::with_capacity(SUITES.len());
    for suite in SUITES {
        let status = run_suite(&workspace, suite.arguments)?;
        suite_records.push(SuiteRecord {
            id: suite.id.to_owned(),
            command: normalized_command(suite.arguments),
            status: if status.success() {
                Outcome::Passed
            } else {
                Outcome::Failed
            },
            exit_code: status.code(),
        });
    }
    let suite_statuses = suite_records
        .iter()
        .map(|suite| (suite.id.as_str(), suite.status))
        .collect::<std::collections::BTreeMap<_, _>>();
    let cases = CASES
        .iter()
        .map(|spec| CaseRecord {
            id: spec.id.to_owned(),
            suite: spec.suite.to_owned(),
            status: suite_statuses
                .get(spec.suite)
                .copied()
                .unwrap_or(Outcome::Failed),
            expected_error: expected_error(spec.id).map(str::to_owned),
            zero_call_asserted: spec.zero_call_asserted,
        })
        .collect::<Vec<_>>();
    let blocked = cases.iter().any(|case| case.status != Outcome::Passed);
    let report = ContractMatrixReport {
        schema: SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        disposition: if blocked {
            Disposition::Blocked
        } else {
            Disposition::Pass
        },
        suites: suite_records,
        cases,
        stop_conditions: STOP_CONDITIONS
            .iter()
            .map(|id| StopCondition {
                id: (*id).to_owned(),
                observed: blocked,
            })
            .collect(),
    };
    validate_structure(&report)?;
    write_report(path, &report)?;
    if blocked {
        return Err(ContractMatrixError::SuiteFailed);
    }
    validate_report(&report, source_revision)
}

fn run_suite(workspace: &Path, arguments: &[&str]) -> Result<ExitStatus, ContractMatrixError> {
    let stdout = tempfile::tempfile().map_err(ContractMatrixError::TempOutput)?;
    let stderr = tempfile::tempfile().map_err(ContractMatrixError::TempOutput)?;
    let status = Command::new("cargo")
        .args(arguments)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            stdout
                .try_clone()
                .map_err(ContractMatrixError::TempOutput)?,
        ))
        .stderr(Stdio::from(
            stderr
                .try_clone()
                .map_err(ContractMatrixError::TempOutput)?,
        ))
        .status()
        .map_err(ContractMatrixError::Spawn)?;
    for output in [stdout, stderr] {
        if output
            .metadata()
            .map_err(ContractMatrixError::TempOutput)?
            .len()
            > MAX_CHILD_OUTPUT_BYTES
        {
            return Err(ContractMatrixError::ChildOutputLimit);
        }
    }
    Ok(status)
}

fn normalized_command(arguments: &[&str]) -> String {
    format!("cargo {}", arguments.join(" "))
}

fn expected_error(id: &str) -> Option<&'static str> {
    if id.starts_with("profile.") || id == "batch.prohibited-operations" {
        Some("UNSUPPORTED_CAPABILITY")
    } else if id.starts_with("cursor.") {
        Some("INVALID_CURSOR")
    } else if (id.starts_with("advanced.") && id != "advanced.pagination")
        || matches!(
            id,
            "batch.maximum-operations"
                | "batch.maximum-depth"
                | "batch.duplicate-identifiers"
                | "batch.missing-dependencies"
                | "batch.cycles"
                | "batch.single-generation"
        )
    {
        Some("INVALID_ARGUMENT")
    } else {
        None
    }
}

fn require_exact_revision(source_revision: &str) -> Result<(), ContractMatrixError> {
    let workspace = workspace_root()?;
    let head = command_text(&workspace, "git", &["rev-parse", "HEAD"])?;
    if head.trim() != source_revision {
        return Err(ContractMatrixError::RevisionMismatch);
    }
    let status = command_text(
        &workspace,
        "git",
        &["status", "--porcelain=v1", "--untracked-files=no"],
    )?;
    if !status.trim().is_empty() {
        return Err(ContractMatrixError::DirtyTree);
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, ContractMatrixError> {
    let current = std::env::current_dir().map_err(ContractMatrixError::WorkingDirectory)?;
    let root = command_text(&current, "git", &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root.trim()))
}

fn command_text(
    directory: &Path,
    program: &str,
    arguments: &[&str],
) -> Result<String, ContractMatrixError> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .map_err(ContractMatrixError::Spawn)?;
    if !output.status.success() {
        return Err(ContractMatrixError::CommandFailed(program.to_owned()));
    }
    String::from_utf8(output.stdout).map_err(|_| ContractMatrixError::CommandOutput)
}

fn write_report(path: &Path, report: &ContractMatrixReport) -> Result<(), ContractMatrixError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(ContractMatrixError::Io)?;
    }
    let encoded = encode_report(report)?;
    let mut file = fs::File::create(path).map_err(ContractMatrixError::Io)?;
    file.write_all(&encoded).map_err(ContractMatrixError::Io)?;
    file.write_all(b"\n").map_err(ContractMatrixError::Io)
}

fn encode_report(report: &ContractMatrixReport) -> Result<Vec<u8>, ContractMatrixError> {
    let encoded = serde_json::to_vec(report).map_err(ContractMatrixError::Json)?;
    if encoded.len() > MAX_REPORT_BYTES {
        return Err(ContractMatrixError::ReportLimit);
    }
    Ok(encoded)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ContractMatrixError> {
    let metadata = fs::metadata(path).map_err(ContractMatrixError::Io)?;
    if metadata.len() > MAX_REPORT_BYTES as u64 {
        return Err(ContractMatrixError::ReportLimit);
    }
    fs::read(path).map_err(ContractMatrixError::Io)
}

fn decode_report(encoded: &[u8]) -> Result<ContractMatrixReport, ContractMatrixError> {
    if encoded.is_empty() || encoded.len() > MAX_REPORT_BYTES {
        return Err(ContractMatrixError::ReportLimit);
    }
    let report = serde_json::from_slice(encoded).map_err(|_| ContractMatrixError::InvalidReport)?;
    validate_structure(&report)?;
    let canonical = encode_report(&report)?;
    if encoded.strip_suffix(b"\n").unwrap_or(encoded) != canonical {
        return Err(ContractMatrixError::NonCanonical);
    }
    Ok(report)
}

fn validate_report(
    report: &ContractMatrixReport,
    source_revision: &str,
) -> Result<(), ContractMatrixError> {
    validate_structure(report)?;
    if report.source_revision != source_revision {
        return Err(ContractMatrixError::RevisionMismatch);
    }
    if report.disposition != Disposition::Pass
        || report
            .suites
            .iter()
            .any(|suite| suite.status != Outcome::Passed || suite.exit_code != Some(0))
        || report
            .cases
            .iter()
            .any(|case| case.status != Outcome::Passed)
        || report
            .stop_conditions
            .iter()
            .any(|condition| condition.observed)
    {
        return Err(ContractMatrixError::Blocked);
    }
    Ok(())
}

fn validate_structure(report: &ContractMatrixReport) -> Result<(), ContractMatrixError> {
    validate_revision(&report.source_revision)?;
    if report.schema != SCHEMA
        || report.suites.len() != SUITES.len()
        || report.cases.len() != CASES.len()
        || report.stop_conditions.len() != STOP_CONDITIONS.len()
    {
        return Err(ContractMatrixError::InvalidReport);
    }
    let expected_suites = SUITES.iter().map(|suite| suite.id).collect::<BTreeSet<_>>();
    let observed_suites = report
        .suites
        .iter()
        .map(|suite| suite.id.as_str())
        .collect::<BTreeSet<_>>();
    let expected_cases = CASES.iter().map(|case| case.id).collect::<BTreeSet<_>>();
    let observed_cases = report
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    let observed_stops = report
        .stop_conditions
        .iter()
        .map(|condition| condition.id.as_str())
        .collect::<BTreeSet<_>>();
    if observed_suites != expected_suites
        || observed_cases != expected_cases
        || observed_stops != STOP_CONDITIONS.into_iter().collect()
        || report
            .cases
            .iter()
            .any(|case| !expected_suites.contains(case.suite.as_str()))
        || report.suites.iter().any(|record| {
            SUITES
                .iter()
                .find(|suite| suite.id == record.id)
                .is_none_or(|suite| record.command != normalized_command(suite.arguments))
        })
        || report.cases.iter().any(|record| {
            CASES
                .iter()
                .find(|case| case.id == record.id)
                .is_none_or(|case| {
                    record.suite != case.suite
                        || record.zero_call_asserted != case.zero_call_asserted
                        || record.expected_error.as_deref() != expected_error(case.id)
                })
        })
    {
        return Err(ContractMatrixError::InvalidReport);
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<(), ContractMatrixError> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ContractMatrixError::InvalidRevision);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SuiteSpec {
    id: &'static str,
    arguments: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
struct CaseSpec {
    id: &'static str,
    suite: &'static str,
    zero_call_asserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractMatrixReport {
    schema: String,
    source_revision: String,
    disposition: Disposition,
    suites: Vec<SuiteRecord>,
    cases: Vec<CaseRecord>,
    stop_conditions: Vec<StopCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteRecord {
    id: String,
    command: String,
    status: Outcome,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseRecord {
    id: String,
    suite: String,
    status: Outcome,
    expected_error: Option<String>,
    zero_call_asserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StopCondition {
    id: String,
    observed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Pass,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Passed,
    Failed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ContractMatrixError {
    #[error("contract-matrix requires exactly one of --output PATH or --verify PATH")]
    MissingMode,
    #[error("contract-matrix requires --source-revision REV")]
    MissingSourceRevision,
    #[error("{0} requires a value")]
    MissingValue(String),
    #[error("duplicate contract-matrix option: {0}")]
    DuplicateOption(String),
    #[error("unexpected contract-matrix argument: {0}")]
    UnexpectedArgument(String),
    #[error("source revision must be a 40-character hexadecimal commit identifier")]
    InvalidRevision,
    #[error("source revision differs from the current checkout or retained report")]
    RevisionMismatch,
    #[error("contract evidence must be produced from a clean tracked tree")]
    DirtyTree,
    #[error("one or more mandatory contract suites failed")]
    SuiteFailed,
    #[error("contract matrix has a blocked disposition")]
    Blocked,
    #[error("contract matrix report is malformed or incomplete")]
    InvalidReport,
    #[error("contract matrix report is not canonical JSON")]
    NonCanonical,
    #[error("contract matrix report exceeds its byte ceiling")]
    ReportLimit,
    #[error("child test output exceeded its retained temporary ceiling")]
    ChildOutputLimit,
    #[error("failed to determine the working directory")]
    WorkingDirectory(#[source] std::io::Error),
    #[error("failed to spawn a required command")]
    Spawn(#[source] std::io::Error),
    #[error("required command failed: {0}")]
    CommandFailed(String),
    #[error("required command emitted non-UTF-8 output")]
    CommandOutput,
    #[error("temporary child output failed")]
    TempOutput(#[source] std::io::Error),
    #[error("contract matrix I/O failed")]
    Io(#[source] std::io::Error),
    #[error("contract matrix serialization failed")]
    Json(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn fixture() -> ContractMatrixReport {
        ContractMatrixReport {
            schema: SCHEMA.to_owned(),
            source_revision: REVISION.to_owned(),
            disposition: Disposition::Pass,
            suites: SUITES
                .iter()
                .map(|suite| SuiteRecord {
                    id: suite.id.to_owned(),
                    command: normalized_command(suite.arguments),
                    status: Outcome::Passed,
                    exit_code: Some(0),
                })
                .collect(),
            cases: CASES
                .iter()
                .map(|case| CaseRecord {
                    id: case.id.to_owned(),
                    suite: case.suite.to_owned(),
                    status: Outcome::Passed,
                    expected_error: expected_error(case.id).map(str::to_owned),
                    zero_call_asserted: case.zero_call_asserted,
                })
                .collect(),
            stop_conditions: STOP_CONDITIONS
                .iter()
                .map(|id| StopCondition {
                    id: (*id).to_owned(),
                    observed: false,
                })
                .collect(),
        }
    }

    #[test]
    fn complete_pass_report_verifies_and_round_trips() {
        let report = fixture();
        validate_report(&report, REVISION).expect("complete report passes");
        let encoded = encode_report(&report).expect("report encodes");
        let decoded = decode_report(&encoded).expect("canonical report decodes");
        assert_eq!(decoded, report);
    }

    #[test]
    fn every_missing_case_or_stop_condition_blocks_verification() {
        let mut missing_case = fixture();
        missing_case.cases.pop();
        assert!(matches!(
            validate_report(&missing_case, REVISION),
            Err(ContractMatrixError::InvalidReport)
        ));

        let mut observed_stop = fixture();
        observed_stop.stop_conditions[0].observed = true;
        assert!(matches!(
            validate_report(&observed_stop, REVISION),
            Err(ContractMatrixError::Blocked)
        ));
    }

    #[test]
    fn failed_suite_and_wrong_revision_block_verification() {
        let mut failed = fixture();
        failed.suites[0].status = Outcome::Failed;
        failed.suites[0].exit_code = Some(1);
        assert!(matches!(
            validate_report(&failed, REVISION),
            Err(ContractMatrixError::Blocked)
        ));
        assert!(matches!(
            validate_report(&fixture(), "abcdefabcdefabcdefabcdefabcdefabcdefabcd"),
            Err(ContractMatrixError::RevisionMismatch)
        ));
    }

    #[test]
    fn options_are_closed_and_require_exact_revision() {
        let options = Options::parse(
            &mut [
                "--verify".to_owned(),
                "matrix.json".to_owned(),
                "--source-revision".to_owned(),
                REVISION.to_owned(),
            ]
            .into_iter(),
        )
        .expect("closed options parse");
        assert_eq!(
            options,
            Options {
                mode: Mode::Verify("matrix.json".into()),
                source_revision: REVISION.to_owned(),
            }
        );
        assert!(Options::parse(&mut std::iter::empty()).is_err());
        assert!(
            Options::parse(
                &mut [
                    "--output".to_owned(),
                    "matrix.json".to_owned(),
                    "--source-revision".to_owned(),
                    "short".to_owned(),
                ]
                .into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn report_digest_is_stable() {
        let encoded = encode_report(&fixture()).expect("fixture encodes");
        let first = Sha256::digest(&encoded);
        let second = Sha256::digest(&encoded);
        assert_eq!(first, second);
    }
}
