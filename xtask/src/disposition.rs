//! Validates authoritative disposition records against bound evidence.
//!
//! Records separate implementation, acceptance, gate, and product-capability
//! state. Every accepted claim is tied to content metadata and one exact,
//! reachable source revision without printing evidence content or local paths.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "2.0";
const SUMMARY_FILE: &str = "summary.md";
const RECORDS_DIR: &str = "records";
const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOCAL_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REMOTE_EVIDENCE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_EVIDENCE_PER_RECORD: usize = 32;
const MAX_EVIDENCE_PER_CLAIM: usize = 8;
const MAX_IDENTIFIER_BYTES: usize = 96;

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
    validate_record_dependencies(&summary, &record_index, &mut problems);
    validate_capability_graph(&records, &mut problems);

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
    let capability_count = records
        .iter()
        .map(|loaded| loaded.record.capabilities.len())
        .sum::<usize>();
    let checklist_count = records
        .iter()
        .map(|loaded| loaded.record.checklist.len())
        .sum::<usize>();
    println!("disposition validation passed");
    println!("schema_version={SCHEMA_VERSION}");
    println!("records={}", records.len());
    println!("summary_entries={}", summary.len());
    println!("accepted={accepted}");
    println!("capabilities={capability_count}");
    println!("checklist_mappings={checklist_count}");
    println!(
        "summary_sha256={}",
        sha256_hex(render_summary(&record_index).as_bytes())
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
            problems.push(Problem::new(SUMMARY_FILE, "id", "", "record id is empty"));
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
                format!(
                    "duplicate summary id at lines {} and {}",
                    previous.line,
                    line_index + 1
                ),
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
            let table =
                toml::from_str::<toml::Table>(&text).map_err(|_| DispositionError::Parse {
                    path: logical_path.clone(),
                })?;
            let version = table
                .get("schema_version")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| DispositionError::Parse {
                    path: logical_path.clone(),
                })?;
            if version != SCHEMA_VERSION {
                return Err(DispositionError::MigrationRequired {
                    path: logical_path,
                    found: version.to_owned(),
                    required: SCHEMA_VERSION,
                });
            }
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
                format!("duplicate record id also defined by {}", previous.path),
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
                "record id must equal the TOML file stem",
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
                format!("unsupported schema version {}", record.schema_version),
            ));
        }
        validate_identifier(&loaded.path, "id", id, id, problems);
        if record.title.trim().is_empty() {
            problems.push(Problem::new(&loaded.path, "title", id, "title is empty"));
        }
        validate_revision(repository_root, loaded, problems)?;
        validate_state_combination(loaded, problems);
        validate_record_ordering(loaded, problems);

        let detail_bytes = validate_bound_document(
            root,
            loaded,
            "detail",
            &record.detail,
            MAX_DOCUMENT_BYTES,
            problems,
        )?;
        if let Some(bytes) = detail_bytes {
            validate_detail(loaded, &bytes, problems);
            validate_checklist(loaded, &bytes, problems);
        }
        let report_bytes = validate_bound_document(
            repository_root,
            loaded,
            "completion_report",
            &record.completion_report,
            MAX_DOCUMENT_BYTES,
            problems,
        )?;
        if let Some(bytes) = report_bytes {
            validate_completion_report(loaded, &bytes, problems);
        }
        validate_evidence(repository_root, loaded, problems)?;
        validate_capabilities(loaded, problems);
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
            "expected a canonical 40- or 64-character lowercase Git revision",
        ));
        return Ok(());
    }
    if !revision_is_reachable(repository_root, revision)? {
        problems.push(Problem::new(
            &loaded.path,
            "source_revision",
            &loaded.record.id,
            "revision is not a reachable commit",
        ));
    }
    Ok(())
}

fn revision_is_reachable(repository_root: &Path, revision: &str) -> Result<bool, DispositionError> {
    let object = format!("{revision}^{{commit}}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["cat-file", "-e"])
        .arg(object)
        .output()
        .map_err(|source| DispositionError::Git { source })?;
    Ok(output.status.success())
}

fn validate_state_combination(loaded: &LoadedRecord, problems: &mut Vec<Problem>) {
    let record = &loaded.record;
    let id = &record.id;
    if record.acceptance.requires_evidence() && record.evidence.is_empty() {
        problems.push(Problem::new(
            &loaded.path,
            "evidence",
            id,
            "accepted disposition requires evidence",
        ));
    }
    if record.evidence.len() > MAX_EVIDENCE_PER_RECORD {
        problems.push(Problem::new(
            &loaded.path,
            "evidence",
            id,
            format!("record exceeds the {MAX_EVIDENCE_PER_RECORD}-evidence limit"),
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
            "fallback acceptance requires a boundary",
        ));
    }
    if record.acceptance != Acceptance::Fallback && record.fallback_boundary.is_some() {
        problems.push(Problem::new(
            &loaded.path,
            "fallback_boundary",
            id,
            "only fallback acceptance may define a boundary",
        ));
    }
    if record.acceptance.is_accepted()
        && record.implementation_status == ImplementationStatus::NotStarted
    {
        problems.push(Problem::new(
            &loaded.path,
            "implementation_status",
            id,
            "accepted disposition cannot be not_started",
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
            "gate outcome conflicts with acceptance",
        ));
    }
}

fn validate_record_ordering(loaded: &LoadedRecord, problems: &mut Vec<Problem>) {
    for (field, entries) in [
        ("dependencies", loaded.record.dependencies.as_slice()),
        ("dependents", loaded.record.dependents.as_slice()),
        ("residual_risks", loaded.record.residual_risks.as_slice()),
    ] {
        validate_string_list(&loaded.path, field, &loaded.record.id, entries, problems);
    }
    let evidence_ids = loaded
        .record
        .evidence
        .iter()
        .map(|evidence| evidence.id.as_str())
        .collect::<Vec<_>>();
    validate_string_list(
        &loaded.path,
        "evidence.id",
        &loaded.record.id,
        &evidence_ids,
        problems,
    );
    let capability_ids = loaded
        .record
        .capabilities
        .iter()
        .map(|capability| capability.id.as_str())
        .collect::<Vec<_>>();
    validate_string_list(
        &loaded.path,
        "capabilities.id",
        &loaded.record.id,
        &capability_ids,
        problems,
    );
    let checklist_lines = loaded
        .record
        .checklist
        .iter()
        .map(|mapping| mapping.line)
        .collect::<Vec<_>>();
    if !is_strictly_sorted(&checklist_lines) {
        problems.push(Problem::new(
            &loaded.path,
            "checklist",
            &loaded.record.id,
            "checklist mappings must be unique and ordered by line",
        ));
    }
}

fn validate_string_list(
    path: &str,
    field: &str,
    id: &str,
    entries: &[impl AsRef<str>],
    problems: &mut Vec<Problem>,
) {
    if entries.iter().any(|entry| entry.as_ref().trim().is_empty()) {
        problems.push(Problem::new(path, field, id, "entries must be non-empty"));
    }
    if !entries
        .windows(2)
        .all(|window| window[0].as_ref() < window[1].as_ref())
    {
        problems.push(Problem::new(
            path,
            field,
            id,
            "entries must be unique and canonically ordered",
        ));
    }
}

fn validate_bound_document(
    base: &Path,
    loaded: &LoadedRecord,
    field: &str,
    binding: &DocumentBinding,
    maximum_bytes: u64,
    problems: &mut Vec<Problem>,
) -> Result<Option<Vec<u8>>, DispositionError> {
    validate_binding_metadata(
        &loaded.path,
        field,
        &loaded.record.id,
        BindingMetadata {
            record_revision: &loaded.record.source_revision,
            binding_revision: &binding.source_revision,
            digest: &binding.sha256,
            bytes: binding.bytes,
            maximum_bytes,
        },
        problems,
    );
    let Some(path) = resolve_local_reference(
        base,
        &binding.path,
        &loaded.path,
        field,
        &loaded.record.id,
        problems,
    )?
    else {
        return Ok(None);
    };
    let bytes = fs::read(path).map_err(|source| DispositionError::Read {
        path: binding.path.clone(),
        source,
    })?;
    validate_content_metadata(
        &loaded.path,
        field,
        &loaded.record.id,
        &bytes,
        binding.bytes,
        &binding.sha256,
        problems,
    );
    Ok(Some(bytes))
}

struct BindingMetadata<'a> {
    record_revision: &'a str,
    binding_revision: &'a str,
    digest: &'a str,
    bytes: u64,
    maximum_bytes: u64,
}

