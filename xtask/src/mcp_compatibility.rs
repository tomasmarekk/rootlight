//! Offline MCP compatibility evidence generation and verification.
//!
//! The checked-in current snapshot is reproducible from the public contract
//! crate. Published release snapshots are append-only and become required
//! automatically as reviewed release-history rows are added.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rootlight_mcp_contract::accounting::tool_list_payload;
use rootlight_mcp_contract::batch::BATCH_TOOL_REGISTRY;
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};
use rootlight_mcp_contract::vertical::VerticalTool;
use rootlight_mcp_contract::{ErrorCode, MCP_SPECIFICATION_DATE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const HISTORY_FILE: &str = "release-history-v1.json";
const MANIFEST_FILE: &str = "immutable-manifest-v1.json";
const HISTORY_SCHEMA: &str = "rootlight.mcp-release-history/1";
const MANIFEST_SCHEMA: &str = "rootlight.mcp-compatibility-manifest/1";
const CURRENT_SCHEMA: &str = "rootlight.mcp-compatibility-current/1";
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 256;
const CURRENT_FILES: [&str; 6] = [
    "current/catalog.json",
    "current/profiles.json",
    "current/batch.json",
    "current/success-examples.json",
    "current/error-examples.json",
    "current/policy.json",
];

/// Command-line options for the compatibility gate.
pub(crate) struct Options {
    fixture_root: Option<PathBuf>,
    refresh_current: bool,
}

impl Options {
    /// Parses an optional fixture root and explicit snapshot refresh mode.
    pub(crate) fn parse(
        args: &mut impl Iterator<Item = String>,
    ) -> Result<Self, CompatibilityError> {
        let mut fixture_root = None;
        let mut refresh_current = false;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--fixture-root" if fixture_root.is_none() => {
                    fixture_root = Some(PathBuf::from(
                        args.next()
                            .ok_or(CompatibilityError::MissingArgument("--fixture-root"))?,
                    ));
                }
                "--refresh-current" if !refresh_current => refresh_current = true,
                _ => return Err(CompatibilityError::UnexpectedArgument(argument)),
            }
        }
        Ok(Self {
            fixture_root,
            refresh_current,
        })
    }
}

/// Refreshes explicitly requested current evidence, then verifies all evidence.
pub(crate) fn check(options: &Options) -> Result<(), CompatibilityError> {
    let root = options
        .fixture_root
        .clone()
        .unwrap_or_else(default_fixture_root);
    if options.refresh_current {
        refresh_current(&root)?;
    }
    let summary = verify(&root)?;
    println!(
        "MCP compatibility evidence verified: {} files, {} tools, {} required prior minor snapshot(s), owner review {}",
        summary.file_count, summary.tool_count, summary.required_prior_minors, summary.owner_review
    );
    Ok(())
}

fn default_fixture_root() -> PathBuf {
    let relative = Path::new("tests/fixtures/mcp/compatibility");
    let mut candidate = std::env::current_dir().unwrap_or_default();
    for _ in 0..8 {
        let fixture = candidate.join(relative);
        if fixture.is_dir() {
            return fixture;
        }
        if !candidate.pop() {
            break;
        }
    }
    relative.to_path_buf()
}

fn refresh_current(root: &Path) -> Result<(), CompatibilityError> {
    let history_path = root.join(HISTORY_FILE);
    let history_bytes = read_bounded(&history_path)?;
    let history: ReleaseHistory = parse_json(&history_path, &history_bytes)?;
    validate_history(&history)?;
    validate_release_refs(root, &history)?;

    let current = root.join("current");
    fs::create_dir_all(&current).map_err(|source| CompatibilityError::Io {
        path: current.clone(),
        source,
    })?;
    for (relative, value) in expected_current(root)? {
        write_json(&root.join(relative), &value)?;
    }

    let manifest_path = root.join(MANIFEST_FILE);
    if manifest_path.exists() {
        fs::remove_file(&manifest_path).map_err(|source| CompatibilityError::Io {
            path: manifest_path.clone(),
            source,
        })?;
    }
    let files = collect_files(root)?;
    let entries = files
        .into_iter()
        .map(|relative| {
            let bytes = read_bounded(&root.join(&relative))?;
            Ok(ManifestEntry {
                path: path_text(&relative)?,
                bytes: u64::try_from(bytes.len()).map_err(|_| {
                    CompatibilityError::FixtureContract("fixture length does not fit u64".into())
                })?,
                sha256: sha256(&bytes),
            })
        })
        .collect::<Result<Vec<_>, CompatibilityError>>()?;
    let manifest = ImmutableManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        algorithm: "sha256".to_owned(),
        release_history_sha256: sha256(&history_bytes),
        files: entries,
    };
    write_json(&manifest_path, &manifest)?;
    Ok(())
}

