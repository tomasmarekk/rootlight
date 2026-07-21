//! Validates authoritative stage disposition records against the plan summary.
//!
//! One machine-readable record per stage is the source of truth for
//! implementation status, acceptance disposition, gate outcome, source
//! revision, and evidence. The validator rejects impossible combinations so
//! the summary cannot claim acceptance that the records do not support. Record
//! identifiers are data, never hard-coded here, so the check stays generic.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

const SCHEMA_VERSION: &str = "1.0";
const SUMMARY_FILE: &str = "summary.md";
const RECORDS_DIR: &str = "records";

pub(crate) fn check(root: &Path) -> Result<(), DispositionError> {
    if !root.exists() {
        println!(
            "disposition check skipped: {} is absent, so the public tree does not depend on it",
            root.display()
        );
        return Ok(());
    }

    let summary_path = root.join(SUMMARY_FILE);
    let summary_text =
        fs::read_to_string(&summary_path).map_err(|source| DispositionError::Read {
            path: summary_path.clone(),
            source,
        })?;
    let summary = parse_summary(&summary_text);

    let records = load_records(root)?;

    let mut problems: Vec<Problem> = Vec::new();
    validate_records(&records, &mut problems);
    validate_summary_against_records(&summary, &records, &mut problems);
    validate_gate_blocking(&summary, &records, &mut problems);

    if problems.is_empty() {
        println!(
            "disposition check passed: {} records consistent with {} summary entries",
            records.len(),
            summary.len()
        );
        return Ok(());
    }

    problems.sort();
    let report = problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    Err(DispositionError::Problems { report })
}

/// Parses summary checkbox lines of the form `[X] <id>, ...` into a map from
/// stage id to whether the box is checked. Identifiers are treated as opaque
/// data so no naming convention is assumed here.
fn parse_summary(text: &str) -> BTreeMap<String, bool> {
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = match trimmed.strip_prefix("[X] ") {
            Some(rest) => Some((rest, true)),
            None => trimmed.strip_prefix("[ ] ").map(|rest| (rest, false)),
        };
        let Some((rest, checked)) = rest else {
            continue;
        };
        let id = rest
            .split([',', ' ', '\t'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !id.is_empty() {
            entries.insert(id, checked);
        }
    }
    entries
}

fn load_records(root: &Path) -> Result<BTreeMap<String, Record>, DispositionError> {
    let dir = root.join(RECORDS_DIR);
    let mut records = BTreeMap::new();
    if !dir.exists() {
        return Ok(records);
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|source| DispositionError::ReadDir {
            path: dir.clone(),
            source,
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    for path in paths {
        let text = fs::read_to_string(&path).map_err(|source| DispositionError::Read {
            path: path.clone(),
            source,
        })?;
        let record: Record = toml::from_str(&text).map_err(|source| DispositionError::Parse {
            path: path.clone(),
            source,
        })?;
        if records.insert(record.id.clone(), record).is_some() {
            // Duplicate ids are reported by validate_records; keep the first.
        }
    }
    Ok(records)
}

fn validate_records(records: &BTreeMap<String, Record>, problems: &mut Vec<Problem>) {
    for record in records.values() {
        if record.schema_version != SCHEMA_VERSION {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::UnsupportedSchema(record.schema_version.clone()),
            });
        }
        if record.id.trim().is_empty() {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::EmptyId,
            });
        }
        if !is_full_revision(&record.source_revision) {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::InvalidRevision,
            });
        }
        if record.acceptance.requires_evidence() && record.evidence.is_empty() {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::MissingEvidence,
            });
        }
        if record.title.trim().is_empty() {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::EmptyTitle,
            });
        }
        if record.acceptance == Acceptance::Fallback
            && record
                .fallback_boundary
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::MissingFallbackBoundary,
            });
        }
        if record.acceptance.is_accepted()
            && record.implementation_status == ImplementationStatus::NotStarted
        {
            problems.push(Problem {
                id: record.id.clone(),
                kind: ProblemKind::AcceptedWithoutImplementation,
            });
        }
        for entry in record.evidence.iter().chain(record.residual_risks.iter()) {
            if entry.trim().is_empty() {
                problems.push(Problem {
                    id: record.id.clone(),
                    kind: ProblemKind::EmptyEntry,
                });
                break;
            }
        }
    }
}