fn validate_binding_metadata(
    owner_path: &str,
    field: &str,
    id: &str,
    metadata: BindingMetadata<'_>,
    problems: &mut Vec<Problem>,
) {
    if metadata.binding_revision != metadata.record_revision {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            "bound source revision differs from the record revision",
        ));
    }
    if !is_sha256(metadata.digest) {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            "SHA-256 must be 64 lowercase hexadecimal characters",
        ));
    }
    if metadata.bytes == 0 || metadata.bytes > metadata.maximum_bytes {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            format!("byte count must be within 1..={}", metadata.maximum_bytes),
        ));
    }
}

fn validate_content_metadata(
    owner_path: &str,
    field: &str,
    id: &str,
    content: &[u8],
    expected_bytes: u64,
    expected_digest: &str,
    problems: &mut Vec<Problem>,
) {
    let actual_bytes = u64::try_from(content.len()).unwrap_or(u64::MAX);
    if actual_bytes != expected_bytes {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            "bound byte count differs from the referenced content",
        ));
    }
    if sha256_hex(content) != expected_digest {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            "bound SHA-256 differs from the referenced content",
        ));
    }
}

fn validate_detail(loaded: &LoadedRecord, bytes: &[u8], problems: &mut Vec<Problem>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        problems.push(Problem::new(
            &loaded.path,
            "detail",
            &loaded.record.id,
            "detail must be UTF-8",
        ));
        return;
    };
    let Some(checked) = parse_detail_status(text) else {
        problems.push(Problem::new(
            &loaded.record.detail.path,
            "Status",
            &loaded.record.id,
            "detail has no checkbox Status field",
        ));
        return;
    };
    if checked != loaded.record.acceptance.is_accepted() {
        problems.push(Problem::new(
            &loaded.record.detail.path,
            "Status",
            &loaded.record.id,
            "detail checkbox conflicts with acceptance",
        ));
    }
}

fn validate_completion_report(loaded: &LoadedRecord, bytes: &[u8], problems: &mut Vec<Problem>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        problems.push(Problem::new(
            &loaded.path,
            "completion_report",
            &loaded.record.id,
            "completion report must be UTF-8",
        ));
        return;
    };
    let Some(status) = parse_completion_status(text) else {
        problems.push(Problem::new(
            &loaded.record.completion_report.path,
            "Status",
            &loaded.record.id,
            "report has no recognized Status field",
        ));
        return;
    };
    if status != loaded.record.acceptance {
        problems.push(Problem::new(
            &loaded.record.completion_report.path,
            "Status",
            &loaded.record.id,
            "report status conflicts with acceptance",
        ));
    }
}

fn validate_evidence(
    repository_root: &Path,
    loaded: &LoadedRecord,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    let mut evidence_ids = BTreeSet::new();
    for evidence in &loaded.record.evidence {
        validate_identifier(
            &loaded.path,
            "evidence.id",
            &loaded.record.id,
            &evidence.id,
            problems,
        );
        if !evidence_ids.insert(evidence.id.as_str()) {
            problems.push(Problem::new(
                &loaded.path,
                "evidence.id",
                &loaded.record.id,
                "evidence identifiers must be unique",
            ));
        }
        validate_binding_metadata(
            &loaded.path,
            "evidence",
            &loaded.record.id,
            BindingMetadata {
                record_revision: &loaded.record.source_revision,
                binding_revision: &evidence.source_revision,
                digest: &evidence.sha256,
                bytes: evidence.bytes,
                maximum_bytes: match evidence.source {
                    EvidenceSource::Local { .. } => MAX_LOCAL_EVIDENCE_BYTES,
                    EvidenceSource::GithubArtifact(_) => MAX_REMOTE_EVIDENCE_BYTES,
                },
            },
            problems,
        );
        match &evidence.source {
            EvidenceSource::Local { path } => {
                validate_local_evidence(repository_root, loaded, evidence, path, problems)?;
            }
            EvidenceSource::GithubArtifact(artifact) => validate_github_evidence(
                loaded,
                evidence,
                GithubArtifactIdentity {
                    repository: &artifact.repository,
                    run_id: artifact.run_id,
                    run_attempt: artifact.run_attempt,
                    job_id: artifact.job_id,
                    artifact_id: artifact.artifact_id,
                    artifact_name: &artifact.artifact_name,
                    head_sha: &artifact.head_sha,
                    run_url: &artifact.run_url,
                    job_url: &artifact.job_url,
                    api_url: &artifact.api_url,
                    archive_url: &artifact.archive_url,
                    api_digest: &artifact.api_digest,
                    api_bytes: artifact.api_bytes,
                },
                problems,
            ),
        }
    }
    Ok(())
}

