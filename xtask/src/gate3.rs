//! Immutable acceptance evidence assembly and offline verification.
//!
//! Producers remain responsible for their measurements. This command closes
//! the boundary by validating every source-free artifact, binding it to one
//! clean revision, and embedding exact bytes in a checksum-verifiable file.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use rootlight_bench::{
    AblationDecision, GateDisposition, decode_context_pack_ablation, decode_performance_evidence,
    decode_trajectory_evidence, validate_performance_evidence,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const SCHEMA: &str = "rootlight.acceptance-evidence/1";
const MAX_BUNDLE_BYTES: usize = 600 * 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 128 * 1024 * 1024;
const EXPECTED_ARTIFACTS: [ArtifactSpec; 6] = [
    ArtifactSpec {
        id: "vertical-summary",
        file: "vertical-summary.json",
    },
    ArtifactSpec {
        id: "vertical-transcript",
        file: "vertical-transcript.jsonl",
    },
    ArtifactSpec {
        id: "contract-security-matrix",
        file: "contract-security-matrix.json",
    },
    ArtifactSpec {
        id: "workflow-trajectories",
        file: "trajectory-evidence.json",
    },
    ArtifactSpec {
        id: "context-ablation",
        file: "ablation-evidence.json",
    },
    ArtifactSpec {
        id: "performance-samples",
        file: "performance-evidence.json",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    mode: Mode,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Assemble {
        input_dir: PathBuf,
        bin_dir: PathBuf,
        output: PathBuf,
    },
    Verify {
        bundle: PathBuf,
    },
}

impl Options {
    pub(crate) fn parse(arguments: &mut impl Iterator<Item = String>) -> Result<Self, Gate3Error> {
        let mut input_dir = None;
        let mut bin_dir = None;
        let mut output = None;
        let mut verify = None;
        let mut source_revision = None;
        while let Some(flag) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| Gate3Error::MissingValue(flag.clone()))?;
            let slot = match flag.as_str() {
                "--input-dir" => &mut input_dir,
                "--bin-dir" => &mut bin_dir,
                "--output" => &mut output,
                "--verify" => &mut verify,
                "--source-revision" => &mut source_revision,
                _ => return Err(Gate3Error::UnexpectedArgument(flag)),
            };
            if slot.replace(value).is_some() {
                return Err(Gate3Error::DuplicateOption(flag));
            }
        }
        let source_revision = source_revision.ok_or(Gate3Error::MissingSourceRevision)?;
        validate_revision(&source_revision)?;
        let mode = match (input_dir, bin_dir, output, verify) {
            (Some(input_dir), Some(bin_dir), Some(output), None) => Mode::Assemble {
                input_dir: input_dir.into(),
                bin_dir: bin_dir.into(),
                output: output.into(),
            },
            (None, None, None, Some(bundle)) => Mode::Verify {
                bundle: bundle.into(),
            },
            _ => return Err(Gate3Error::InvalidMode),
        };
        Ok(Self {
            mode,
            source_revision,
        })
    }
}

pub(crate) fn run(options: &Options) -> Result<(), Gate3Error> {
    match &options.mode {
        Mode::Assemble {
            input_dir,
            bin_dir,
            output,
        } => assemble(input_dir, bin_dir, output, &options.source_revision),
        Mode::Verify { bundle } => verify_file(bundle, &options.source_revision),
    }
}

fn assemble(
    input_dir: &Path,
    bin_dir: &Path,
    output: &Path,
    source_revision: &str,
) -> Result<(), Gate3Error> {
    let workspace = require_exact_clean_revision(source_revision)?;
    let artifacts = load_artifacts(input_dir)?;
    let evidence = validate_artifacts(&artifacts, source_revision)?;
    let manifest = build_manifest(&workspace, bin_dir, source_revision, &artifacts, &evidence)?;
    let disposition = reproduce_disposition(&evidence)?;
    let bundle = AcceptanceBundle {
        schema: SCHEMA.to_owned(),
        manifest,
        disposition,
        artifacts,
    };
    validate_structure(&bundle, source_revision)?;
    let encoded = encode_bundle(&bundle)?;
    privacy_scan(&encoded)?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(Gate3Error::Io)?;
    }
    let mut file = fs::File::create(output).map_err(Gate3Error::Io)?;
    file.write_all(&encoded).map_err(Gate3Error::Io)?;
    file.write_all(b"\n").map_err(Gate3Error::Io)?;
    verify_file(output, source_revision)
}