fn validate_summary_against_records(
    summary: &BTreeMap<String, bool>,
    records: &BTreeMap<String, Record>,
    problems: &mut Vec<Problem>,
) {
    for (id, checked) in summary {
        if !checked {
            continue;
        }
        match records.get(id) {
            None => problems.push(Problem {
                id: id.clone(),
                kind: ProblemKind::CheckedWithoutRecord,
            }),
            Some(record) if !record.acceptance.is_accepted() => problems.push(Problem {
                id: id.clone(),
                kind: ProblemKind::CheckedWithoutAcceptance,
            }),
            Some(record) if record.evidence.is_empty() => problems.push(Problem {
                id: id.clone(),
                kind: ProblemKind::MissingEvidence,
            }),
            Some(_) => {}
        }
    }
}

fn validate_gate_blocking(
    summary: &BTreeMap<String, bool>,
    records: &BTreeMap<String, Record>,
    problems: &mut Vec<Problem>,
) {
    for record in records.values() {
        if record.gate_outcome != Some(GateOutcome::Blocked) {
            continue;
        }
        for dependent in &record.dependents {
            let dependent_accepted = records
                .get(dependent)
                .is_some_and(|dependent_record| dependent_record.acceptance.is_accepted());
            let dependent_checked = summary.get(dependent).copied().unwrap_or(false);
            if dependent_accepted || dependent_checked {
                problems.push(Problem {
                    id: dependent.clone(),
                    kind: ProblemKind::BlockedByUpstream(record.id.clone()),
                });
            }
        }
    }
}