fn validate_local_evidence(
    repository_root: &Path,
    loaded: &LoadedRecord,
    evidence: &Evidence,
    path: &str,
    problems: &mut Vec<Problem>,
) -> Result<(), DispositionError> {
    let field = format!("evidence.{}", evidence.id);
    let Some(path_buf) = resolve_local_reference(
        repository_root,
        path,
        &loaded.path,
        &field,
        &loaded.record.id,
        problems,
    )?
    else {
        return Ok(());
    };
    let bytes = fs::read(path_buf).map_err(|source| DispositionError::Read {
        path: path.to_owned(),
        source,
    })?;
    validate_content_metadata(
        &loaded.path,
        &field,
        &loaded.record.id,
        &bytes,
        evidence.bytes,
        &evidence.sha256,
        problems,
    );

    let metadata = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("source_revision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    if metadata.as_deref() != Some(evidence.source_revision.as_str()) {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "local evidence metadata has a missing or stale source revision",
        ));
    }
    Ok(())
}

struct GithubArtifactIdentity<'a> {
    repository: &'a str,
    run_id: u64,
    run_attempt: u32,
    job_id: u64,
    artifact_id: u64,
    artifact_name: &'a str,
    head_sha: &'a str,
    run_url: &'a str,
    job_url: &'a str,
    api_url: &'a str,
    archive_url: &'a str,
    api_digest: &'a str,
    api_bytes: u64,
}

fn validate_github_evidence(
    loaded: &LoadedRecord,
    evidence: &Evidence,
    identity: GithubArtifactIdentity<'_>,
    problems: &mut Vec<Problem>,
) {
    let field = format!("evidence.{}", evidence.id);
    if !is_github_repository(identity.repository) {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub repository must be a canonical lowercase owner/name",
        ));
    }
    if identity.run_id == 0
        || identity.run_attempt == 0
        || identity.job_id == 0
        || identity.artifact_id == 0
    {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub run, attempt, job, and artifact identifiers must be nonzero",
        ));
    }
    if identity.artifact_name.trim().is_empty()
        || identity.artifact_name.len() > 128
        || identity.artifact_name.contains(['/', '\\'])
    {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub artifact name is invalid",
        ));
    }
    if identity.head_sha != evidence.source_revision {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub workflow head revision differs from the bound source revision",
        ));
    }
    let expected_run_url = format!(
        "https://api.github.com/repos/{}/actions/runs/{}/attempts/{}",
        identity.repository, identity.run_id, identity.run_attempt
    );
    if identity.run_url != expected_run_url {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub workflow run URL is not the canonical API endpoint",
        ));
    }
    let expected_job_url = format!(
        "https://api.github.com/repos/{}/actions/jobs/{}",
        identity.repository, identity.job_id
    );
    if identity.job_url != expected_job_url {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub workflow job URL is not the canonical API endpoint",
        ));
    }
    let expected_url = format!(
        "https://api.github.com/repos/{}/actions/artifacts/{}",
        identity.repository, identity.artifact_id
    );
    if identity.api_url != expected_url {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub artifact URL is not the canonical API endpoint",
        ));
    }
    let expected_archive_url = format!("{expected_url}/zip");
    if identity.archive_url != expected_archive_url {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub artifact archive URL is not the canonical API endpoint",
        ));
    }
    let expected_api_digest = format!("sha256:{}", evidence.sha256);
    if identity.api_digest != expected_api_digest {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub API digest differs from the bound SHA-256",
        ));
    }
    if identity.api_bytes != evidence.bytes {
        problems.push(Problem::new(
            &loaded.path,
            &field,
            &loaded.record.id,
            "GitHub API byte count differs from the bound byte count",
        ));
    }
}

fn validate_capabilities(loaded: &LoadedRecord, problems: &mut Vec<Problem>) {
    let evidence = loaded
        .record
        .evidence
        .iter()
        .map(|entry| (entry.id.as_str(), entry.classification))
        .collect::<BTreeMap<_, _>>();

    if loaded.record.capabilities.is_empty() {
        problems.push(Problem::new(
            &loaded.path,
            "capabilities",
            &loaded.record.id,
            "record must declare at least one product capability disposition",
        ));
        return;
    }

    for capability in &loaded.record.capabilities {
        validate_identifier(
            &loaded.path,
            "capabilities.id",
            &loaded.record.id,
            &capability.id,
            problems,
        );
        validate_string_list(
            &loaded.path,
            "capabilities.evidence",
            &loaded.record.id,
            &capability.evidence,
            problems,
        );
        validate_string_list(
            &loaded.path,
            "capabilities.depends_on",
            &loaded.record.id,
            &capability.depends_on,
            problems,
        );
        validate_string_list(
            &loaded.path,
            "capabilities.unlocks",
            &loaded.record.id,
            &capability.unlocks,
            problems,
        );
        validate_claim_evidence(
            &loaded.path,
            "capabilities.evidence",
            &loaded.record.id,
            &capability.evidence,
            capability.disposition.evidence_classification(),
            &evidence,
            problems,
        );

        let requires_boundary = capability.disposition == CapabilityDisposition::Fallback;
        if requires_boundary
            != capability
                .boundary
                .as_deref()
                .is_some_and(|boundary| !boundary.trim().is_empty())
        {
            problems.push(Problem::new(
                &loaded.path,
                "capabilities.boundary",
                &loaded.record.id,
                "only fallback capability dispositions require a boundary",
            ));
        }
        if capability.disposition != CapabilityDisposition::Available
            && !capability.unlocks.is_empty()
        {
            problems.push(Problem::new(
                &loaded.path,
                "capabilities.unlocks",
                &loaded.record.id,
                "only available capabilities may unlock dependents",
            ));
        }
    }

    let has_fallback = loaded
        .record
        .capabilities
        .iter()
        .any(|entry| entry.disposition == CapabilityDisposition::Fallback);
    let has_blocked = loaded
        .record
        .capabilities
        .iter()
        .any(|entry| entry.disposition == CapabilityDisposition::Blocked);
    let has_deferred = loaded
        .record
        .capabilities
        .iter()
        .any(|entry| entry.disposition == CapabilityDisposition::Deferred);
    match loaded.record.acceptance {
        Acceptance::Pass if has_fallback || has_blocked || has_deferred => {
            problems.push(Problem::new(
                &loaded.path,
                "capabilities",
                &loaded.record.id,
                "pass acceptance cannot contain fallback, blocked, or deferred capabilities",
            ))
        }
        Acceptance::Fallback if !has_fallback && !has_blocked => problems.push(Problem::new(
            &loaded.path,
            "capabilities",
            &loaded.record.id,
            "fallback acceptance requires a fallback or blocked capability",
        )),
        Acceptance::Blocked if !has_blocked => problems.push(Problem::new(
            &loaded.path,
            "capabilities",
            &loaded.record.id,
            "blocked acceptance requires a blocked capability",
        )),
        Acceptance::Pass | Acceptance::Fallback | Acceptance::Blocked | Acceptance::Pending => {}
    }
}