fn verify(root: &Path) -> Result<VerificationSummary, CompatibilityError> {
    let history_path = root.join(HISTORY_FILE);
    let history_bytes = read_bounded(&history_path)?;
    let history: ReleaseHistory = parse_json(&history_path, &history_bytes)?;
    validate_history(&history)?;
    validate_release_refs(root, &history)?;

    let manifest_path = root.join(MANIFEST_FILE);
    let manifest_bytes = read_bounded(&manifest_path)?;
    let manifest: ImmutableManifest = parse_json(&manifest_path, &manifest_bytes)?;
    validate_manifest(&manifest, &history_bytes)?;

    let actual_files = collect_files(root)?;
    let actual_paths = actual_files
        .iter()
        .map(|path| path_text(path))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let expected_paths = manifest
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    if actual_paths != expected_paths {
        return Err(CompatibilityError::FileSet {
            missing: expected_paths.difference(&actual_paths).cloned().collect(),
            unexpected: actual_paths.difference(&expected_paths).cloned().collect(),
        });
    }
    for entry in &manifest.files {
        validate_relative_path(&entry.path)?;
        let bytes = read_bounded(&root.join(&entry.path))?;
        let observed_bytes = u64::try_from(bytes.len()).map_err(|_| {
            CompatibilityError::FixtureContract("fixture length does not fit u64".into())
        })?;
        let observed_sha256 = sha256(&bytes);
        if observed_bytes != entry.bytes || observed_sha256 != entry.sha256 {
            return Err(CompatibilityError::Digest {
                path: entry.path.clone(),
                expected_bytes: entry.bytes,
                observed_bytes,
                expected_sha256: entry.sha256.clone(),
                observed_sha256,
            });
        }
    }

    let expected_current = expected_current(root)?;
    for (relative, expected) in expected_current {
        let path = root.join(relative);
        let bytes = read_bounded(&path)?;
        let observed: Value = parse_json(&path, &bytes)?;
        if observed != expected {
            return Err(CompatibilityError::CurrentDrift {
                path: relative.to_owned(),
            });
        }
    }
    validate_release_snapshots(root, &history, &expected_paths)?;

    Ok(VerificationSummary {
        file_count: manifest.files.len(),
        tool_count: McpTool::ALL.len(),
        required_prior_minors: required_prior_minors(history.releases.len()),
        owner_review: history.owner_review.status_name(),
    })
}

fn expected_current(root: &Path) -> Result<Vec<(&'static str, Value)>, CompatibilityError> {
    Ok(vec![
        (
            CURRENT_FILES[0],
            json!({
                "schema": CURRENT_SCHEMA,
                "mcp_specification_date": MCP_SPECIFICATION_DATE,
                "catalog": tool_list_payload(ExposureProfile::Developer),
            }),
        ),
        (CURRENT_FILES[1], profile_snapshot()),
        (CURRENT_FILES[2], batch_snapshot()),
        (CURRENT_FILES[3], success_examples(root)?),
        (CURRENT_FILES[4], error_examples()?),
        (CURRENT_FILES[5], policy_snapshot()),
    ])
}