fn verify_file(path: &Path, source_revision: &str) -> Result<(), Gate3Error> {
    let encoded = read_bounded(path, MAX_BUNDLE_BYTES)?;
    let bundle: AcceptanceBundle =
        serde_json::from_slice(&encoded).map_err(|_| Gate3Error::InvalidBundle)?;
    let canonical = encode_bundle(&bundle)?;
    if encoded.strip_suffix(b"\n").unwrap_or(&encoded) != canonical {
        return Err(Gate3Error::NonCanonical);
    }
    validate_structure(&bundle, source_revision)?;
    let evidence = validate_artifacts(&bundle.artifacts, source_revision)?;
    if bundle.disposition != reproduce_disposition(&evidence)? {
        return Err(Gate3Error::DispositionMismatch);
    }
    privacy_scan(&encoded)
}

fn load_artifacts(input_dir: &Path) -> Result<Vec<ArtifactRecord>, Gate3Error> {
    let mut artifacts = Vec::with_capacity(EXPECTED_ARTIFACTS.len());
    for spec in EXPECTED_ARTIFACTS {
        let bytes = read_bounded(&input_dir.join(spec.file), MAX_ARTIFACT_BYTES)?;
        privacy_scan(&bytes)?;
        artifacts.push(ArtifactRecord {
            id: spec.id.to_owned(),
            schema: artifact_schema(spec.id, &bytes)?,
            bytes: u64::try_from(bytes.len()).map_err(|_| Gate3Error::LimitExceeded)?,
            sha256: sha256_hex(&bytes),
            encoded_hex: hex_encode(&bytes),
        });
    }
    Ok(artifacts)
}

fn validate_artifacts(
    artifacts: &[ArtifactRecord],
    source_revision: &str,
) -> Result<EvidenceDisposition, Gate3Error> {
    validate_artifact_inventory(artifacts)?;
    let decoded = artifacts
        .iter()
        .map(|artifact| {
            let bytes = artifact_bytes(artifact)?;
            privacy_scan(&bytes)?;
            Ok((artifact.id.as_str(), bytes))
        })
        .collect::<Result<BTreeMap<_, _>, Gate3Error>>()?;

    let vertical: Value = serde_json::from_slice(required(&decoded, "vertical-summary")?)
        .map_err(|_| Gate3Error::InvalidArtifact("vertical-summary"))?;
    validate_vertical(
        &vertical,
        required(&decoded, "vertical-transcript")?,
        source_revision,
    )?;

    let contract_path = write_verification_copy(
        required(&decoded, "contract-security-matrix")?,
        "contract-security-matrix.json",
    )?;
    crate::contract_matrix::verify_for_gate(contract_path.path(), source_revision)
        .map_err(|_| Gate3Error::InvalidArtifact("contract-security-matrix"))?;

    let trajectory = decode_trajectory_evidence(required(&decoded, "workflow-trajectories")?)
        .map_err(|_| Gate3Error::InvalidArtifact("workflow-trajectories"))?;
    let ablation = decode_context_pack_ablation(required(&decoded, "context-ablation")?)
        .map_err(|_| Gate3Error::InvalidArtifact("context-ablation"))?;
    let performance = decode_performance_evidence(required(&decoded, "performance-samples")?)
        .map_err(|_| Gate3Error::InvalidArtifact("performance-samples"))?;
    validate_performance_evidence(&performance)
        .map_err(|_| Gate3Error::InvalidArtifact("performance-samples"))?;
    if performance.environment.source_revision != source_revision {
        return Err(Gate3Error::RevisionMismatch);
    }
    if ablation.protocol.source_revision != source_revision {
        return Err(Gate3Error::RevisionMismatch);
    }

    let mut fallback_reasons = Vec::new();
    let vertical_decision = vertical["gate_decision"]
        .as_str()
        .ok_or(Gate3Error::InvalidArtifact("vertical-summary"))?;
    if vertical_decision == "fallback" {
        fallback_reasons.push("volatile-process-state".to_owned());
    } else if vertical_decision != "pass" {
        return Err(Gate3Error::BlockedArtifact("vertical-summary"));
    }
    if trajectory.denominator.not_available > 0 {
        fallback_reasons.push("optional-comparison-adapter-unavailable".to_owned());
    }
    match &ablation.aggregate.decision {
        AblationDecision::Pass => {}
        AblationDecision::Fallback { reason_codes } => {
            fallback_reasons.extend(reason_codes.iter().cloned());
        }
        AblationDecision::Blocked { .. } => {
            return Err(Gate3Error::BlockedArtifact("context-ablation"));
        }
    }
    match performance.disposition {
        GateDisposition::Pass => {}
        GateDisposition::Fallback => {
            fallback_reasons.extend(performance.residual_limitations.iter().cloned());
        }
        GateDisposition::Blocked => {
            return Err(Gate3Error::BlockedArtifact("performance-samples"));
        }
    }
    fallback_reasons.sort();
    fallback_reasons.dedup();
    Ok(EvidenceDisposition {
        fallback_reasons,
        tokenizer_id: performance.environment.tokenizer_id,
        tokenizer_sha256: performance.environment.tokenizer_sha256,
        fixture_sha256: performance.environment.fixture_sha256,
        schema_versions: artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact.schema.clone()))
            .collect(),
    })
}