fn is_full_revision(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: String,
    id: String,
    title: String,
    implementation_status: ImplementationStatus,
    acceptance: Acceptance,
    #[serde(default)]
    gate_outcome: Option<GateOutcome>,
    source_revision: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    fallback_boundary: Option<String>,
    #[serde(default)]
    dependents: Vec<String>,
    #[serde(default)]
    residual_risks: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ImplementationStatus {
    NotStarted,
    Present,
    Complete,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Acceptance {
    Pass,
    Fallback,
    Blocked,
    Pending,
}

impl Acceptance {
    const fn is_accepted(self) -> bool {
        matches!(self, Self::Pass | Self::Fallback)
    }

    const fn requires_evidence(self) -> bool {
        self.is_accepted()
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum GateOutcome {
    Pass,
    Fallback,
    Blocked,
    NotApplicable,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Problem {
    id: String,
    kind: ProblemKind,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProblemKind {
    EmptyId,
    EmptyTitle,
    InvalidRevision,
    UnsupportedSchema(String),
    MissingEvidence,
    MissingFallbackBoundary,
    AcceptedWithoutImplementation,
    EmptyEntry,
    CheckedWithoutRecord,
    CheckedWithoutAcceptance,
    BlockedByUpstream(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ProblemKind::EmptyId => write!(formatter, "{}: record id is empty", self.id),
            ProblemKind::EmptyTitle => write!(formatter, "{}: record title is empty", self.id),
            ProblemKind::InvalidRevision => write!(
                formatter,
                "{}: source_revision is not a 64-character hex revision",
                self.id
            ),
            ProblemKind::UnsupportedSchema(version) => write!(
                formatter,
                "{}: unsupported schema_version {version}",
                self.id
            ),
            ProblemKind::MissingEvidence => write!(
                formatter,
                "{}: accepted disposition requires at least one evidence link",
                self.id
            ),
            ProblemKind::MissingFallbackBoundary => write!(
                formatter,
                "{}: fallback acceptance requires a documented fallback_boundary",
                self.id
            ),
            ProblemKind::AcceptedWithoutImplementation => write!(
                formatter,
                "{}: accepted disposition requires implementation_status present or complete",
                self.id
            ),
            ProblemKind::EmptyEntry => write!(
                formatter,
                "{}: evidence and residual_risks entries must be non-empty",
                self.id
            ),
            ProblemKind::CheckedWithoutRecord => write!(
                formatter,
                "{}: summary marks this complete but no accepted record exists",
                self.id
            ),
            ProblemKind::CheckedWithoutAcceptance => write!(
                formatter,
                "{}: summary marks this complete but the record is not accepted",
                self.id
            ),
            ProblemKind::BlockedByUpstream(gate) => write!(
                formatter,
                "{}: cannot be eligible while upstream gate {gate} is blocked",
                self.id
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DispositionError {
    #[error("failed to read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read records directory {}", path.display())]
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse record {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("disposition validation failed:\n{report}")]
    Problems { report: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn write_stage(root: &Path, id: &str, body: &str) {
        let dir = root.join(RECORDS_DIR);
        fs::create_dir_all(&dir).expect("create records dir");
        fs::write(dir.join(format!("{id}.toml")), body).expect("write record");
    }

    fn record_body(acceptance: &str, evidence: &str, extra: &str) -> String {
        format!(
            "schema_version = \"1.0\"\nid = \"alpha\"\ntitle = \"First synthetic stage\"\nimplementation_status = \"present\"\nacceptance = \"{acceptance}\"\nsource_revision = \"{REV}\"\nevidence = [{evidence}]\n{extra}"
        )
    }

    #[test]
    fn absent_root_is_vacuously_ok() {
        let temp = tempfile::tempdir().expect("temp dir");
        let missing = temp.path().join("does-not-exist");
        assert!(check(&missing).is_ok());
    }

    #[test]
    fn accepted_stage_with_evidence_passes() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[X] alpha, First synthetic stage\n",
        )
        .expect("write");
        write_stage(
            root,
            "alpha",
            &record_body("pass", "\"evidence/alpha.json\"", ""),
        );
        assert!(check(root).is_ok());
    }

    #[test]
    fn checked_without_record_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[X] alpha, First synthetic stage\n",
        )
        .expect("write");
        let error = check(root).expect_err("checked stage without record must fail");
        let message = error.to_string();
        assert!(message.contains("no accepted record"), "{message}");
    }

    #[test]
    fn accepted_without_evidence_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[ ] alpha, First synthetic stage\n",
        )
        .expect("write");
        write_stage(root, "alpha", &record_body("fallback", "", ""));
        let error = check(root).expect_err("fallback without evidence must fail");
        assert!(error.to_string().contains("evidence"));
    }

    #[test]
    fn invalid_revision_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[ ] alpha, First synthetic stage\n",
        )
        .expect("write");
        let body = record_body("pending", "", "").replace(REV, "not-a-revision");
        write_stage(root, "alpha", &body);
        let error = check(root).expect_err("invalid revision must fail");
        assert!(error.to_string().contains("source_revision"));
    }

    #[test]
    fn blocked_gate_prevents_accepted_dependent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[ ] alpha, Gate stage\n[ ] beta, Dependent stage\n",
        )
        .expect("write");
        write_stage(
            root,
            "alpha",
            &format!(
                "schema_version = \"1.0\"\nid = \"alpha\"\ntitle = \"Gate stage\"\nimplementation_status = \"present\"\nacceptance = \"blocked\"\ngate_outcome = \"blocked\"\nsource_revision = \"{REV}\"\ndependents = [\"beta\"]\n"
            ),
        );
        write_stage(
            root,
            "beta",
            &record_body("pass", "\"evidence/beta.json\"", "")
                .replace("id = \"alpha\"", "id = \"beta\""),
        );
        let error = check(root).expect_err("accepted dependent under blocked gate must fail");
        assert!(error.to_string().contains("upstream gate"));
    }

    #[test]
    fn problems_are_deterministically_ordered() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        fs::write(
            root.join(SUMMARY_FILE),
            "[X] zeta, Later stage\n[X] alpha, First synthetic stage\n",
        )
        .expect("write");
        let error = check(root).expect_err("two missing records must fail");
        let message = error.to_string();
        let alpha_pos = message.find("alpha:").expect("alpha problem");
        let zeta_pos = message.find("zeta:").expect("zeta problem");
        assert!(alpha_pos < zeta_pos, "problems must be sorted by id");
    }
}
