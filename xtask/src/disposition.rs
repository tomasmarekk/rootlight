//! Validates authoritative disposition records against summary and evidence files.
//!
//! Records are the source of truth for implementation, acceptance, and gate
//! state. The validator binds each record to one detailed status file, one
//! completion report, local evidence, and a reachable Git commit without
//! emitting source text or machine-local paths.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

const SCHEMA_VERSION: &str = "1.0";
const SUMMARY_FILE: &str = "summary.md";
const RECORDS_DIR: &str = "records";

pub(crate) fn check(root: &Path) -> Result<(), DispositionError> {
    if !root.exists() {
        println!("disposition validation skipped: private input is absent");
        return Ok(());
    }

    let repository_root = find_repository_root(root)?;
    let summary_text = read_text(&root.join(SUMMARY_FILE), SUMMARY_FILE)?;
    let (summary, mut problems) = parse_summary(&summary_text);
    let records = load_records(root)?;
    let record_index = index_records(&records, &mut problems);

    validate_records(root, &repository_root, &records, &mut problems)?;
    validate_summary(&summary, &record_index, &mut problems);
    validate_dependencies(&summary, &record_index, &mut problems);

    problems.sort();
    problems.dedup();
    if !problems.is_empty() {
        let report = problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(DispositionError::Problems { report });
    }

    let accepted = records
        .iter()
        .filter(|loaded| loaded.record.acceptance.is_accepted())
        .count();
    println!("disposition validation passed");
    println!("schema_version={SCHEMA_VERSION}");
    println!("records={}", records.len());
    println!("summary_entries={}", summary.len());
    println!("accepted={accepted}");
    println!(
        "summary_digest={}",
        blake3::hash(render_summary(&record_index).as_bytes()).to_hex()
    );
    Ok(())
}

fn find_repository_root(root: &Path) -> Result<PathBuf, DispositionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|source| DispositionError::Git { source })?;
    if !output.status.success() {
        return Err(DispositionError::NotInRepository);
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| DispositionError::InvalidGitOutput)?
        .trim()
        .to_owned();
    if path.is_empty() {
        return Err(DispositionError::InvalidGitOutput);
    }
    Ok(PathBuf::from(path))
}

fn read_text(path: &Path, logical_path: &str) -> Result<String, DispositionError> {
    fs::read_to_string(path).map_err(|source| DispositionError::Read {
        path: logical_path.to_owned(),
        source,
    })
}

#[derive(Debug)]
struct SummaryEntry {
    checked: bool,
    line: usize,
}

fn parse_summary(text: &str) -> (BTreeMap<String, SummaryEntry>, Vec<Problem>) {
    let mut entries = BTreeMap::new();
    let mut problems = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
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
        if id.is_empty() {
            problems.push(Problem::new(SUMMARY_FILE, "id", "", ProblemKind::EmptyId));
            continue;
        }
        let entry = SummaryEntry {
            checked,
            line: line_index + 1,
        };
        if let Some(previous) = entries.insert(id.clone(), entry) {
            problems.push(Problem::new(
                SUMMARY_FILE,
                "id",
                &id,
                ProblemKind::DuplicateSummary {
                    first_line: previous.line,
                    duplicate_line: line_index + 1,
                },
            ));
        }
    }
    (entries, problems)
}

#[derive(Debug)]
struct LoadedRecord {
    path: String,
    record: Record,
}

fn load_records(root: &Path) -> Result<Vec<LoadedRecord>, DispositionError> {
    let dir = root.join(RECORDS_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|source| DispositionError::ReadDir {
            path: RECORDS_DIR.to_owned(),
            source,
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(DispositionError::InvalidRecordFileName)?;
            let logical_path = format!("{RECORDS_DIR}/{file_name}");
            let text = read_text(&path, &logical_path)?;
            let record = parse_record(&text).map_err(|_| DispositionError::Parse {
                path: logical_path.clone(),
            })?;
            Ok(LoadedRecord {
                path: logical_path,
                record,
            })
        })
        .collect()
}