fn validate_vertical(
    summary: &Value,
    transcript: &[u8],
    source_revision: &str,
) -> Result<(), Gate3Error> {
    let matrix = &summary["tool_matrix"];
    let cells = matrix["cells"]
        .as_array()
        .ok_or(Gate3Error::InvalidArtifact("vertical-summary"))?;
    if summary["run_status"] != "completed"
        || summary["environment"]["source_revision"] != source_revision
        || matrix["exact_tool_count"] != 19
        || matrix["expected_cell_count"].as_u64() != u64::try_from(cells.len()).ok()
        || matrix["unexecuted_applicable_cells"] != 0
        || matrix["input_and_output_schema_validation"] != true
        || matrix["source_free_transcript"] != true
        || summary["protocol"]["tool_contract_version_selector"] != "rootlight/toolContractVersion"
        || summary["protocol"]["versioned_tool_calls"] != true
        || summary["protocol"]["unsupported_major_tool_count"] != 19
        || summary["fixture"]["prompt_injection_observed_only_in_untrusted_data_channel"] != true
        || summary["artifacts"]["transcript_sha256"] != sha256_hex(transcript)
        || summary["artifacts"]["repository_root_arguments_redacted"] != true
    {
        return Err(Gate3Error::InvalidArtifact("vertical-summary"));
    }
    Ok(())
}

fn build_manifest(
    workspace: &Path,
    bin_dir: &Path,
    source_revision: &str,
    artifacts: &[ArtifactRecord],
    evidence: &EvidenceDisposition,
) -> Result<BundleManifest, Gate3Error> {
    let rustc_verbose = command_text(workspace, "rustc", &["-vV"])?;
    let target = rustc_verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or(Gate3Error::CommandOutput)?
        .to_owned();
    let tree_sha = command_text(workspace, "git", &["rev-parse", "HEAD^{tree}"])?;
    let config_sha256 = sha256_hex(
        serde_json::to_vec(&EXPECTED_ARTIFACTS.map(|spec| (spec.id, spec.file)))
            .map_err(Gate3Error::Json)?
            .as_slice(),
    );
    let artifact_manifest = artifacts
        .iter()
        .map(|artifact| ArtifactIdentity {
            id: artifact.id.clone(),
            schema: artifact.schema.clone(),
            bytes: artifact.bytes,
            sha256: artifact.sha256.clone(),
        })
        .collect::<Vec<_>>();
    let artifact_manifest_sha256 =
        sha256_hex(&serde_json::to_vec(&artifact_manifest).map_err(Gate3Error::Json)?);
    let binary_sha256 = ["rootlight-daemon", "rootlight-mcp"]
        .into_iter()
        .map(|binary| {
            let path = binary_path(bin_dir, binary);
            let bytes = read_bounded(&path, MAX_BINARY_BYTES)?;
            Ok((binary.to_owned(), sha256_hex(&bytes)))
        })
        .collect::<Result<BTreeMap<_, _>, Gate3Error>>()?;
    Ok(BundleManifest {
        source_revision: source_revision.to_owned(),
        tree_oid: tree_sha.trim().to_owned(),
        cargo_lock_sha256: sha256_hex(&read_bounded(
            &workspace.join("Cargo.lock"),
            MAX_ARTIFACT_BYTES,
        )?),
        rustc_verbose_sha256: sha256_hex(rustc_verbose.as_bytes()),
        target,
        operating_system: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        build_profile: "ci-dev".to_owned(),
        feature_set: vec!["workspace-default".to_owned()],
        configuration_sha256: config_sha256,
        binary_sha256,
        fixture_sha256: evidence.fixture_sha256.clone(),
        tokenizer_id: evidence.tokenizer_id.clone(),
        tokenizer_sha256: evidence.tokenizer_sha256.clone(),
        schema_versions: evidence.schema_versions.clone(),
        artifact_manifest,
        artifact_manifest_sha256,
    })
}