fn profile_snapshot() -> Value {
    let profiles = ExposureProfile::ALL
        .into_iter()
        .map(|profile| {
            json!({
                "name": profile.name(),
                "tools": profile.tools().iter().map(|tool| tool.name()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema": CURRENT_SCHEMA,
        "profiles": profiles,
    })
}

fn batch_snapshot() -> Value {
    let tools = BATCH_TOOL_REGISTRY
        .iter()
        .map(|entry| {
            json!({
                "tool": entry.tool.name(),
                "contract_version": entry.contract_version,
                "required_profile": entry.required_profile.name(),
                "read_only": entry.read_only,
                "eligible": entry.eligible,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema": CURRENT_SCHEMA,
        "tools": tools,
    })
}

fn success_examples(root: &Path) -> Result<Value, CompatibilityError> {
    let source_root = root
        .parent()
        .ok_or_else(|| CompatibilityError::FixtureContract("fixture root has no parent".into()))?;
    let v1_path = source_root.join("1.0/tool-contracts.json");
    let v2_path = source_root.join("2.0/repo-list-contract.json");
    let v1: Value = parse_json(&v1_path, &read_bounded(&v1_path)?)?;
    let v2: Value = parse_json(&v2_path, &read_bounded(&v2_path)?)?;
    let mut tools = v1
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            CompatibilityError::FixtureContract(
                "1.0 tool-contracts.json must contain a tools array".into(),
            )
        })?;
    let repo_list = tools
        .iter()
        .position(|entry| entry.get("tool") == Some(&Value::String("repo.list".into())))
        .ok_or_else(|| {
            CompatibilityError::FixtureContract(
                "1.0 tool-contracts.json is missing repo.list".into(),
            )
        })?;
    tools[repo_list] = v2;
    validate_examples(&tools)?;
    Ok(json!({
        "schema": CURRENT_SCHEMA,
        "tools": tools,
    }))
}

fn validate_examples(tools: &[Value]) -> Result<(), CompatibilityError> {
    if tools.len() != McpTool::ALL.len() {
        return Err(CompatibilityError::FixtureContract(format!(
            "success examples contain {} tools, expected {}",
            tools.len(),
            McpTool::ALL.len()
        )));
    }
    let mut observed = BTreeSet::new();
    for tool in VerticalTool::ALL {
        let example = tools
            .iter()
            .find(|entry| entry.get("tool") == Some(&Value::String(tool.name().into())))
            .ok_or_else(|| {
                CompatibilityError::FixtureContract(format!(
                    "success examples are missing {}",
                    tool.name()
                ))
            })?;
        if !observed.insert(tool.name()) {
            return Err(CompatibilityError::FixtureContract(format!(
                "success examples duplicate {}",
                tool.name()
            )));
        }
        let input = example.get("input").ok_or_else(|| {
            CompatibilityError::FixtureContract(format!(
                "success example {} has no input",
                tool.name()
            ))
        })?;
        let output = example.get("output").ok_or_else(|| {
            CompatibilityError::FixtureContract(format!(
                "success example {} has no output",
                tool.name()
            ))
        })?;
        validate_instance(tool, "input", tool.input_schema_json(), input)?;
        validate_instance(tool, "success", tool.output_schema_json(), output)?;
    }
    Ok(())
}

fn error_examples() -> Result<Value, CompatibilityError> {
    let mut examples = Vec::with_capacity(VerticalTool::ALL.len());
    for tool in VerticalTool::ALL {
        let output = json!({
            "schema_version": "1.0",
            "error": {
                "code": "INVALID_ARGUMENT",
                "message": "request is invalid",
                "retryable": false,
                "details": {},
                "next_actions": [],
            },
        });
        validate_instance(tool, "error", tool.output_schema_json(), &output)?;
        examples.push(json!({
            "tool": tool.name(),
            "output": output,
        }));
    }
    Ok(json!({
        "schema": CURRENT_SCHEMA,
        "tools": examples,
    }))
}

fn validate_instance(
    tool: VerticalTool,
    kind: &'static str,
    schema_json: &str,
    instance: &Value,
) -> Result<(), CompatibilityError> {
    let schema: Value =
        serde_json::from_str(schema_json).map_err(|source| CompatibilityError::EmbeddedSchema {
            tool: tool.name(),
            source,
        })?;
    let validator =
        jsonschema::draft202012::new(&schema).map_err(|source| CompatibilityError::Schema {
            tool: tool.name(),
            message: source.to_string(),
        })?;
    if let Err(error) = validator.validate(instance) {
        return Err(CompatibilityError::InvalidExample {
            tool: tool.name(),
            kind,
            message: error.to_string(),
        });
    }
    Ok(())
}

fn policy_snapshot() -> Value {
    json!({
        "schema": CURRENT_SCHEMA,
        "unknown_fields": {
            "input": "reject",
            "output": "reject",
        },
        "tool_contract_versions": McpTool::ALL
            .into_iter()
            .map(|tool| json!({
                "tool": tool.name(),
                "version": tool.contract_version(),
            }))
            .collect::<Vec<_>>(),
        "tool_version_negotiation": {
            "implemented": true,
            "selector_location": "_meta",
            "selector_key": "rootlight/toolContractVersion",
            "omitted_selector": "current_contract",
            "unsupported_major_evidence": "all_tool_process_and_zero_execution_unit",
            "required_error": ErrorCode::ProtocolMismatch,
            "required_recovery": "select_supported_version",
            "supported_version_detail": "supported_version",
        },
        "output_projection": {
            "implemented": false,
            "older_closed_output_claimed": false,
        },
        "cursor": {
            "current_prefix": "c3.",
            "unsupported_legacy_prefixes": ["c1.", "c2."],
            "legacy_disposition": "unsupported_version",
            "public_error": ErrorCode::InvalidCursor,
            "recovery": "restart_enumeration",
        },
    })
}

fn validate_history(history: &ReleaseHistory) -> Result<(), CompatibilityError> {
    if history.schema != HISTORY_SCHEMA {
        return Err(CompatibilityError::FixtureContract(format!(
            "release history schema must be {HISTORY_SCHEMA}"
        )));
    }
    if history.machine_observation.status != "verified" {
        return Err(CompatibilityError::FixtureContract(
            "release history machine observation must be verified".into(),
        ));
    }
    if !valid_commit(&history.machine_observation.observed_revision) {
        return Err(CompatibilityError::FixtureContract(
            "machine observation revision must be a lowercase 40-digit Git object ID".into(),
        ));
    }
    if history.machine_observation.observed_release_count != history.releases.len() {
        return Err(CompatibilityError::FixtureContract(
            "machine-observed release count does not match release rows".into(),
        ));
    }
    if !history.releases.is_empty() && matches!(history.owner_review, OwnerReview::Pending) {
        return Err(CompatibilityError::FixtureContract(
            "published release rows require explicit owner approval".into(),
        ));
    }
    if let OwnerReview::Approved {
        approved_by,
        approved_on,
    } = &history.owner_review
        && (approved_by.trim().is_empty() || approved_on.trim().is_empty())
    {
        return Err(CompatibilityError::FixtureContract(
            "approved owner review requires non-empty approver and date evidence".into(),
        ));
    }
    let mut previous = None;
    let mut tags = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for release in &history.releases {
        let minor = parse_minor(&release.minor)?;
        if previous.is_some_and(|value| value >= minor) {
            return Err(CompatibilityError::FixtureContract(
                "release rows must be strictly ordered by minor version".into(),
            ));
        }
        previous = Some(minor);
        if !valid_tag(&release.release_tag) {
            return Err(CompatibilityError::FixtureContract(format!(
                "invalid release tag {}",
                release.release_tag
            )));
        }
        if !valid_commit(&release.release_commit) {
            return Err(CompatibilityError::FixtureContract(format!(
                "invalid release commit {}",
                release.release_commit
            )));
        }
        validate_relative_path(&release.fixture_directory)?;
        if !release.fixture_directory.starts_with("releases/") {
            return Err(CompatibilityError::FixtureContract(format!(
                "release fixture directory {} must be below releases/",
                release.fixture_directory
            )));
        }
        if !tags.insert(release.release_tag.as_str())
            || !directories.insert(release.fixture_directory.as_str())
        {
            return Err(CompatibilityError::FixtureContract(
                "release tags and fixture directories must be unique".into(),
            ));
        }
    }
    Ok(())
}

fn validate_release_refs(
    fixture_root: &Path,
    history: &ReleaseHistory,
) -> Result<(), CompatibilityError> {
    if history.releases.is_empty() {
        return Ok(());
    }
    let workspace = fixture_root
        .ancestors()
        .find(|candidate| candidate.join("Cargo.toml").is_file())
        .ok_or_else(|| {
            CompatibilityError::FixtureContract(
                "release verification requires a workspace containing Cargo.toml".into(),
            )
        })?;
    for release in &history.releases {
        let revision = format!("refs/tags/{}^{{commit}}", release.release_tag);
        let output = Command::new("git")
            .args(["-C"])
            .arg(workspace)
            .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
            .arg(&revision)
            .output()
            .map_err(|source| CompatibilityError::Git {
                tag: release.release_tag.clone(),
                source,
            })?;
        let observed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || observed != release.release_commit {
            return Err(CompatibilityError::ReleaseRef {
                tag: release.release_tag.clone(),
                expected: release.release_commit.clone(),
                observed,
            });
        }
    }
    Ok(())
}

fn validate_manifest(
    manifest: &ImmutableManifest,
    history_bytes: &[u8],
) -> Result<(), CompatibilityError> {
    if manifest.schema != MANIFEST_SCHEMA || manifest.algorithm != "sha256" {
        return Err(CompatibilityError::FixtureContract(
            "manifest schema or digest algorithm is unsupported".into(),
        ));
    }
    if manifest.release_history_sha256 != sha256(history_bytes) {
        return Err(CompatibilityError::FixtureContract(
            "manifest does not bind the exact release-history bytes".into(),
        ));
    }
    if manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err(CompatibilityError::FixtureContract(format!(
            "manifest must contain 1..={MAX_FILES} files"
        )));
    }
    let mut previous = None;
    let mut paths = BTreeSet::new();
    for entry in &manifest.files {
        validate_relative_path(&entry.path)?;
        if entry.path == MANIFEST_FILE {
            return Err(CompatibilityError::FixtureContract(
                "manifest cannot hash itself".into(),
            ));
        }
        if entry.bytes > MAX_FILE_BYTES {
            return Err(CompatibilityError::FixtureContract(format!(
                "{} exceeds the fixture byte limit",
                entry.path
            )));
        }
        if !valid_sha256(&entry.sha256) {
            return Err(CompatibilityError::FixtureContract(format!(
                "{} has an invalid SHA-256",
                entry.path
            )));
        }
        if previous.is_some_and(|path: &str| path >= entry.path.as_str()) {
            return Err(CompatibilityError::FixtureContract(
                "manifest paths must be unique and sorted".into(),
            ));
        }
        previous = Some(entry.path.as_str());
        paths.insert(entry.path.as_str());
    }
    if !paths.contains(HISTORY_FILE) {
        return Err(CompatibilityError::FixtureContract(
            "manifest must include release history".into(),
        ));
    }
    for current in CURRENT_FILES {
        if !paths.contains(current) {
            return Err(CompatibilityError::FixtureContract(format!(
                "manifest is missing {current}"
            )));
        }
    }
    Ok(())
}

fn validate_release_snapshots(
    root: &Path,
    history: &ReleaseHistory,
    manifest_paths: &BTreeSet<String>,
) -> Result<(), CompatibilityError> {
    let required = required_prior_minors(history.releases.len());
    for release in &history.releases {
        let prefix = format!("{}/", release.fixture_directory);
        let files = manifest_paths
            .iter()
            .filter(|path| path.starts_with(&prefix))
            .count();
        if files == 0 || !root.join(&release.fixture_directory).is_dir() {
            return Err(CompatibilityError::FixtureContract(format!(
                "release {} has no immutable fixture snapshot",
                release.minor
            )));
        }
    }
    for release in history.releases.iter().rev().take(required) {
        for name in [
            "catalog.json",
            "profiles.json",
            "batch.json",
            "success-examples.json",
            "error-examples.json",
            "policy.json",
        ] {
            let path = format!("{}/{name}", release.fixture_directory);
            if !manifest_paths.contains(&path) {
                return Err(CompatibilityError::FixtureContract(format!(
                    "required prior minor {} is missing {name}",
                    release.minor
                )));
            }
        }
    }
    Ok(())
}

const fn required_prior_minors(release_count: usize) -> usize {
    if release_count > 2 { 2 } else { release_count }
}

fn parse_minor(value: &str) -> Result<(u64, u64), CompatibilityError> {
    let mut parts = value.split('.');
    let major_text = parts.next().unwrap_or_default();
    let minor_text = parts.next().unwrap_or_default();
    let valid_number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    if parts.next().is_some() || !valid_number(major_text) || !valid_number(minor_text) {
        return Err(CompatibilityError::FixtureContract(format!(
            "invalid minor version {value}"
        )));
    }
    let major = major_text.parse().map_err(|_| {
        CompatibilityError::FixtureContract(format!("minor version {value} is too large"))
    })?;
    let minor = minor_text.parse().map_err(|_| {
        CompatibilityError::FixtureContract(format!("minor version {value} is too large"))
    })?;
    Ok((major, minor))
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>, CompatibilityError> {
    let mut files = Vec::new();
    collect_files_below(root, root, &mut files)?;
    files.sort();
    if files.len() > MAX_FILES {
        return Err(CompatibilityError::FixtureContract(format!(
            "fixture tree contains more than {MAX_FILES} files"
        )));
    }
    Ok(files)
}

fn collect_files_below(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), CompatibilityError> {
    let entries = fs::read_dir(directory).map_err(|source| CompatibilityError::Io {
        path: directory.to_owned(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CompatibilityError::Io {
            path: directory.to_owned(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CompatibilityError::Io {
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(CompatibilityError::FixtureContract(format!(
                "fixture tree cannot contain symlink {}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            collect_files_below(root, &entry.path(), files)?;
        } else if file_type.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| {
                    CompatibilityError::FixtureContract("fixture path escaped its root".into())
                })?
                .to_owned();
            if relative != Path::new(MANIFEST_FILE) {
                files.push(relative);
            }
        } else {
            return Err(CompatibilityError::FixtureContract(format!(
                "fixture tree contains unsupported entry {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, CompatibilityError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| CompatibilityError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(CompatibilityError::FixtureContract(format!(
            "{} must be a regular file",
            path.display()
        )));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(CompatibilityError::FixtureContract(format!(
            "{} exceeds the fixture byte limit",
            path.display()
        )));
    }
    fs::read(path).map_err(|source| CompatibilityError::Io {
        path: path.to_owned(),
        source,
    })
}

fn parse_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    bytes: &[u8],
) -> Result<T, CompatibilityError> {
    serde_json::from_slice(bytes).map_err(|source| CompatibilityError::Json {
        path: path.to_owned(),
        source,
    })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), CompatibilityError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(CompatibilityError::SerializeJson)?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|source| CompatibilityError::Io {
        path: path.to_owned(),
        source,
    })
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}

fn path_text(path: &Path) -> Result<String, CompatibilityError> {
    path.to_str()
        .map(|value| value.replace('\\', "/"))
        .ok_or_else(|| CompatibilityError::FixtureContract("fixture path is not UTF-8".into()))
}

fn validate_relative_path(path: &str) -> Result<(), CompatibilityError> {
    let candidate = Path::new(path);
    if path.is_empty()
        || path.contains('\\')
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(CompatibilityError::FixtureContract(format!(
            "invalid fixture path {path}"
        )));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_tag(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/'))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseHistory {
    schema: String,
    machine_observation: MachineObservation,
    owner_review: OwnerReview,
    releases: Vec<ReleaseRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineObservation {
    status: String,
    observed_revision: String,
    observed_release_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum OwnerReview {
    Pending,
    Approved {
        approved_by: String,
        approved_on: String,
    },
}

impl OwnerReview {
    const fn status_name(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved { .. } => "approved",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseRow {
    minor: String,
    release_tag: String,
    release_commit: String,
    fixture_directory: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImmutableManifest {
    schema: String,
    algorithm: String,
    release_history_sha256: String,
    files: Vec<ManifestEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    path: String,
    bytes: u64,
    sha256: String,
}

struct VerificationSummary {
    file_count: usize,
    tool_count: usize,
    required_prior_minors: usize,
    owner_review: &'static str,
}

/// Fail-closed errors produced by the compatibility gate.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CompatibilityError {
    #[error("{0} requires a path")]
    MissingArgument(&'static str),
    #[error("unexpected MCP compatibility argument: {0}")]
    UnexpectedArgument(String),
    #[error("I/O failed for {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize compatibility evidence")]
    SerializeJson(#[source] serde_json::Error),
    #[error("failed to inspect release tag {tag}")]
    Git {
        tag: String,
        #[source]
        source: std::io::Error,
    },
    #[error("release tag {tag} resolves to {observed:?}, expected commit {expected}")]
    ReleaseRef {
        tag: String,
        expected: String,
        observed: String,
    },
    #[error("invalid embedded schema for {tool}")]
    EmbeddedSchema {
        tool: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to compile the {tool} schema: {message}")]
    Schema { tool: &'static str, message: String },
    #[error("{kind} example for {tool} violates its current schema: {message}")]
    InvalidExample {
        tool: &'static str,
        kind: &'static str,
        message: String,
    },
    #[error("compatibility fixture contract failed: {0}")]
    FixtureContract(String),
    #[error("compatibility fixture set drifted; missing={missing:?}, unexpected={unexpected:?}")]
    FileSet {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error(
        "compatibility fixture {path} drifted: bytes {observed_bytes}/{expected_bytes}, sha256 {observed_sha256}/{expected_sha256}"
    )]
    Digest {
        path: String,
        expected_bytes: u64,
        observed_bytes: u64,
        expected_sha256: String,
        observed_sha256: String,
    },
    #[error("current MCP compatibility evidence drifted in {path}; refresh it explicitly")]
    CurrentDrift { path: String },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use rootlight_mcp_contract::pagination::{AuthenticatedCursor, CursorError};
    use serde_json::Value;
    use tempfile::TempDir;

    use super::{
        CURRENT_FILES, CompatibilityError, MANIFEST_FILE, OwnerReview, ReleaseHistory,
        policy_snapshot, refresh_current, required_prior_minors, validate_examples,
        validate_history, validate_relative_path, verify,
    };

    #[test]
    fn prior_minor_requirements_activate_from_release_rows() {
        assert_eq!(required_prior_minors(0), 0);
        assert_eq!(required_prior_minors(1), 1);
        assert_eq!(required_prior_minors(2), 2);
        assert_eq!(required_prior_minors(8), 2);
    }

    #[test]
    fn fixture_paths_reject_escape_and_platform_variants() {
        for invalid in ["", "../escape", "/absolute", r"current\catalog.json"] {
            assert!(validate_relative_path(invalid).is_err(), "{invalid}");
        }
        assert!(validate_relative_path(CURRENT_FILES[0]).is_ok());
        assert!(validate_relative_path(MANIFEST_FILE).is_ok());
    }

    #[test]
    fn zero_release_history_can_remain_pending_without_inventing_approval() {
        let history = ReleaseHistory {
            schema: super::HISTORY_SCHEMA.into(),
            machine_observation: super::MachineObservation {
                status: "verified".into(),
                observed_revision: "63527fb7a79886aa2e0440149d60bb622c3691d5".into(),
                observed_release_count: 0,
            },
            owner_review: OwnerReview::Pending,
            releases: Vec::new(),
        };
        assert!(validate_history(&history).is_ok());
    }

    #[test]
    fn published_rows_cannot_bypass_owner_review() {
        let source = r#"{
            "schema":"rootlight.mcp-release-history/1",
            "machine_observation":{
                "status":"verified",
                "observed_revision":"63527fb7a79886aa2e0440149d60bb622c3691d5",
                "observed_release_count":1
            },
            "owner_review":{"status":"pending"},
            "releases":[{
                "minor":"0.1",
                "release_tag":"v0.1.0",
                "release_commit":"63527fb7a79886aa2e0440149d60bb622c3691d5",
                "fixture_directory":"releases/v0.1.0"
            }]
        }"#;
        let history: ReleaseHistory = serde_json::from_str(source).expect("history parses");
        assert!(matches!(
            validate_history(&history),
            Err(CompatibilityError::FixtureContract(message))
                if message.contains("owner approval")
        ));
    }

    #[test]
    fn history_rejects_unknown_fields() {
        let source = r#"{
            "schema":"rootlight.mcp-release-history/1",
            "machine_observation":{
                "status":"verified",
                "observed_revision":"63527fb7a79886aa2e0440149d60bb622c3691d5",
                "observed_release_count":0,
                "unexpected":true
            },
            "owner_review":{"status":"pending"},
            "releases":[]
        }"#;
        assert!(serde_json::from_str::<ReleaseHistory>(source).is_err());
    }

    #[test]
    fn checked_in_fixture_tree_verifies_offline() {
        let fixture = prepared_fixture();
        let summary = verify(&fixture.root).expect("fresh fixture verifies");
        assert_eq!(summary.tool_count, 19);
        assert_eq!(summary.required_prior_minors, 0);
    }

    #[test]
    fn manifest_rejects_mutated_fixture_bytes() {
        let fixture = prepared_fixture();
        let path = fixture.root.join(CURRENT_FILES[5]);
        fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("policy fixture opens")
            .write_all(b" ")
            .expect("policy fixture mutates");
        assert!(matches!(
            verify(&fixture.root),
            Err(CompatibilityError::Digest { .. })
        ));
    }

    #[test]
    fn manifest_rejects_deleted_fixture() {
        let fixture = prepared_fixture();
        fs::remove_file(fixture.root.join(CURRENT_FILES[2])).expect("batch fixture deletes");
        assert!(matches!(
            verify(&fixture.root),
            Err(CompatibilityError::FileSet { missing, .. }) if !missing.is_empty()
        ));
    }

    #[test]
    fn manifest_rejects_untracked_fixture() {
        let fixture = prepared_fixture();
        fs::write(fixture.root.join("current/untracked.json"), b"{}\n")
            .expect("extra fixture writes");
        assert!(matches!(
            verify(&fixture.root),
            Err(CompatibilityError::FileSet { unexpected, .. }) if !unexpected.is_empty()
        ));
    }

    #[test]
    fn current_inventory_rejects_a_missing_tool() {
        let fixture = prepared_fixture();
        let bytes = fs::read(fixture.root.join(CURRENT_FILES[3])).expect("examples read");
        let mut document: Value = serde_json::from_slice(&bytes).expect("examples parse");
        let tools = document["tools"].as_array_mut().expect("tools array");
        tools.pop();
        assert!(matches!(
            validate_examples(tools),
            Err(CompatibilityError::FixtureContract(message))
                if message.contains("expected 19")
        ));
    }

    #[test]
    fn unsupported_legacy_cursor_policy_matches_the_parser() {
        for cursor in ["c1.AAAA", "c2.AAAA"] {
            assert_eq!(
                AuthenticatedCursor::from_wire(cursor),
                Err(CursorError::UnsupportedVersion)
            );
        }
        let policy = policy_snapshot();
        assert_eq!(policy["cursor"]["current_prefix"], "c3.");
        assert_eq!(
            policy["cursor"]["public_error"],
            Value::String("INVALID_CURSOR".into())
        );
    }

    #[test]
    fn unsupported_major_policy_requires_explicit_version_selection() {
        let policy = policy_snapshot();
        assert_eq!(policy["tool_version_negotiation"]["implemented"], true);
        assert_eq!(
            policy["tool_version_negotiation"]["selector_key"],
            "rootlight/toolContractVersion"
        );
        assert_eq!(
            policy["tool_version_negotiation"]["unsupported_major_evidence"],
            "all_tool_process_and_zero_execution_unit"
        );
        assert_eq!(
            policy["output_projection"]["older_closed_output_claimed"],
            false
        );
    }

    struct PreparedFixture {
        _directory: TempDir,
        root: PathBuf,
    }

    fn prepared_fixture() -> PreparedFixture {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let mcp_root = directory.path().join("mcp");
        let root = mcp_root.join("compatibility");
        fs::create_dir_all(mcp_root.join("1.0")).expect("1.0 directory creates");
        fs::create_dir_all(mcp_root.join("2.0")).expect("2.0 directory creates");
        fs::create_dir_all(&root).expect("compatibility directory creates");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has workspace parent")
            .join("tests/fixtures/mcp");
        copy(
            &source.join("1.0/tool-contracts.json"),
            &mcp_root.join("1.0/tool-contracts.json"),
        );
        copy(
            &source.join("2.0/repo-list-contract.json"),
            &mcp_root.join("2.0/repo-list-contract.json"),
        );
        copy(
            &source.join("compatibility/release-history-v1.json"),
            &root.join("release-history-v1.json"),
        );
        refresh_current(&root).expect("current fixture refreshes");
        PreparedFixture {
            _directory: directory,
            root,
        }
    }

    fn copy(source: &Path, destination: &Path) {
        fs::copy(source, destination).unwrap_or_else(|error| {
            panic!(
                "failed to copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        });
    }
}