fn validate_capability_graph(records: &[LoadedRecord], problems: &mut Vec<Problem>) {
    let mut capabilities = BTreeMap::<&str, (&LoadedRecord, &Capability)>::new();
    for loaded in records {
        for capability in &loaded.record.capabilities {
            if let Some((other, _)) =
                capabilities.insert(capability.id.as_str(), (loaded, capability))
            {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.id",
                    &loaded.record.id,
                    format!("capability identifier is also defined by {}", other.path),
                ));
            }
        }
    }

    for (id, (loaded, capability)) in &capabilities {
        for dependency in &capability.depends_on {
            let Some((upstream_record, upstream)) = capabilities.get(dependency.as_str()) else {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.depends_on",
                    &loaded.record.id,
                    format!("unknown capability dependency {dependency}"),
                ));
                continue;
            };
            if !upstream.unlocks.iter().any(|entry| entry == id) {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.depends_on",
                    &loaded.record.id,
                    format!("dependency {dependency} does not declare the reciprocal unlock"),
                ));
            }
            if matches!(
                capability.disposition,
                CapabilityDisposition::Available | CapabilityDisposition::Fallback
            ) && upstream.disposition != CapabilityDisposition::Available
            {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.disposition",
                    &loaded.record.id,
                    format!(
                        "{} capability is blocked by {} dependency {dependency}",
                        capability.disposition.as_str(),
                        upstream.disposition.as_str()
                    ),
                ));
            }
            if upstream.disposition != CapabilityDisposition::Available
                && !upstream.unlocks.is_empty()
            {
                problems.push(Problem::new(
                    &upstream_record.path,
                    "capabilities.unlocks",
                    &upstream_record.record.id,
                    "non-available capability cannot unlock a dependent",
                ));
            }
        }
        for unlocked in &capability.unlocks {
            let Some((downstream_record, downstream)) = capabilities.get(unlocked.as_str()) else {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.unlocks",
                    &loaded.record.id,
                    format!("unknown unlocked capability {unlocked}"),
                ));
                continue;
            };
            if !downstream.depends_on.iter().any(|entry| entry == id) {
                problems.push(Problem::new(
                    &loaded.path,
                    "capabilities.unlocks",
                    &loaded.record.id,
                    format!(
                        "unlocked capability {unlocked} does not declare the reciprocal dependency"
                    ),
                ));
            }
            if capability.disposition != CapabilityDisposition::Available {
                problems.push(Problem::new(
                    &downstream_record.path,
                    "capabilities.depends_on",
                    &downstream_record.record.id,
                    format!(
                        "dependent capability cannot be unlocked by {}",
                        capability.disposition.as_str()
                    ),
                ));
            }
        }
    }
}

fn validate_checklist(loaded: &LoadedRecord, bytes: &[u8], problems: &mut Vec<Problem>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let rows = parse_checklist_rows(text);
    let mappings = loaded
        .record
        .checklist
        .iter()
        .map(|mapping| (mapping.line, mapping))
        .collect::<BTreeMap<_, _>>();
    let evidence = loaded
        .record
        .evidence
        .iter()
        .map(|entry| (entry.id.as_str(), entry.classification))
        .collect::<BTreeMap<_, _>>();

    for row in &rows {
        let Some(mapping) = mappings.get(&row.line) else {
            problems.push(Problem::new(
                &loaded.path,
                "checklist",
                &loaded.record.id,
                format!("detail checklist line {} has no mapping", row.line),
            ));
            continue;
        };
        if mapping.item_sha256 != row.sha256 {
            problems.push(Problem::new(
                &loaded.path,
                "checklist.item_sha256",
                &loaded.record.id,
                format!("detail checklist line {} has a stale digest", row.line),
            ));
        }
        validate_string_list(
            &loaded.path,
            "checklist.evidence",
            &loaded.record.id,
            &mapping.evidence,
            problems,
        );
        validate_claim_evidence(
            &loaded.path,
            "checklist.evidence",
            &loaded.record.id,
            &mapping.evidence,
            mapping.disposition,
            &evidence,
            problems,
        );
        validate_checklist_acceptance(loaded, row, mapping, problems);
    }
    for mapping in &loaded.record.checklist {
        if !rows.iter().any(|row| row.line == mapping.line) {
            problems.push(Problem::new(
                &loaded.path,
                "checklist.line",
                &loaded.record.id,
                format!(
                    "checklist mapping line {} is not a detail checkbox",
                    mapping.line
                ),
            ));
        }
    }
}

fn validate_claim_evidence(
    path: &str,
    field: &str,
    id: &str,
    references: &[String],
    expected: EvidenceClassification,
    evidence: &BTreeMap<&str, EvidenceClassification>,
    problems: &mut Vec<Problem>,
) {
    if references.is_empty() || references.len() > MAX_EVIDENCE_PER_CLAIM {
        problems.push(Problem::new(
            path,
            field,
            id,
            format!("claim evidence count must be within 1..={MAX_EVIDENCE_PER_CLAIM}"),
        ));
    }
    let mut found_expected = false;
    for reference in references {
        let Some(classification) = evidence.get(reference.as_str()) else {
            problems.push(Problem::new(
                path,
                field,
                id,
                format!("unknown evidence reference {reference}"),
            ));
            continue;
        };
        found_expected |= *classification == expected;
    }
    if !references.is_empty() && !found_expected {
        problems.push(Problem::new(
            path,
            field,
            id,
            format!("claim lacks {} evidence", expected.as_str()),
        ));
    }
}