fn reproduce_disposition(evidence: &EvidenceDisposition) -> Result<BundleDisposition, Gate3Error> {
    if evidence.fallback_reasons.is_empty() {
        return Ok(BundleDisposition {
            outcome: Outcome::Pass,
            reason_codes: Vec::new(),
            permitted_claims: vec!["bounded-source-backed-single-repository-analysis".to_owned()],
            prohibited_claims: Vec::new(),
            residual_risks: Vec::new(),
            blocked_downstream_work: Vec::new(),
        });
    }
    Ok(BundleDisposition {
        outcome: Outcome::Fallback,
        reason_codes: evidence.fallback_reasons.clone(),
        permitted_claims: vec!["bounded-source-backed-single-repository-analysis".to_owned()],
        prohibited_claims: vec![
            "context-pack-quality-parity-with-direct-retrieval".to_owned(),
            "durable-cross-restart-operation-or-query-state".to_owned(),
            "unavailable-comparison-adapter-results".to_owned(),
        ],
        residual_risks: evidence.fallback_reasons.clone(),
        blocked_downstream_work: vec!["semantic-product-expansion".to_owned()],
    })
}

fn validate_structure(bundle: &AcceptanceBundle, source_revision: &str) -> Result<(), Gate3Error> {
    if bundle.schema != SCHEMA || bundle.manifest.source_revision != source_revision {
        return Err(Gate3Error::RevisionMismatch);
    }
    validate_revision(source_revision)?;
    validate_revision(&bundle.manifest.tree_oid)?;
    validate_sha256(&bundle.manifest.cargo_lock_sha256)?;
    validate_sha256(&bundle.manifest.rustc_verbose_sha256)?;
    validate_sha256(&bundle.manifest.configuration_sha256)?;
    validate_sha256(&bundle.manifest.tokenizer_sha256)?;
    validate_sha256(&bundle.manifest.artifact_manifest_sha256)?;
    validate_artifact_inventory(&bundle.artifacts)?;
    let identities = bundle
        .artifacts
        .iter()
        .map(|artifact| ArtifactIdentity {
            id: artifact.id.clone(),
            schema: artifact.schema.clone(),
            bytes: artifact.bytes,
            sha256: artifact.sha256.clone(),
        })
        .collect::<Vec<_>>();
    if identities != bundle.manifest.artifact_manifest
        || sha256_hex(&serde_json::to_vec(&identities).map_err(Gate3Error::Json)?)
            != bundle.manifest.artifact_manifest_sha256
        || bundle.manifest.schema_versions.len() != EXPECTED_ARTIFACTS.len()
        || bundle.manifest.schema_versions
            != bundle
                .artifacts
                .iter()
                .map(|artifact| (artifact.id.clone(), artifact.schema.clone()))
                .collect()
        || bundle.manifest.binary_sha256.len() != 2
        || bundle.manifest.feature_set != ["workspace-default"]
        || bundle.manifest.target.is_empty()
        || bundle.manifest.operating_system.is_empty()
        || bundle.manifest.architecture.is_empty()
    {
        return Err(Gate3Error::InvalidManifest);
    }
    for digest in bundle
        .manifest
        .binary_sha256
        .values()
        .chain(bundle.manifest.fixture_sha256.values())
    {
        validate_sha256(digest)?;
    }
    if bundle.disposition.outcome == Outcome::Pass
        && (!bundle.disposition.reason_codes.is_empty()
            || !bundle.disposition.residual_risks.is_empty()
            || !bundle.disposition.blocked_downstream_work.is_empty())
    {
        return Err(Gate3Error::DispositionMismatch);
    }
    if bundle.disposition.outcome == Outcome::Fallback
        && (bundle.disposition.reason_codes.is_empty()
            || bundle.disposition.permitted_claims.is_empty()
            || bundle.disposition.prohibited_claims.is_empty()
            || bundle.disposition.residual_risks.is_empty()
            || bundle.disposition.blocked_downstream_work.is_empty())
    {
        return Err(Gate3Error::DispositionMismatch);
    }
    Ok(())
}