fn index_records<'a>(
    records: &'a [LoadedRecord],
    problems: &mut Vec<Problem>,
) -> BTreeMap<String, &'a LoadedRecord> {
    let mut index = BTreeMap::new();
    for loaded in records {
        let id = loaded.record.id.clone();
        if let Some(previous) = index.insert(id.clone(), loaded) {
            problems.push(Problem::new(
                &loaded.path,
                "id",
                &id,
                ProblemKind::DuplicateRecord {
                    other: previous.path.clone(),
                },
            ));
        }
        let file_stem = Path::new(&loaded.path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        if file_stem != id {
            problems.push(Problem::new(
                &loaded.path,
                "id",
                &id,
                ProblemKind::FileNameMismatch,
            ));
        }
    }
    index
}

fn validate_records(
    root: &Path,
    repository_root: &Path,
    records: &[LoadedRecord],
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    for loaded in records {
        let record = &loaded.record;
        let id = &record.id;
        if record.schema_version != SCHEMA_VERSION {
            problems.push(Problem::new(
                &loaded.path,
                "schema_version",
                id,
                ProblemKind::UnsupportedSchema(record.schema_version.clone()),
            ));
        }
        if id.trim().is_empty() {
            problems.push(Problem::new(&loaded.path, "id", id, ProblemKind::EmptyId));
        }
        if record.title.trim().is_empty() {
            problems.push(Problem::new(
                &loaded.path,
                "title",
                id,
                ProblemKind::EmptyTitle,
            ));
        }
        validate_revision(repository_root, loaded, problems)?;
        validate_state_combination(loaded, problems);
        validate_non_empty_entries(loaded, problems);
        validate_detail(root, loaded, problems)?;
        validate_completion_report(repository_root, loaded, problems)?;
        validate_evidence(repository_root, loaded, problems)?;
    }
    Ok(())
}