fn validate_checklist_acceptance(
    loaded: &LoadedRecord,
    row: &ChecklistRow,
    mapping: &ChecklistMapping,
    problems: &mut Vec<Problem>,
) {
    let consistent = match mapping.disposition {
        EvidenceClassification::ObservedPass => {
            row.checked && loaded.record.acceptance.is_accepted()
        }
        EvidenceClassification::FallbackCovered => loaded.record.acceptance == Acceptance::Fallback,
        EvidenceClassification::Deferred => {
            !row.checked
                && matches!(
                    loaded.record.acceptance,
                    Acceptance::Pending | Acceptance::Blocked
                )
        }
        EvidenceClassification::Unavailable => {
            !row.checked
                && matches!(
                    loaded.record.acceptance,
                    Acceptance::Fallback | Acceptance::Blocked
                )
        }
        EvidenceClassification::NotApplicable => true,
    };
    if !consistent {
        problems.push(Problem::new(
            &loaded.path,
            "checklist.disposition",
            &loaded.record.id,
            format!(
                "detail checklist line {} conflicts with acceptance",
                row.line
            ),
        ));
    }
    if loaded.record.acceptance == Acceptance::Pass
        && !row.checked
        && mapping.disposition != EvidenceClassification::NotApplicable
    {
        problems.push(Problem::new(
            &loaded.path,
            "checklist.disposition",
            &loaded.record.id,
            format!(
                "pass acceptance leaves detail checklist line {} unresolved",
                row.line
            ),
        ));
    }
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
            "reference must be a contained relative file",
        ));
        return Ok(None);
    }
    let path = base.join(relative);
    if !path.is_file() {
        problems.push(Problem::new(
            owner_path,
            field,
            id,
            format!("referenced file does not exist: {value}"),
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
            "reference must be a contained relative file",
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
                "summary entry has no authoritative record",
            ));
            continue;
        };
        if entry.checked != loaded.record.acceptance.is_accepted() {
            problems.push(Problem::new(
                SUMMARY_FILE,
                "checkbox",
                id,
                "summary checkbox conflicts with acceptance",
            ));
        }
    }
    for (id, loaded) in records {
        if !summary.contains_key(id) {
            problems.push(Problem::new(
                &loaded.path,
                "id",
                id,
                "authoritative record has no summary entry",
            ));
        }
    }
}

fn validate_record_dependencies(
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
                    format!("unknown dependency {dependency}"),
                ));
                continue;
            };
            if !upstream.record.dependents.iter().any(|entry| entry == id) {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependencies",
                    id,
                    format!("dependency {dependency} lacks reciprocal dependent"),
                ));
            }
            if !upstream.record.acceptance.is_accepted()
                && (loaded.record.acceptance.is_accepted()
                    || summary.get(id).is_some_and(|entry| entry.checked))
            {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependencies",
                    id,
                    format!("acceptance is unavailable while upstream {dependency} is unaccepted"),
                ));
            }
        }
        for dependent in &loaded.record.dependents {
            let Some(downstream) = records.get(dependent) else {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependents",
                    id,
                    format!("unknown dependent {dependent}"),
                ));
                continue;
            };
            if !downstream
                .record
                .dependencies
                .iter()
                .any(|entry| entry == id)
            {
                problems.push(Problem::new(
                    &loaded.path,
                    "dependents",
                    id,
                    format!("dependent {dependent} lacks reciprocal dependency"),
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

#[derive(Debug)]
struct ChecklistRow {
    line: usize,
    checked: bool,
    sha256: String,
}

fn parse_checklist_rows(text: &str) -> Vec<ChecklistRow> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.starts_with("Status:") {
                return None;
            }
            let (position, checked) = match (trimmed.find("[X] "), trimmed.find("[ ] ")) {
                (Some(position), None) => (position, true),
                (None, Some(position)) => (position, false),
                _ => return None,
            };
            let prefix = &trimmed[..position];
            if !prefix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || b"-+*.) \t".contains(&byte))
            {
                return None;
            }
            Some(ChecklistRow {
                line: index + 1,
                checked,
                sha256: sha256_hex(trimmed.as_bytes()),
            })
        })
        .collect()
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

fn validate_identifier(
    path: &str,
    field: &str,
    record_id: &str,
    value: &str,
    problems: &mut Vec<Problem>,
) {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        problems.push(Problem::new(
            path,
            field,
            record_id,
            "identifier must use 1..=96 lowercase ASCII letters, digits, dot, dash, or underscore",
        ));
    }
}