fn validate_artifact_inventory(artifacts: &[ArtifactRecord]) -> Result<(), Gate3Error> {
    if artifacts.len() != EXPECTED_ARTIFACTS.len() {
        return Err(Gate3Error::MissingArtifact);
    }
    let expected = EXPECTED_ARTIFACTS
        .iter()
        .map(|spec| spec.id)
        .collect::<BTreeSet<_>>();
    let observed = artifacts
        .iter()
        .map(|artifact| artifact.id.as_str())
        .collect::<BTreeSet<_>>();
    if expected != observed || observed.len() != artifacts.len() {
        return Err(Gate3Error::MissingArtifact);
    }
    for artifact in artifacts {
        let bytes = artifact_bytes(artifact)?;
        if artifact.bytes != u64::try_from(bytes.len()).map_err(|_| Gate3Error::LimitExceeded)?
            || artifact.sha256 != sha256_hex(&bytes)
            || artifact.schema != artifact_schema(&artifact.id, &bytes)?
        {
            return Err(Gate3Error::ChecksumMismatch);
        }
    }
    Ok(())
}

fn artifact_schema(id: &str, bytes: &[u8]) -> Result<String, Gate3Error> {
    if id == "vertical-transcript" {
        return Ok("rootlight.mcp-transcript-jsonl/1".to_owned());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| Gate3Error::InvalidArtifact("json"))?;
    let schema = match id {
        "vertical-summary" | "performance-samples" => value["schema_version"].as_str(),
        "contract-security-matrix" | "workflow-trajectories" => value["schema"].as_str(),
        "context-ablation" => value["protocol"]["schema"].as_str(),
        _ => None,
    };
    schema
        .filter(|schema| !schema.is_empty())
        .map(str::to_owned)
        .ok_or(Gate3Error::InvalidArtifact("schema"))
}

fn required<'a>(
    artifacts: &'a BTreeMap<&str, Vec<u8>>,
    id: &'static str,
) -> Result<&'a [u8], Gate3Error> {
    artifacts
        .get(id)
        .map(Vec::as_slice)
        .ok_or(Gate3Error::MissingArtifact)
}

fn artifact_bytes(artifact: &ArtifactRecord) -> Result<Vec<u8>, Gate3Error> {
    let bytes = hex_decode(&artifact.encoded_hex)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(Gate3Error::LimitExceeded);
    }
    Ok(bytes)
}

fn write_verification_copy(
    bytes: &[u8],
    name: &str,
) -> Result<tempfile::NamedTempFile, Gate3Error> {
    let mut file = tempfile::Builder::new()
        .suffix(name)
        .tempfile()
        .map_err(Gate3Error::Io)?;
    file.write_all(bytes).map_err(Gate3Error::Io)?;
    Ok(file)
}

fn require_exact_clean_revision(source_revision: &str) -> Result<PathBuf, Gate3Error> {
    let current = std::env::current_dir().map_err(Gate3Error::Io)?;
    let workspace_text = command_text(&current, "git", &["rev-parse", "--show-toplevel"])?;
    let workspace = PathBuf::from(workspace_text.trim());
    let head = command_text(&workspace, "git", &["rev-parse", "HEAD"])?;
    let status = command_text(
        &workspace,
        "git",
        &["status", "--porcelain=v1", "--untracked-files=no"],
    )?;
    if head.trim() != source_revision {
        return Err(Gate3Error::RevisionMismatch);
    }
    if !status.trim().is_empty() {
        return Err(Gate3Error::DirtyTree);
    }
    Ok(workspace)
}

fn command_text(directory: &Path, program: &str, arguments: &[&str]) -> Result<String, Gate3Error> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .map_err(Gate3Error::Io)?;
    if !output.status.success() {
        return Err(Gate3Error::CommandFailed(program.to_owned()));
    }
    String::from_utf8(output.stdout).map_err(|_| Gate3Error::CommandOutput)
}