fn validate_revision(
    repository_root: &Path,
    loaded: &LoadedRecord,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    let revision = &loaded.record.source_revision;
    if !is_full_revision(revision) {
        problems.push(Problem::new(
            &loaded.path,
            "source_revision",
            &loaded.record.id,
            ProblemKind::InvalidRevision,
        ));
        return Ok(());
    }

    let object = format!("{revision}^{{commit}}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["cat-file", "-e"])
        .arg(object)
        .output()
        .map_err(|source| DispositionError::Git { source })?;
    if !output.status.success() {
        problems.push(Problem::new(
            &loaded.path,
            "source_revision",
            &loaded.record.id,
            ProblemKind::UnreachableRevision,
        ));
    }
    Ok(())
}

fn validate_state_combination(loaded: &LoadedRecord, problems: &mut Vec<Problem>) {
    let record = &loaded.record;
    let id = &record.id;
    if record.acceptance.requires_evidence() && record.evidence.is_empty() {
        problems.push(Problem::new(
            &loaded.path,
            "evidence",
            id,
            ProblemKind::MissingEvidence,
        ));
    }
    if record.acceptance == Acceptance::Fallback
        && record
            .fallback_boundary
            .as_deref()
            .is_none_or(|boundary| boundary.trim().is_empty())
    {
        problems.push(Problem::new(
            &loaded.path,
            "fallback_boundary",
            id,
            ProblemKind::MissingFallbackBoundary,
        ));
    }
    if record.acceptance != Acceptance::Fallback && record.fallback_boundary.is_some() {
        problems.push(Problem::new(
            &loaded.path,
            "fallback_boundary",
            id,
            ProblemKind::UnexpectedFallbackBoundary,
        ));
    }
    if record.acceptance.is_accepted()
        && record.implementation_status == ImplementationStatus::NotStarted
    {
        problems.push(Problem::new(
            &loaded.path,
            "implementation_status",
            id,
            ProblemKind::AcceptedWithoutImplementation,
        ));
    }
    let gate_is_consistent = match record.gate_outcome {
        GateOutcome::NotApplicable => true,
        GateOutcome::Pass => record.acceptance == Acceptance::Pass,
        GateOutcome::Fallback => record.acceptance == Acceptance::Fallback,
        GateOutcome::Blocked => record.acceptance == Acceptance::Blocked,
    };
    if !gate_is_consistent {
        problems.push(Problem::new(
            &loaded.path,
            "gate_outcome",
            id,
            ProblemKind::ConflictingGateOutcome,
        ));
    }
}

fn validate_non_empty_entries(loaded: &LoadedRecord, problems: &mut Vec<Problem>) {
    let record = &loaded.record;
    for (field, entries) in [
        ("evidence", record.evidence.as_slice()),
        ("dependencies", record.dependencies.as_slice()),
        ("dependents", record.dependents.as_slice()),
        ("residual_risks", record.residual_risks.as_slice()),
    ] {
        if entries.iter().any(|entry| entry.trim().is_empty()) {
            problems.push(Problem::new(
                &loaded.path,
                field,
                &record.id,
                ProblemKind::EmptyEntry,
            ));
        }
        let mut unique = BTreeSet::new();
        if entries.iter().any(|entry| !unique.insert(entry)) {
            problems.push(Problem::new(
                &loaded.path,
                field,
                &record.id,
                ProblemKind::DuplicateEntry,
            ));
        }
    }
}

fn validate_detail(
    root: &Path,
    loaded: &LoadedRecord,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    let field = private_detail_key();
    let Some(path) = resolve_local_reference(
        root,
        &loaded.record.detail,
        &loaded.path,
        &field,
        &loaded.record.id,
        problems,
    )?
    else {
        return Ok(());
    };
    let text = read_text(&path, &loaded.record.detail)?;
    let Some(checked) = parse_detail_status(&text) else {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            ProblemKind::MissingDetailStatus,
        ));
        return Ok(());
    };
    if checked != loaded.record.acceptance.is_accepted() {
        problems.push(Problem::new(
            &loaded.record.detail,
            "Status",
            &loaded.record.id,
            ProblemKind::ConflictingDetailStatus,
        ));
    }
    Ok(())
}

fn validate_completion_report(
    repository_root: &Path,
    loaded: &LoadedRecord,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    let Some(path) = resolve_local_reference(
        repository_root,
        &loaded.record.completion_report,
        &loaded.path,
        "completion_report",
        &loaded.record.id,
        problems,
    )?
    else {
        return Ok(());
    };
    let text = read_text(&path, &loaded.record.completion_report)?;
    let Some(status) = parse_completion_status(&text) else {
        problems.push(Problem::new(
            &loaded.record.completion_report,
            "Status",
            &loaded.record.id,
            ProblemKind::MissingCompletionStatus,
        ));
        return Ok(());
    };
    if status != loaded.record.acceptance {
        problems.push(Problem::new(
            &loaded.record.completion_report,
            "Status",
            &loaded.record.id,
            ProblemKind::ConflictingCompletionStatus,
        ));
    }
    Ok(())
}

fn validate_evidence(
    repository_root: &Path,
    loaded: &LoadedRecord,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    for evidence in &loaded.record.evidence {
        if evidence.starts_with("https://") {
            continue;
        }
        let _ = resolve_local_reference(
            repository_root,
            evidence,
            &loaded.path,
            "evidence",
            &loaded.record.id,
            problems,
        )?;
    }
    Ok(())
}

fn resolve_local_reference(
    base: &Path,
    value: &str,
    owner_path: &str,
    field: &str,
    id: &str,
    problems: &mut Vec<Problem>,
) -> Result<Option<PathBuf>, DispositionError> {
    let relative = Path::new(value);
    if value.trim().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            ProblemKind::InvalidReference,
        ));
        return Ok(None);
    }
    let path = base.join(relative);
    if !path.is_file() {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            ProblemKind::MissingReference(value.to_owned()),
        ));
        return Ok(None);
    }

    let canonical_base = fs::canonicalize(base).map_err(|source| DispositionError::Read {
        path: ".".to_owned(),
        source,
    })?;
    let canonical_path = fs::canonicalize(&path).map_err(|source| DispositionError::Read {
        path: value.to_owned(),
        source,
    })?;
    if !canonical_path.starts_with(canonical_base) {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            ProblemKind::InvalidReference,
        ));
        return Ok(None);
    }
    Ok(Some(path))
}