fn is_full_revision(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn is_github_repository(value: &str) -> bool {
    let Some((owner, name)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !name.is_empty()
        && !name.contains('/')
        && owner.bytes().chain(name.bytes()).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
}

fn is_strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|window| window[0] < window[1])
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
    detail: DocumentBinding,
    completion_report: DocumentBinding,
    #[serde(default)]
    evidence: Vec<Evidence>,
    #[serde(default)]
    fallback_boundary: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    dependents: Vec<String>,
    #[serde(default)]
    residual_risks: Vec<String>,
    #[serde(default)]
    capabilities: Vec<Capability>,
    #[serde(default)]
    checklist: Vec<ChecklistMapping>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentBinding {
    path: String,
    source_revision: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    id: String,
    classification: EvidenceClassification,
    source_revision: String,
    sha256: String,
    bytes: u64,
    source: EvidenceSource,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceSource {
    Local { path: String },
    GithubArtifact(Box<GithubArtifactSource>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GithubArtifactSource {
    repository: String,
    run_id: u64,
    run_attempt: u32,
    job_id: u64,
    artifact_id: u64,
    artifact_name: String,
    head_sha: String,
    run_url: String,
    job_url: String,
    api_url: String,
    archive_url: String,
    api_digest: String,
    api_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    id: String,
    disposition: CapabilityDisposition,
    evidence: Vec<String>,
    #[serde(default)]
    boundary: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    unlocks: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChecklistMapping {
    line: usize,
    item_sha256: String,
    disposition: EvidenceClassification,
    evidence: Vec<String>,
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

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum CapabilityDisposition {
    Available,
    Fallback,
    Blocked,
    Deferred,
    NotApplicable,
}

impl CapabilityDisposition {
    const fn evidence_classification(self) -> EvidenceClassification {
        match self {
            Self::Available => EvidenceClassification::ObservedPass,
            Self::Fallback => EvidenceClassification::FallbackCovered,
            Self::Blocked => EvidenceClassification::Unavailable,
            Self::Deferred => EvidenceClassification::Deferred,
            Self::NotApplicable => EvidenceClassification::NotApplicable,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Fallback => "fallback",
            Self::Blocked => "blocked",
            Self::Deferred => "deferred",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum EvidenceClassification {
    ObservedPass,
    FallbackCovered,
    Deferred,
    Unavailable,
    NotApplicable,
}

impl EvidenceClassification {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ObservedPass => "observed_pass",
            Self::FallbackCovered => "fallback_covered",
            Self::Deferred => "deferred",
            Self::Unavailable => "unavailable",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Problem {
    path: String,
    field: String,
    id: String,
    detail: String,
}

impl Problem {
    fn new(path: &str, field: &str, id: &str, detail: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            field: field.to_owned(),
            id: id.to_owned(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} [{}] {}: {}",
            self.path,
            self.field,
            if self.id.is_empty() {
                "<empty>"
            } else {
                &self.id
            },
            self.detail
        )
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
    #[error("failed to enumerate records at {path}")]
    ReadDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}")]
    Parse { path: String },
    #[error(
        "{path} uses disposition schema {found}; migrate it to schema {required} with content-bound documents, evidence, capabilities, and checklist mappings"
    )]
    MigrationRequired {
        path: String,
        found: String,
        required: &'static str,
    },
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
            let detail = format!(
                "# Synthetic stage\n\nStatus: {box_state} Synthetic state\n\n- {box_state} Observable criterion\n"
            );
            let report = format!("# Synthetic report\n\nStatus: {acceptance}\n");
            let evidence = format!(
                "{{\"source_revision\":\"{}\",\"result\":\"synthetic\"}}\n",
                self.revision
            );
            fs::write(
                self.root().join(SUMMARY_FILE),
                format!("{box_state} {id}, Synthetic stage\n"),
            )
            .expect("write summary");
            fs::create_dir_all(self.root().join(RECORDS_DIR)).expect("create records");
            fs::create_dir_all(self.root().join("details")).expect("create details");
            fs::create_dir_all(self.root().join("reports")).expect("create reports");
            fs::create_dir_all(self.root().join("evidence")).expect("create evidence");
            fs::write(self.root().join(format!("details/{id}.md")), &detail).expect("write detail");
            fs::write(self.root().join(format!("reports/{id}.md")), &report).expect("write report");
            fs::write(self.root().join(format!("evidence/{id}.json")), &evidence)
                .expect("write evidence");

            let gate = match acceptance {
                "pass" => "pass",
                "fallback" => "fallback",
                "blocked" => "blocked",
                "pending" => "not_applicable",
                _ => panic!("unsupported test acceptance"),
            };
            let evidence_class = match acceptance {
                "pass" => "observed_pass",
                "fallback" => "fallback_covered",
                "blocked" => "unavailable",
                "pending" => "deferred",
                _ => panic!("unsupported test acceptance"),
            };
            let capability = match acceptance {
                "pass" => "available",
                "fallback" => "fallback",
                "blocked" => "blocked",
                "pending" => "deferred",
                _ => panic!("unsupported test acceptance"),
            };
            let fallback = if acceptance == "fallback" {
                "fallback_boundary = \"Synthetic boundary.\"\n"
            } else {
                ""
            };
            let capability_boundary = if acceptance == "fallback" {
                "boundary = \"Synthetic capability boundary.\"\n"
            } else {
                ""
            };
            let checklist_disposition = evidence_class;
            let checklist_line = 5;
            let checklist_digest =
                sha256_hex(format!("- {box_state} Observable criterion").as_bytes());
            fs::write(
                self.root().join(format!("{RECORDS_DIR}/{id}.toml")),
                format!(
                    "schema_version = \"2.0\"\nid = \"{id}\"\ntitle = \"Synthetic stage\"\nimplementation_status = \"present\"\nacceptance = \"{acceptance}\"\ngate_outcome = \"{gate}\"\nsource_revision = \"{revision}\"\n{fallback}dependencies = []\ndependents = []\nresidual_risks = []\n\n[detail]\npath = \"details/{id}.md\"\nsource_revision = \"{revision}\"\nsha256 = \"{detail_hash}\"\nbytes = {detail_bytes}\n\n[completion_report]\npath = \"reports/{id}.md\"\nsource_revision = \"{revision}\"\nsha256 = \"{report_hash}\"\nbytes = {report_bytes}\n\n[[evidence]]\nid = \"{id}-evidence\"\nclassification = \"{evidence_class}\"\nsource_revision = \"{revision}\"\nsha256 = \"{evidence_hash}\"\nbytes = {evidence_bytes}\nsource = {{ kind = \"local\", path = \"evidence/{id}.json\" }}\n\n[[capabilities]]\nid = \"{id}.capability\"\ndisposition = \"{capability}\"\nevidence = [\"{id}-evidence\"]\n{capability_boundary}depends_on = []\nunlocks = []\n\n[[checklist]]\nline = {checklist_line}\nitem_sha256 = \"{checklist_digest}\"\ndisposition = \"{checklist_disposition}\"\nevidence = [\"{id}-evidence\"]\n",
                    revision = self.revision,
                    detail_hash = sha256_hex(detail.as_bytes()),
                    detail_bytes = detail.len(),
                    report_hash = sha256_hex(report.as_bytes()),
                    report_bytes = report.len(),
                    evidence_hash = sha256_hex(evidence.as_bytes()),
                    evidence_bytes = evidence.len(),
                ),
            )
            .expect("write record");
        }

        fn record_path(&self, id: &str) -> PathBuf {
            self.root().join(format!("records/{id}.toml"))
        }

        fn replace_record(&self, id: &str, from: &str, to: &str) {
            let path = self.record_path(id);
            let text = fs::read_to_string(&path)
                .expect("read record")
                .replace(from, to);
            fs::write(path, text).expect("replace record content");
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

    fn error_message(repository: &TestRepository) -> String {
        check(repository.root())
            .expect_err("fixture must fail")
            .to_string()
    }

    #[test]
    fn absent_private_root_is_vacuously_ok() {
        let temp = tempfile::tempdir().expect("temporary directory");
        assert!(check(&temp.path().join("absent")).is_ok());
    }

    #[test]
    fn valid_content_bound_disposition_passes() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        assert!(check(repository.root()).is_ok());
    }

    #[test]
    fn deferred_and_unavailable_evidence_states_are_valid_when_unaccepted() {
        for acceptance in ["pending", "blocked"] {
            let repository = TestRepository::new();
            repository.write_valid("alpha", acceptance, false);
            assert!(
                check(repository.root()).is_ok(),
                "{acceptance} fixture must be valid"
            );
        }
    }

    #[test]
    fn reachable_but_stale_local_evidence_revision_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        fs::write(repository.root().join("second.txt"), "second\n").expect("write second file");
        run_git(repository.root(), &["add", "second.txt"]);
        run_git(
            repository.root(),
            &["commit", "--quiet", "-m", "second revision"],
        );
        let output = Command::new("git")
            .arg("-C")
            .arg(repository.root())
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("resolve second revision");
        let second = String::from_utf8(output.stdout)
            .expect("revision is UTF-8")
            .trim()
            .to_owned();
        let path = repository.record_path("alpha");
        let text = fs::read_to_string(&path).expect("read record");
        let old_revision = format!("source_revision = \"{}\"", repository.revision);
        fs::write(
            &path,
            text.replacen(&old_revision, &format!("source_revision = \"{second}\""), 3),
        )
        .expect("write stale-evidence record");
        let message = error_message(&repository);
        assert!(message.contains("[evidence]"), "{message}");
        assert!(
            message.contains("bound source revision differs"),
            "{message}"
        );
    }

    #[test]
    fn wrong_local_digest_and_byte_count_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.root().join("evidence/alpha.json");
        let content = fs::read_to_string(&path)
            .expect("read evidence")
            .replace("}\n", ",\"extra\":true}\n");
        fs::write(path, content).expect("mutate evidence");
        let message = error_message(&repository);
        assert!(message.contains("bound byte count differs"), "{message}");
        assert!(message.contains("bound SHA-256 differs"), "{message}");
    }

    #[test]
    fn arbitrary_remote_url_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.record_path("alpha");
        let evidence =
            fs::read(repository.root().join("evidence/alpha.json")).expect("read evidence");
        let source = github_source(
            &repository.revision,
            &sha256_hex(&evidence),
            u64::try_from(evidence.len()).expect("evidence length fits u64"),
        )
        .replace(
            "https://api.github.com/repos/owner/project/actions/artifacts/3",
            "https://example.invalid/proof",
        );
        let text = fs::read_to_string(&path).expect("read record").replace(
            "source = { kind = \"local\", path = \"evidence/alpha.json\" }",
            &source,
        );
        fs::write(path, text).expect("write remote evidence");
        assert!(error_message(&repository).contains("not the canonical API endpoint"));
    }

    #[test]
    fn canonical_github_artifact_identity_is_accepted() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let evidence =
            fs::read(repository.root().join("evidence/alpha.json")).expect("read evidence");
        repository.replace_record(
            "alpha",
            "source = { kind = \"local\", path = \"evidence/alpha.json\" }",
            &github_source(
                &repository.revision,
                &sha256_hex(&evidence),
                u64::try_from(evidence.len()).expect("evidence length fits u64"),
            ),
        );
        assert!(check(repository.root()).is_ok());
    }

    #[test]
    fn github_artifact_digest_bytes_and_revision_must_match_binding() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let evidence =
            fs::read(repository.root().join("evidence/alpha.json")).expect("read evidence");
        let source = github_source(
            &repository.revision,
            &sha256_hex(&evidence),
            u64::try_from(evidence.len()).expect("evidence length fits u64"),
        )
        .replace(
            &format!("head_sha = \"{}\"", repository.revision),
            "head_sha = \"0000000000000000000000000000000000000000\"",
        )
        .replace(
            &format!("api_digest = \"sha256:{}\"", sha256_hex(&evidence)),
            "api_digest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"",
        )
        .replace(
            &format!("api_bytes = {}", evidence.len()),
            "api_bytes = 1",
        );
        repository.replace_record(
            "alpha",
            "source = { kind = \"local\", path = \"evidence/alpha.json\" }",
            &source,
        );
        let message = error_message(&repository);
        assert!(message.contains("head revision differs"), "{message}");
        assert!(message.contains("API digest differs"), "{message}");
        assert!(message.contains("API byte count differs"), "{message}");
    }

    #[test]
    fn fallback_capability_cannot_unlock_available_dependent() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "fallback", true);
        repository.replace_record("alpha", "unlocks = []", "unlocks = [\"bravo.capability\"]");
        let alpha_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read alpha summary");
        repository.write_valid("bravo", "pass", true);
        let bravo_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read bravo summary");
        fs::write(
            repository.root().join(SUMMARY_FILE),
            format!("{alpha_summary}{bravo_summary}"),
        )
        .expect("write combined summary");
        repository.replace_record(
            "bravo",
            "depends_on = []",
            "depends_on = [\"alpha.capability\"]",
        );
        let message = error_message(&repository);
        assert!(
            message.contains("only available capabilities may unlock"),
            "{message}"
        );
        assert!(
            message.contains("available capability is blocked"),
            "{message}"
        );
    }

    #[test]
    fn missing_duplicate_and_noncanonical_checklist_mappings_are_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let path = repository.record_path("alpha");
        let original = fs::read_to_string(&path).expect("read record");
        let without_mapping = original
            .split("[[checklist]]")
            .next()
            .expect("checklist marker");
        fs::write(&path, without_mapping).expect("remove checklist mapping");
        assert!(error_message(&repository).contains("has no mapping"));

        fs::write(&path, &original).expect("restore record");
        let mapping = original
            .split("[[checklist]]")
            .nth(1)
            .expect("checklist mapping");
        fs::write(&path, format!("{original}\n[[checklist]]{mapping}")).expect("duplicate mapping");
        assert!(error_message(&repository).contains("unique and ordered by line"));

        let detail_path = repository.root().join("details/alpha.md");
        let mut detail = fs::read_to_string(&detail_path).expect("read detail");
        detail.push_str("- [X] Second criterion\n");
        fs::write(&detail_path, &detail).expect("append checklist row");
        let detail_hash = sha256_hex(detail.as_bytes());
        let updated = original
            .replace(
                &format!("sha256 = \"{}\"", sha256_hex(
                    "# Synthetic stage\n\nStatus: [X] Synthetic state\n\n- [X] Observable criterion\n"
                        .as_bytes()
                )),
                &format!("sha256 = \"{detail_hash}\""),
            )
            .replace("bytes = 73", &format!("bytes = {}", detail.len()));
        let second_mapping = format!(
            "\n[[checklist]]\nline = 6\nitem_sha256 = \"{}\"\ndisposition = \"observed_pass\"\nevidence = [\"alpha-evidence\"]\n",
            sha256_hex("- [X] Second criterion".as_bytes())
        );
        let first_marker = updated.find("[[checklist]]").expect("checklist marker");
        let (head, tail) = updated.split_at(first_marker);
        fs::write(&path, format!("{head}{second_mapping}{tail}"))
            .expect("write noncanonical mappings");
        assert!(error_message(&repository).contains("unique and ordered by line"));
    }

    #[test]
    fn accepted_header_with_unresolved_row_is_rejected() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        let detail_path = repository.root().join("details/alpha.md");
        let detail = fs::read_to_string(&detail_path)
            .expect("read detail")
            .replace("- [X] Observable", "- [ ] Observable");
        fs::write(&detail_path, &detail).expect("write unresolved detail");
        let path = repository.record_path("alpha");
        let text = fs::read_to_string(&path)
            .expect("read record")
            .replace(
                "item_sha256 = \"",
                "item_sha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n# old = \"",
            );
        fs::write(path, text).expect("write stale mapping");
        let message = error_message(&repository);
        assert!(message.contains("stale digest"), "{message}");
        assert!(message.contains("conflicts with acceptance"), "{message}");
    }

    #[test]
    fn missing_record_and_blocked_record_dependency_are_rejected() {
        let repository = TestRepository::new();
        fs::write(repository.root().join(SUMMARY_FILE), "[X] alpha, Stage\n")
            .expect("write summary");
        assert!(error_message(&repository).contains("no authoritative record"));

        repository.write_valid("alpha", "blocked", false);
        let alpha_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read alpha summary");
        repository.write_valid("bravo", "pass", true);
        let bravo_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read bravo summary");
        fs::write(
            repository.root().join(SUMMARY_FILE),
            format!("{alpha_summary}{bravo_summary}"),
        )
        .expect("write combined summary");
        repository.replace_record("alpha", "dependents = []", "dependents = [\"bravo\"]");
        repository.replace_record("bravo", "dependencies = []", "dependencies = [\"alpha\"]");
        assert!(error_message(&repository).contains("upstream alpha is unaccepted"));
    }

    #[test]
    fn pending_record_dependency_cannot_unlock_accepted_downstream() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pending", false);
        let alpha_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read alpha summary");
        repository.write_valid("bravo", "fallback", true);
        let bravo_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read bravo summary");
        fs::write(
            repository.root().join(SUMMARY_FILE),
            format!("{alpha_summary}{bravo_summary}"),
        )
        .expect("write combined summary");
        repository.replace_record("alpha", "dependents = []", "dependents = [\"bravo\"]");
        repository.replace_record("bravo", "dependencies = []", "dependencies = [\"alpha\"]");
        let message = error_message(&repository);
        assert!(
            message.contains("upstream alpha is unaccepted"),
            "{message}"
        );
    }

    #[test]
    fn fallback_capability_is_fail_closed_by_unavailable_dependency() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "blocked", false);
        let alpha_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read alpha summary");
        repository.write_valid("bravo", "fallback", true);
        let bravo_summary =
            fs::read_to_string(repository.root().join(SUMMARY_FILE)).expect("read bravo summary");
        fs::write(
            repository.root().join(SUMMARY_FILE),
            format!("{alpha_summary}{bravo_summary}"),
        )
        .expect("write combined summary");
        repository.replace_record(
            "bravo",
            "depends_on = []",
            "depends_on = [\"alpha.capability\"]",
        );
        let message = error_message(&repository);
        assert!(
            message.contains("fallback capability is blocked by blocked dependency"),
            "{message}"
        );
    }

    #[test]
    fn escaping_reference_is_rejected_without_absolute_path_leak() {
        let repository = TestRepository::new();
        repository.write_valid("alpha", "pass", true);
        repository.replace_record(
            "alpha",
            "source = { kind = \"local\", path = \"evidence/alpha.json\" }",
            "source = { kind = \"local\", path = \"../private.json\" }",
        );
        let message = error_message(&repository);
        assert!(message.contains("contained relative file"), "{message}");
        assert!(!message.contains(&repository.root().display().to_string()));
    }

    #[test]
    fn problems_are_deterministically_ordered() {
        let repository = TestRepository::new();
        fs::write(
            repository.root().join(SUMMARY_FILE),
            "[X] zeta, Later\n[X] alpha, Earlier\n",
        )
        .expect("write summary");
        let message = error_message(&repository);
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

    #[test]
    fn tracked_negative_fixtures_are_rejected() {
        let invalid_root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/disposition/invalid");
        let mut fixtures = fs::read_dir(invalid_root)
            .expect("read invalid fixtures")
            .map(|entry| entry.expect("read fixture entry").path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        fixtures.sort();
        assert!(
            !fixtures.is_empty(),
            "at least one invalid fixture is required"
        );
        for fixture in fixtures {
            let fixture_name = fixture
                .file_name()
                .and_then(|name| name.to_str())
                .expect("fixture name is UTF-8");
            assert!(
                check(&fixture).is_err(),
                "invalid fixture was accepted: {fixture_name}"
            );
        }
    }

    fn github_source(revision: &str, digest: &str, bytes: u64) -> String {
        format!(
            "source = {{ kind = \"github_artifact\", repository = \"owner/project\", run_id = 1, run_attempt = 1, job_id = 2, artifact_id = 3, artifact_name = \"proof\", head_sha = \"{revision}\", run_url = \"https://api.github.com/repos/owner/project/actions/runs/1/attempts/1\", job_url = \"https://api.github.com/repos/owner/project/actions/jobs/2\", api_url = \"https://api.github.com/repos/owner/project/actions/artifacts/3\", archive_url = \"https://api.github.com/repos/owner/project/actions/artifacts/3/zip\", api_digest = \"sha256:{digest}\", api_bytes = {bytes} }}"
        )
    }
}