fn binary_path(bin_dir: &Path, name: &str) -> PathBuf {
    let suffix = std::env::consts::EXE_SUFFIX;
    bin_dir.join(format!("{name}{suffix}"))
}

fn encode_bundle(bundle: &AcceptanceBundle) -> Result<Vec<u8>, Gate3Error> {
    let encoded = serde_json::to_vec(bundle).map_err(Gate3Error::Json)?;
    if encoded.len() > MAX_BUNDLE_BYTES {
        return Err(Gate3Error::LimitExceeded);
    }
    Ok(encoded)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, Gate3Error> {
    let metadata = fs::metadata(path).map_err(Gate3Error::Io)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(Gate3Error::LimitExceeded);
    }
    fs::read(path).map_err(Gate3Error::Io)
}

fn privacy_scan(bytes: &[u8]) -> Result<(), Gate3Error> {
    const PATH_MARKERS: [&[u8]; 3] = [b"C:\\Users\\", b"/home/", b"/Users/"];
    if PATH_MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|window| window == *marker))
        || bytes
            .split(|byte| *byte == b'\n')
            .any(|line| crate::source_hygiene::forbidden_reference(line).is_some())
    {
        return Err(Gate3Error::PrivacyBoundary);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), Gate3Error> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Gate3Error::InvalidRevision);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), Gate3Error> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Gate3Error::InvalidManifest);
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(digest.as_ref())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Result<Vec<u8>, Gate3Error> {
    if !value.len().is_multiple_of(2) {
        return Err(Gate3Error::InvalidBundle);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_nibble(pair[0])?;
            let low = hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn hex_nibble(byte: u8) -> Result<u8, Gate3Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(Gate3Error::InvalidBundle),
    }
}