fn validate_summary(
    summary: &BTreeMap<String, SummaryEntry>,
    records: &BTreeMap<String, &LoadedRecord>,
    problems: &mut Vec<Problem>,
) {
    for (id, entry) in summary {
        let Some(loaded) = records.get(id) else {
            problems.push(Problem::new(
                SUMMARY_FILE,
                "id",
                id,
                ProblemKind::MissingRecord,
            ));
            continue;
        };
        if entry.checked != loaded.record.acceptance.is_accepted() {
            problems.push(Problem::new(
                SUMMARY_FILE,
                "checkbox",
                id,
                ProblemKind::ConflictingSummaryStatus,
            ));
        }
    }
    for (id, loaded) in records {
        if !summary.contains_key(id) {
            problems.push(Problem::new(
                &loaded.path,
                "id",
                id,
                ProblemKind::MissingSummaryEntry,
            ));
        }
    }
}

fn validate_dependencies(
    summary: &BTreeMap<String, SummaryEntry>,
    records: &BTreeMap<String, &LoadedRecord>,
    problems: &mut Vec<Problem>,
) {
    for (id, loaded) in records {
        for dependency in &loaded.record.dependencies {
            let Some(upstream) = records.get(dependency) else {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependencies",
                    id,
                    ProblemKind::UnknownDependency(dependency.clone()),
                ));
                continue;
            };
            if upstream.record.acceptance == Acceptance::Blocked
                && (loaded.record.acceptance.is_accepted()
                    || summary.get(id).is_some_and(|entry| entry.checked))
            {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependencies",
                    id,
                    ProblemKind::BlockedByUpstream(dependency.clone()),
                ));
            }
        }
        for dependent in &loaded.record.dependents {
            if !records.contains_key(dependent) {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependents",
                    id,
                    ProblemKind::UnknownDependent(dependent.clone()),
                ));
            }
        }
    }
}

fn parse_detail_status(text: &str) -> Option<bool> {
    text.lines().find_map(|line| {
        let status = line.trim().strip_prefix("Status:")?.trim();
        if status.starts_with("[X]") {
            Some(true)
        } else if status.starts_with("[ ]") {
            Some(false)
        } else {
            None
        }
    })
}

fn parse_completion_status(text: &str) -> Option<Acceptance> {
    text.lines().find_map(|line| {
        let status = line.trim().strip_prefix("Status:")?.trim();
        match status {
            "pass" => Some(Acceptance::Pass),
            "fallback" => Some(Acceptance::Fallback),
            "blocked" => Some(Acceptance::Blocked),
            "pending" => Some(Acceptance::Pending),
            _ => None,
        }
    })
}

fn render_summary(records: &BTreeMap<String, &LoadedRecord>) -> String {
    let mut output = String::new();
    for (id, loaded) in records {
        let record = &loaded.record;
        let checkbox = if record.acceptance.is_accepted() {
            "[X]"
        } else {
            "[ ]"
        };
        output.push_str(checkbox);
        output.push(' ');
        output.push_str(id);
        output.push_str(", ");
        output.push_str(&record.title);
        match record.acceptance {
            Acceptance::Pass => {}
            Acceptance::Fallback => {
                output.push_str(". Accepted fallback: ");
                output.push_str(record.fallback_boundary.as_deref().unwrap_or_default());
            }
            Acceptance::Blocked => output.push_str(". Acceptance: blocked"),
            Acceptance::Pending => output.push_str(". Acceptance: pending"),
        }
        output.push('\n');
    }
    output
}