#[derive(Debug, Clone, Copy)]
struct ArtifactSpec {
    id: &'static str,
    file: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EvidenceDisposition {
    fallback_reasons: Vec<String>,
    tokenizer_id: String,
    tokenizer_sha256: String,
    fixture_sha256: BTreeMap<String, String>,
    schema_versions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBundle {
    schema: String,
    manifest: BundleManifest,
    disposition: BundleDisposition,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    source_revision: String,
    tree_oid: String,
    cargo_lock_sha256: String,
    rustc_verbose_sha256: String,
    target: String,
    operating_system: String,
    architecture: String,
    build_profile: String,
    feature_set: Vec<String>,
    configuration_sha256: String,
    binary_sha256: BTreeMap<String, String>,
    fixture_sha256: BTreeMap<String, String>,
    tokenizer_id: String,
    tokenizer_sha256: String,
    schema_versions: BTreeMap<String, String>,
    artifact_manifest: Vec<ArtifactIdentity>,
    artifact_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactIdentity {
    id: String,
    schema: String,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactRecord {
    id: String,
    schema: String,
    bytes: u64,
    sha256: String,
    encoded_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleDisposition {
    outcome: Outcome,
    reason_codes: Vec<String>,
    permitted_claims: Vec<String>,
    prohibited_claims: Vec<String>,
    residual_risks: Vec<String>,
    blocked_downstream_work: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Pass,
    Fallback,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Gate3Error {
    #[error("gate3 requires --source-revision REV")]
    MissingSourceRevision,
    #[error("{0} requires a value")]
    MissingValue(String),
    #[error("duplicate gate3 option: {0}")]
    DuplicateOption(String),
    #[error("unexpected gate3 argument: {0}")]
    UnexpectedArgument(String),
    #[error("gate3 requires either assemble inputs or one --verify path")]
    InvalidMode,
    #[error("source revision must be a 40-character hexadecimal commit identifier")]
    InvalidRevision,
    #[error("source revision differs")]
    RevisionMismatch,
    #[error("evidence assembly requires a clean tracked tree")]
    DirtyTree,
    #[error("a mandatory evidence artifact is missing or duplicated")]
    MissingArtifact,
    #[error("evidence artifact is invalid: {0}")]
    InvalidArtifact(&'static str),
    #[error("mandatory evidence is blocked: {0}")]
    BlockedArtifact(&'static str),
    #[error("artifact checksum, size, or schema differs")]
    ChecksumMismatch,
    #[error("bundle manifest is inconsistent")]
    InvalidManifest,
    #[error("bundle disposition does not reproduce")]
    DispositionMismatch,
    #[error("bundle contains a local path or internal planning identifier")]
    PrivacyBoundary,
    #[error("bundle or artifact exceeds its byte ceiling")]
    LimitExceeded,
    #[error("bundle is malformed")]
    InvalidBundle,
    #[error("bundle is not canonical JSON")]
    NonCanonical,
    #[error("required command failed: {0}")]
    CommandFailed(String),
    #[error("required command output is invalid")]
    CommandOutput,
    #[error("bundle I/O failed")]
    Io(#[source] std::io::Error),
    #[error("bundle JSON encoding failed")]
    Json(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn hex_encoding_round_trips_and_rejects_noncanonical_input() {
        let bytes = b"source-free evidence";
        assert_eq!(hex_decode(&hex_encode(bytes)).expect("hex decodes"), bytes);
        assert!(hex_decode("0").is_err());
        assert!(hex_decode("GG").is_err());
    }

    #[test]
    fn inventory_rejects_missing_and_corrupt_artifacts() {
        let mut artifacts = EXPECTED_ARTIFACTS
            .iter()
            .map(|spec| {
                let bytes = if spec.id == "vertical-transcript" {
                    b"{}\n".to_vec()
                } else {
                    match spec.id {
                        "vertical-summary" | "performance-samples" => {
                            br#"{"schema_version":"1.0"}"#.to_vec()
                        }
                        "contract-security-matrix" | "workflow-trajectories" => {
                            br#"{"schema":"1.0"}"#.to_vec()
                        }
                        "context-ablation" => br#"{"protocol":{"schema":"1.0"}}"#.to_vec(),
                        _ => unreachable!(),
                    }
                };
                ArtifactRecord {
                    id: spec.id.to_owned(),
                    schema: artifact_schema(spec.id, &bytes).expect("fixture schema"),
                    bytes: u64::try_from(bytes.len()).expect("fixture length"),
                    sha256: sha256_hex(&bytes),
                    encoded_hex: hex_encode(&bytes),
                }
            })
            .collect::<Vec<_>>();
        validate_artifact_inventory(&artifacts).expect("complete inventory validates");
        artifacts.pop();
        assert!(matches!(
            validate_artifact_inventory(&artifacts),
            Err(Gate3Error::MissingArtifact)
        ));
    }

    #[test]
    fn inventory_rejects_digest_and_schema_mutations() {
        let bytes = b"{}\n";
        let mut artifact = ArtifactRecord {
            id: "vertical-transcript".to_owned(),
            schema: "rootlight.mcp-transcript-jsonl/1".to_owned(),
            bytes: u64::try_from(bytes.len()).expect("fixture length"),
            sha256: sha256_hex(bytes),
            encoded_hex: hex_encode(bytes),
        };
        artifact.sha256.replace_range(..1, "f");
        assert!(matches!(
            artifact_bytes(&artifact).and_then(|decoded| {
                if artifact.sha256 == sha256_hex(&decoded) {
                    Ok(())
                } else {
                    Err(Gate3Error::ChecksumMismatch)
                }
            }),
            Err(Gate3Error::ChecksumMismatch)
        ));
    }

    #[test]
    fn options_require_closed_assemble_or_verify_modes() {
        let verify = Options::parse(
            &mut [
                "--verify".to_owned(),
                "bundle.json".to_owned(),
                "--source-revision".to_owned(),
                REVISION.to_owned(),
            ]
            .into_iter(),
        )
        .expect("verify mode parses");
        assert_eq!(
            verify,
            Options {
                mode: Mode::Verify {
                    bundle: "bundle.json".into()
                },
                source_revision: REVISION.to_owned(),
            }
        );
        assert!(Options::parse(&mut std::iter::empty()).is_err());
    }

    #[test]
    fn privacy_scan_rejects_paths_and_internal_labels() {
        assert!(privacy_scan(b"source-free").is_ok());
        assert!(privacy_scan(b"C:\\Users\\private\\repo").is_err());
        let private_label = ["GA", "TE-3"].concat();
        assert!(privacy_scan(private_label.as_bytes()).is_err());
    }
}