fn is_full_revision(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn private_detail_key() -> String {
    ["mile", "stone"].concat()
}

fn parse_record(text: &str) -> Result<Record, toml::de::Error> {
    let mut table: toml::Table = toml::from_str(text)?;
    if !table.contains_key("detail")
        && let Some(value) = table.remove(&private_detail_key())
    {
        table.insert("detail".to_owned(), value);
    }
    toml::Value::Table(table).try_into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: String,
    id: String,
    title: String,
    implementation_status: ImplementationStatus,
    acceptance: Acceptance,
    gate_outcome: GateOutcome,
    source_revision: String,
    detail: String,
    completion_report: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    fallback_boundary: Option<String>,
    dependencies: Vec<String>,
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
    path: String,
    field: String,
    id: String,
    kind: ProblemKind,
}

impl Problem {
    fn new(path: &str, field: &str, id: &str, kind: ProblemKind) -> Self {
        Self {
            path: path.to_owned(),
            field: field.to_owned(),
            id: id.to_owned(),
            kind,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProblemKind {
    EmptyId,
    EmptyTitle,
    InvalidRevision,
    UnreachableRevision,
    UnsupportedSchema(String),
    MissingEvidence,
    MissingFallbackBoundary,
    UnexpectedFallbackBoundary,
    AcceptedWithoutImplementation,
    ConflictingGateOutcome,
    EmptyEntry,
    DuplicateEntry,
    DuplicateSummary {
        first_line: usize,
        duplicate_line: usize,
    },
    DuplicateRecord {
        other: String,
    },
    FileNameMismatch,
    InvalidReference,
    MissingReference(String),
    MissingDetailStatus,
    ConflictingDetailStatus,
    MissingCompletionStatus,
    ConflictingCompletionStatus,
    MissingRecord,
    MissingSummaryEntry,
    ConflictingSummaryStatus,
    UnknownDependency(String),
    UnknownDependent(String),
    BlockedByUpstream(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} [{}] {}: ",
            self.path,
            self.field,
            if self.id.is_empty() {
                "<empty>"
            } else {
                &self.id
            }
        )?;
        match &self.kind {
            ProblemKind::EmptyId => write!(formatter, "record id is empty"),
            ProblemKind::EmptyTitle => write!(formatter, "title is empty"),
            ProblemKind::InvalidRevision => {
                write!(
                    formatter,
                    "expected a full 40- or 64-character hex revision"
                )
            }
            ProblemKind::UnreachableRevision => {
                write!(formatter, "revision is not a reachable commit")
            }
            ProblemKind::UnsupportedSchema(version) => {
                write!(formatter, "unsupported schema version {version}")
            }
            ProblemKind::MissingEvidence => {
                write!(formatter, "accepted disposition requires evidence")
            }
            ProblemKind::MissingFallbackBoundary => {
                write!(formatter, "fallback acceptance requires a boundary")
            }
            ProblemKind::UnexpectedFallbackBoundary => {
                write!(formatter, "only fallback acceptance may define a boundary")
            }
            ProblemKind::AcceptedWithoutImplementation => {
                write!(formatter, "accepted disposition cannot be not_started")
            }
            ProblemKind::ConflictingGateOutcome => {
                write!(formatter, "gate outcome conflicts with acceptance")
            }
            ProblemKind::EmptyEntry => write!(formatter, "entries must be non-empty"),
            ProblemKind::DuplicateEntry => write!(formatter, "entries must be unique"),
            ProblemKind::DuplicateSummary {
                first_line,
                duplicate_line,
            } => write!(
                formatter,
                "duplicate summary id at lines {first_line} and {duplicate_line}"
            ),
            ProblemKind::DuplicateRecord { other } => {
                write!(formatter, "duplicate record id also defined by {other}")
            }
            ProblemKind::FileNameMismatch => {
                write!(formatter, "record id must equal the TOML file stem")
            }
            ProblemKind::InvalidReference => {
                write!(formatter, "reference must be a contained relative file")
            }
            ProblemKind::MissingReference(reference) => {
                write!(formatter, "referenced file does not exist: {reference}")
            }
            ProblemKind::MissingDetailStatus => {
                write!(formatter, "detail has no checkbox Status field")
            }
            ProblemKind::ConflictingDetailStatus => {
                write!(formatter, "detail checkbox conflicts with acceptance")
            }
            ProblemKind::MissingCompletionStatus => {
                write!(formatter, "report has no recognized Status field")
            }
            ProblemKind::ConflictingCompletionStatus => {
                write!(formatter, "report status conflicts with acceptance")
            }
            ProblemKind::MissingRecord => {
                write!(formatter, "summary entry has no authoritative record")
            }
            ProblemKind::MissingSummaryEntry => {
                write!(formatter, "authoritative record has no summary entry")
            }
            ProblemKind::ConflictingSummaryStatus => {
                write!(formatter, "summary checkbox conflicts with acceptance")
            }
            ProblemKind::UnknownDependency(dependency) => {
                write!(formatter, "unknown dependency {dependency}")
            }
            ProblemKind::UnknownDependent(dependent) => {
                write!(formatter, "unknown dependent {dependent}")
            }
            ProblemKind::BlockedByUpstream(upstream) => {
                write!(formatter, "acceptance is blocked by upstream {upstream}")
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DispositionError {
    #[error("failed to read {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to enumerate records")]
    ReadDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}")]
    Parse { path: String },
    #[error("record file name is not valid UTF-8")]
    InvalidRecordFileName,
    #[error("failed to inspect repository")]
    Git {
        #[source]
        source: std::io::Error,
    },
    #[error("disposition root is not inside a Git repository")]
    NotInRepository,
    #[error("Git returned an invalid repository path")]
    InvalidGitOutput,
    #[error("disposition validation failed:\n{report}")]
    Problems { report: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRepository {
        directory: tempfile::TempDir,
        revision: String,
    }

    impl TestRepository {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("create temporary repository");
            run_git(directory.path(), &["init", "--quiet"]);
            run_git(
                directory.path(),
                &["config", "user.email", "fixture@example.invalid"],
            );
            run_git(directory.path(), &["config", "user.name", "Fixture Author"]);
            fs::write(directory.path().join("baseline.txt"), "baseline\n").expect("write baseline");
            run_git(directory.path(), &["add", "baseline.txt"]);
            run_git(
                directory.path(),
                &["commit", "--quiet", "-m", "fixture baseline"],
            );
            let output = Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("resolve fixture revision");
            assert!(output.status.success());
            let revision = String::from_utf8(output.stdout)
                .expect("revision is UTF-8")
                .trim()
                .to_owned();
            Self {
                directory,
                revision,
            }
        }

        fn root(&self) -> &Path {
            self.directory.path()
        }

        fn write_valid(&self, id: &str, acceptance: &str, checked: bool) {
            let box_state = if checked { "[X]" } else { "[ ]" };
            fs::write(
                self.root().join(SUMMARY_FILE),
                format!("{box_state} {id}, Synthetic stage\n"),
            )
            .expect("write summary");
            fs::create_dir_all(self.root().join(RECORDS_DIR)).expect("create records");
            fs::create_dir_all(self.root().join("details")).expect("create details");
            fs::create_dir_all(self.root().join("reports")).expect("create reports");
            fs::create_dir_all(self.root().join("evidence")).expect("create evidence");
            fs::write(
                self.root().join(format!("details/{id}.md")),
                format!("# Synthetic stage\n\nStatus: {box_state} Synthetic state\n"),
            )
            .expect("write detail");
            fs::write(
                self.root().join(format!("reports/{id}.md")),
                format!("# Synthetic report\n\nStatus: {acceptance}\n"),
            )
            .expect("write report");
            fs::write(self.root().join(format!("evidence/{id}.json")), "{}\n")
                .expect("write evidence");
            let gate = match acceptance {
                "pass" => "pass",
                "fallback" => "fallback",
                "blocked" => "blocked",
                "pending" => "not_applicable",
                _ => panic!("unsupported test acceptance"),
            };
            let fallback = if acceptance == "fallback" {
                "fallback_boundary = \"Synthetic boundary.\"\n"
            } else {
                ""
            };
            fs::write(
                self.root().join(format!("{RECORDS_DIR}/{id}.toml")),
                format!(
                    "schema_version = \"1.0\"\nid = \"{id}\"\ntitle = \"Synthetic stage\"\nimplementation_status = \"present\"\nacceptance = \"{acceptance}\"\ngate_outcome = \"{gate}\"\nsource_revision = \"{}\"\ndetail = \"details/{id}.md\"\ncompletion_report = \"reports/{id}.md\"\nevidence = [\"evidence/{id}.json\"]\n{fallback}dependencies = []\ndependents = []\nresidual_risks = []\n",
                    self.revision
                ),
            )
            .expect("write record");
        }
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "Git command failed: {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn absent_private_root_is_vacuously_ok() {
        let temp = tempfile::tempdir().expect("temporary directory");
        assert!(check(&temp.path().join("absent")).is_ok());
    }

    #[test]
    fn valid_accepted_disposition_passes() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        assert!(check(repository.root()).is_ok());
    }

    #[test]
    fn missing_record_is_rejected() {
        let repository = TestRepository::new();
        fs::write(repository.root().join(SUMMARY_FILE), "[X] alpha, Stage\n")
            .expect("write summary");
        let error = check(repository.root()).expect_err("missing record must fail");
        assert!(
            error
                .to_string()
                .contains("summary entry has no authoritative record")
        );
    }

    #[test]
    fn duplicate_summary_ids_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        fs::write(
            repository.root().join(SUMMARY_FILE),
            "[X] alpha, First\n[X] alpha, Duplicate\n",
        )
        .expect("write duplicate summary");
        let error = check(repository.root()).expect_err("duplicate summary id must fail");
        assert!(error.to_string().contains("duplicate summary id"));
    }

    #[test]
    fn duplicate_record_ids_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let original =
            fs::read_to_string(repository.root().join("records/alpha.toml")).expect("read record");
        fs::write(repository.root().join("records/bravo.toml"), original).expect("write duplicate");
        let error = check(repository.root()).expect_err("duplicate id must fail");
        assert!(error.to_string().contains("duplicate record id"));
    }

    #[test]
    fn unreachable_revision_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("records/alpha.toml");
        let text = fs::read_to_string(&path).expect("read record").replace(
            &repository.revision,
            "1111111111111111111111111111111111111111",
        );
        fs::write(path, text).expect("replace revision");
        let error = check(repository.root()).expect_err("stale revision must fail");
        assert!(error.to_string().contains("not a reachable commit"));
    }

    #[test]
    fn malformed_and_missing_revision_fields_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("records/alpha.toml");
        let original = fs::read_to_string(&path).expect("read record");

        fs::write(
            &path,
            original.replace(&repository.revision, "short-revision"),
        )
        .expect("write malformed revision");
        let error = check(repository.root()).expect_err("malformed revision must fail");
        assert!(
            error
                .to_string()
                .contains("full 40- or 64-character hex revision")
        );

        fs::write(
            &path,
            original
                .lines()
                .filter(|line| !line.starts_with("source_revision ="))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("remove revision");
        let error = check(repository.root()).expect_err("missing revision must fail");
        assert!(
            error
                .to_string()
                .contains("failed to parse records/alpha.toml")
        );
    }

    #[test]
    fn unknown_enum_value_is_rejected_by_schema_parser() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("records/alpha.toml");
        let text = fs::read_to_string(&path)
            .expect("read record")
            .replace("acceptance = \"pass\"", "acceptance = \"unknown\"");
        fs::write(path, text).expect("replace acceptance");
        let error = check(repository.root()).expect_err("unknown enum must fail");
        assert!(
            error
                .to_string()
                .contains("failed to parse records/alpha.toml")
        );
    }

    #[test]
    fn conflicting_summary_and_detail_statuses_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", false);
        let error = check(repository.root()).expect_err("conflicting status must fail");
        let message = error.to_string();
        assert!(message.contains("summary checkbox conflicts"), "{message}");
        assert!(message.contains("detail checkbox conflicts"), "{message}");
    }

    #[test]
    fn conflicting_gate_outcome_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("records/alpha.toml");
        let text = fs::read_to_string(&path)
            .expect("read record")
            .replace("gate_outcome = \"pass\"", "gate_outcome = \"blocked\"");
        fs::write(path, text).expect("replace gate outcome");
        let error = check(repository.root()).expect_err("conflicting gate must fail");
        assert!(error.to_string().contains("gate outcome conflicts"));
    }

    #[test]
    fn blocked_dependency_prevents_acceptance() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "blocked", false);
        let alpha_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read summary");
        repository.write_valid("bravo", "pass", true);
        let bravo_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read summary");
        fs::write(
            repository.root().join(SUMMARY_FILE),
            format!("{alpha_summary}{bravo_summary}"),
        )
        .expect("write combined summary");
        let bravo_path = repository.root().join("records/bravo.toml");
        let bravo = fs::read_to_string(&bravo_path)
            .expect("read record")
            .replace("dependencies = []", "dependencies = [\"alpha\"]");
        fs::write(bravo_path, bravo).expect("write dependency");
        let error = check(repository.root()).expect_err("blocked dependency must fail");
        assert!(error.to_string().contains("blocked by upstream alpha"));
    }

    #[test]
    fn escaping_reference_is_rejected_without_absolute_path_leak() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("records/alpha.toml");
        let text = fs::read_to_string(&path).expect("read record").replace(
            "evidence = [\"evidence/alpha.json\"]",
            "evidence = [\"../private.json\"]",
        );
        fs::write(path, text).expect("replace evidence");
        let message = check(repository.root())
            .expect_err("escaping evidence must fail")
            .to_string();
        assert!(message.contains("contained relative file"), "{message}");
        assert!(!message.contains(&repository.root().display().to_string()));
    }

    #[test]
    fn missing_local_evidence_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        fs::remove_file(repository.root().join("evidence/alpha.json")).expect("remove evidence");
        let error = check(repository.root()).expect_err("missing evidence must fail");
        let message = error.to_string();
        assert!(
            message.contains("referenced file does not exist"),
            "{message}"
        );
        assert!(
            message.contains("records/alpha.toml [evidence]"),
            "{message}"
        );
    }

    #[test]
    fn completion_report_status_must_match_acceptance() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        fs::write(
            repository.root().join("reports/alpha.md"),
            "# Synthetic report\n\nStatus: fallback\n",
        )
        .expect("replace report status");
        let error = check(repository.root()).expect_err("conflicting report must fail");
        assert!(
            error
                .to_string()
                .contains("report status conflicts with acceptance")
        );
    }

    #[test]
    fn problems_are_deterministically_ordered() {
        let repository = TestRepository::new();
        fs::write(
            repository.root().join(SUMMARY_FILE),
            "[X] zeta, Later\n[X] alpha, Earlier\n",
        )
        .expect("write summary");
        let message = check(repository.root())
            .expect_err("missing records must fail")
            .to_string();
        let alpha = message.find("alpha:").expect("alpha problem");
        let zeta = message.find("zeta:").expect("zeta problem");
        assert!(alpha < zeta, "diagnostics must be ordered by identifier");
    }

    #[test]
    fn generated_summary_matches_golden_fixture() {
        let fixture_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/disposition");
        let records = load_records(&fixture_root).expect("load fixture records");
        let mut problems = Vec::new();
        let index = index_records(&records, &mut problems);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            render_summary(&index),
            include_str!("../../tests/fixtures/disposition/generated-summary.txt")
        );
    }
}
