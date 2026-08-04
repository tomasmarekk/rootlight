//! Source-free release evidence for cold indexing pinned real repositories.
//!
//! The checked-in corpus owns every threshold. Runners retain raw operation and
//! latency measurements, while this module independently recomputes the gate.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sha256_hex;

/// Schema of the checked-in real-repository corpus.
pub const COLD_INDEX_CORPUS_SCHEMA: &str = "rootlight.cold-index-corpus/1";
/// Schema of one real-repository measurement.
pub const COLD_INDEX_EVIDENCE_SCHEMA: &str = "rootlight.cold-index-evidence/3";
/// Exact repository count required by the release gate.
pub const COLD_INDEX_REPOSITORY_COUNT: usize = 8;
/// Maximum accepted corpus document size.
pub const MAX_COLD_INDEX_CORPUS_BYTES: usize = 256 * 1024;
/// Maximum accepted evidence document size.
pub const MAX_COLD_INDEX_EVIDENCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_COLD_INDEX_PROGRESS_SAMPLES: usize = 64;

const REQUIRED_WORKFLOWS: [&str; 5] = [
    "architecture.overview",
    "context.pack",
    "source.read",
    "symbol.explain",
    "symbol.relationships",
];

/// Pinned source repositories and preregistered release thresholds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexCorpus {
    /// Corpus schema identity.
    pub schema: String,
    /// Sorted, unique repository specifications.
    pub repositories: Vec<ColdIndexRepositorySpec>,
}

impl ColdIndexCorpus {
    /// Returns the specification for one stable corpus identifier.
    #[must_use]
    pub fn repository(&self, id: &str) -> Option<&ColdIndexRepositorySpec> {
        self.repositories
            .binary_search_by_key(&id, |repository| repository.id.as_str())
            .ok()
            .map(|index| &self.repositories[index])
    }
}

/// One immutable real-repository benchmark specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexRepositorySpec {
    /// Stable short identifier used by matrix jobs and evidence files.
    pub id: String,
    /// Fixed HTTPS Git URL.
    pub url: String,
    /// Exact forty-character source revision.
    pub revision: String,
    /// Exact tracked-file count at the pinned revision.
    pub tracked_files: u64,
    /// Exact symbol lookup issued after restart.
    pub lookup_query: String,
    /// Repository-relative path that must occur in the complete result set.
    pub expected_path: String,
    /// Primary language whose tier is release-gated.
    pub primary_language: String,
    /// Best acceptable tier letter.
    pub minimum_tier: String,
    /// Maximum structural-plus-semantic wall time.
    pub maximum_elapsed_ms: u64,
    /// Maximum observed operation peak resident bytes.
    pub maximum_peak_rss_bytes: u64,
    /// Maximum durable state bytes above the empty-state baseline.
    pub maximum_durable_bytes: u64,
    /// Maximum durable bytes per examined source byte.
    pub maximum_durable_bytes_per_source_byte: u64,
    /// Maximum durable bytes per examined file.
    pub maximum_durable_bytes_per_file: u64,
    /// Exact measured sample count per query or workflow.
    pub sample_count: usize,
    /// Maximum nearest-rank p95 for exact `code.locate`.
    pub locate_p95_ns: u64,
    /// Maximum nearest-rank p95 for every required agent workflow.
    pub workflow_p95_ns: BTreeMap<String, u64>,
}

/// Durable resource counters retained from one terminal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexResourceEvidence {
    /// Maximum process resident bytes observed by the daemon.
    pub peak_rss_bytes: u64,
    /// Cumulative durable write traffic.
    pub written_bytes: u64,
    /// Source files examined by this operation.
    pub files_examined: u64,
    /// Source bytes examined by this operation.
    pub bytes_examined: u64,
}

impl ColdIndexResourceEvidence {
    /// Returns durable writes per examined source byte in thousandths.
    #[must_use]
    pub fn write_amplification_milli(&self) -> Option<u64> {
        if self.bytes_examined == 0 {
            return None;
        }
        self.written_bytes
            .checked_mul(1_000)
            .map(|scaled| scaled.div_ceil(self.bytes_examined))
    }
}

/// One nonterminal durable progress observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexProgressEvidence {
    /// Journal revision that exposed this observation.
    pub revision: u64,
    /// Completed coarse preparation units.
    pub completed_units: u64,
    /// Fixed total coarse preparation units.
    pub total_units: u64,
    /// Durable resource counters visible at this revision.
    pub resources: ColdIndexResourceEvidence,
}

/// One typed terminal operation retained by the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexOperationEvidence {
    /// Stable public operation identity.
    pub operation_id: String,
    /// Typed terminal public state.
    pub state: String,
    /// Source-free terminal stage.
    pub stage: String,
    /// Final monotonic journal revision.
    pub revision: u64,
    /// Final completed coarse preparation units.
    pub completed_units: u64,
    /// Final total coarse preparation units.
    pub total_units: u64,
    /// Final durable resource observations.
    pub resources: ColdIndexResourceEvidence,
    /// Bounded nonterminal observations proving monotonic progress.
    pub progress_samples: Vec<ColdIndexProgressEvidence>,
}

/// Primary-language tier observed after semantic completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexTierEvidence {
    /// Canonical primary language.
    pub language: String,
    /// Observed tier letter.
    pub tier: String,
    /// Files indexed for this language.
    pub indexed_files: u64,
}

/// Recovery observations collected after a fresh daemon process starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexRestartEvidence {
    /// Exact candidate interruption and repository-identity recovery proof.
    pub interrupted: ColdIndexInterruptedRecoveryEvidence,
    /// Generation published before daemon shutdown.
    pub generation_before_restart: String,
    /// Active generation resolved after restart.
    pub generation_after_restart: String,
    /// Whether repository status returned ready.
    pub repository_ready: bool,
    /// Whether the structural operation remained terminal and queryable.
    pub structural_operation_recovered: bool,
    /// Whether the semantic operation remained terminal and queryable.
    pub semantic_operation_recovered: bool,
    /// Semantic operation revision reconstructed after restart.
    pub semantic_revision_after_restart: u64,
    /// Semantic operation resources reconstructed after restart.
    pub semantic_resources_after_restart: ColdIndexResourceEvidence,
    /// Projected SQLite journal path length measured in Windows UTF-16 units.
    pub projected_journal_utf16_units: u64,
}

/// Recovery evidence for a candidate killed during a durable index operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexInterruptedRecoveryEvidence {
    /// Operation identity retained across the forced restart.
    pub operation_id: String,
    /// Public repository identity allocated before the forced restart.
    pub repository_id: String,
    /// Durable public state observed before termination.
    pub state_before_restart: String,
    /// Journal revision observed before termination.
    pub revision_before_restart: u64,
    /// Durable resources observed before termination.
    pub resources_before_restart: ColdIndexResourceEvidence,
    /// Public state reconstructed by the restarted candidate.
    pub state_after_restart: String,
    /// Journal revision reconstructed after restart.
    pub revision_after_restart: u64,
    /// Durable resources reconstructed after restart.
    pub resources_after_restart: ColdIndexResourceEvidence,
    /// Whether resubmission reused the original repository identity.
    pub repository_id_reused: bool,
}

/// Exact symbol lookup and every retained latency sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexLocateEvidence {
    /// Exact registered lookup string.
    pub query: String,
    /// Repository-relative path observed in the bounded complete result set.
    pub matched_path: String,
    /// Stable symbol identity selected for workflow calls.
    pub symbol_id: String,
    /// Every measured end-to-end latency.
    pub latency_ns: Vec<u64>,
}

/// Complete source-free evidence from one isolated repository run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdIndexEvidence {
    /// Evidence schema identity.
    pub schema: String,
    /// Rootlight source revision that built the candidate.
    pub source_revision: String,
    /// Candidate release version.
    pub candidate_version: String,
    /// SHA-256 of the exact Windows candidate archive.
    pub candidate_archive_sha256: String,
    /// SHA-256 of the executed daemon binary.
    pub daemon_sha256: String,
    /// SHA-256 of the executed MCP binary.
    pub mcp_sha256: String,
    /// SHA-256 of the exact checked-in corpus bytes.
    pub corpus_sha256: String,
    /// Stable corpus repository identifier.
    pub corpus_repository_id: String,
    /// Exact checked-out repository revision.
    pub repository_revision: String,
    /// Exact clean checkout tracked-file count.
    pub tracked_files: u64,
    /// Public repository identity assigned by Rootlight.
    pub repository_id: String,
    /// Terminal structural operation.
    pub structural_operation: ColdIndexOperationEvidence,
    /// Separately owned terminal semantic refinement.
    pub semantic_operation: ColdIndexOperationEvidence,
    /// Structural durable-write amplification in thousandths.
    pub structural_write_amplification_milli: u64,
    /// Semantic durable-write amplification in thousandths.
    pub semantic_write_amplification_milli: u64,
    /// Total cold-index elapsed time.
    pub elapsed_ms: u64,
    /// Durable state bytes above the measured empty-state baseline.
    pub durable_state_bytes: u64,
    /// Primary-language coverage after semantic completion.
    pub primary_language_tier: ColdIndexTierEvidence,
    /// Restart and durable operation recovery observations.
    pub restart: ColdIndexRestartEvidence,
    /// Post-restart exact lookup measurements.
    pub locate: ColdIndexLocateEvidence,
    /// Every post-restart agent-workflow latency sample by tool.
    pub workflow_latency_ns: BTreeMap<String, Vec<u64>>,
    /// Must remain false: checked-out repository code is input data only.
    pub repository_content_executed: bool,
}

/// Fail-closed corpus or evidence validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ColdIndexEvidenceError {
    /// The corpus is absent, oversized, malformed, or violates its closed contract.
    #[error("cold-index corpus is invalid")]
    InvalidCorpus,
    /// Evidence is absent, oversized, malformed, noncanonical, or outside policy.
    #[error("cold-index evidence is invalid")]
    InvalidEvidence,
    /// The complete evidence set does not contain every corpus entry exactly once.
    #[error("cold-index evidence set is incomplete")]
    IncompleteEvidenceSet,
    /// A local evidence or corpus file could not be read.
    #[error("cold-index evidence IO failed")]
    Io,
}

/// Reads and validates a checked-in cold-index corpus.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError`] for IO, size, JSON, ordering, or policy
/// failures.
pub fn load_cold_index_corpus(path: &Path) -> Result<ColdIndexCorpus, ColdIndexEvidenceError> {
    let encoded = fs::read(path).map_err(|_| ColdIndexEvidenceError::Io)?;
    decode_cold_index_corpus(&encoded)
}

/// Decodes and validates exact corpus bytes.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError::InvalidCorpus`] for malformed or
/// non-preregistered input.
pub fn decode_cold_index_corpus(encoded: &[u8]) -> Result<ColdIndexCorpus, ColdIndexEvidenceError> {
    if encoded.is_empty() || encoded.len() > MAX_COLD_INDEX_CORPUS_BYTES {
        return Err(ColdIndexEvidenceError::InvalidCorpus);
    }
    let corpus: ColdIndexCorpus =
        serde_json::from_slice(encoded).map_err(|_| ColdIndexEvidenceError::InvalidCorpus)?;
    validate_corpus(&corpus)?;
    Ok(corpus)
}

/// Computes the lowercase SHA-256 identity of exact corpus bytes.
#[must_use]
pub fn cold_index_corpus_sha256(encoded: &[u8]) -> String {
    sha256_hex(encoded)
}

/// Encodes one evidence record in the canonical checked representation.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError::InvalidEvidence`] when serialization or
/// the evidence-size bound fails.
pub fn encode_cold_index_evidence(
    evidence: &ColdIndexEvidence,
) -> Result<Vec<u8>, ColdIndexEvidenceError> {
    let mut encoded =
        serde_json::to_vec_pretty(evidence).map_err(|_| ColdIndexEvidenceError::InvalidEvidence)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_COLD_INDEX_EVIDENCE_BYTES {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    Ok(encoded)
}

/// Decodes a canonical evidence record.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError::InvalidEvidence`] for malformed,
/// oversized, or noncanonical JSON.
pub fn decode_cold_index_evidence(
    encoded: &[u8],
) -> Result<ColdIndexEvidence, ColdIndexEvidenceError> {
    if encoded.is_empty() || encoded.len() > MAX_COLD_INDEX_EVIDENCE_BYTES {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let evidence: ColdIndexEvidence =
        serde_json::from_slice(encoded).map_err(|_| ColdIndexEvidenceError::InvalidEvidence)?;
    if encode_cold_index_evidence(&evidence)? != encoded {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    Ok(evidence)
}

/// Recomputes every release decision for one evidence record.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError::InvalidEvidence`] when any identity,
/// terminal-state, resource, tier, recovery, correctness, or latency gate
/// fails.
pub fn verify_cold_index_evidence(
    corpus: &ColdIndexCorpus,
    corpus_sha256: &str,
    evidence: &ColdIndexEvidence,
    source_revision: &str,
    candidate_archive_sha256: &str,
) -> Result<(), ColdIndexEvidenceError> {
    validate_corpus(corpus)?;
    let spec = corpus
        .repository(&evidence.corpus_repository_id)
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    if evidence.schema != COLD_INDEX_EVIDENCE_SCHEMA
        || !is_sha1(source_revision)
        || !is_sha256(candidate_archive_sha256)
        || evidence.source_revision != source_revision
        || evidence.candidate_archive_sha256 != candidate_archive_sha256
        || evidence.corpus_sha256 != corpus_sha256
        || !is_sha256(corpus_sha256)
        || !is_sha256(&evidence.daemon_sha256)
        || !is_sha256(&evidence.mcp_sha256)
        || evidence.candidate_version.is_empty()
        || evidence.candidate_version.len() > 128
        || evidence.repository_revision != spec.revision
        || evidence.tracked_files != spec.tracked_files
        || evidence.repository_id.is_empty()
        || evidence.repository_content_executed
        || evidence.elapsed_ms == 0
        || evidence.elapsed_ms > spec.maximum_elapsed_ms
        || evidence.durable_state_bytes == 0
        || evidence.durable_state_bytes > spec.maximum_durable_bytes
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    validate_terminal_operation(&evidence.structural_operation)?;
    validate_terminal_operation(&evidence.semantic_operation)?;
    let structural = evidence.structural_operation.resources;
    let semantic = evidence.semantic_operation.resources;
    let structural_write_amplification = structural
        .write_amplification_milli()
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    let semantic_write_amplification = semantic
        .write_amplification_milli()
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    let peak_rss_bytes = structural.peak_rss_bytes.max(semantic.peak_rss_bytes);
    if peak_rss_bytes > spec.maximum_peak_rss_bytes
        || structural.files_examined == 0
        || structural.bytes_examined == 0
        || structural.written_bytes == 0
        || semantic.files_examined == 0
        || semantic.bytes_examined == 0
        || semantic.written_bytes == 0
        || evidence.structural_write_amplification_milli != structural_write_amplification
        || evidence.semantic_write_amplification_milli != semantic_write_amplification
        || exceeds_ratio(
            evidence.durable_state_bytes,
            structural.bytes_examined,
            spec.maximum_durable_bytes_per_source_byte,
        )
        || exceeds_ratio(
            evidence.durable_state_bytes,
            structural.files_examined,
            spec.maximum_durable_bytes_per_file,
        )
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let tier = &evidence.primary_language_tier;
    if tier.language != spec.primary_language
        || tier.indexed_files == 0
        || tier_rank(&tier.tier).is_none()
        || tier_rank(&spec.minimum_tier).is_none()
        || tier_rank(&tier.tier) > tier_rank(&spec.minimum_tier)
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let restart = &evidence.restart;
    let interrupted = &restart.interrupted;
    if interrupted.operation_id.is_empty()
        || interrupted.repository_id != evidence.repository_id
        || !matches!(
            interrupted.state_before_restart.as_str(),
            "queued" | "running"
        )
        || interrupted.revision_before_restart == 0
        || interrupted.state_after_restart != "failed"
        || interrupted.revision_after_restart <= interrupted.revision_before_restart
        || interrupted.resources_after_restart.files_examined
            < interrupted.resources_before_restart.files_examined
        || interrupted.resources_after_restart.bytes_examined
            < interrupted.resources_before_restart.bytes_examined
        || interrupted.resources_after_restart.written_bytes
            < interrupted.resources_before_restart.written_bytes
        || interrupted.resources_after_restart.peak_rss_bytes
            < interrupted.resources_before_restart.peak_rss_bytes
        || !interrupted.repository_id_reused
        || restart.projected_journal_utf16_units <= 260
        || restart.generation_before_restart.is_empty()
        || restart.generation_before_restart != restart.generation_after_restart
        || !restart.repository_ready
        || !restart.structural_operation_recovered
        || !restart.semantic_operation_recovered
        || restart.semantic_revision_after_restart != evidence.semantic_operation.revision
        || restart.semantic_resources_after_restart != semantic
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    if evidence.locate.query != spec.lookup_query
        || evidence.locate.matched_path != spec.expected_path
        || evidence.locate.symbol_id.is_empty()
        || evidence.locate.latency_ns.len() != spec.sample_count
        || nearest_rank_p95(&evidence.locate.latency_ns)? > spec.locate_p95_ns
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let observed_workflows = evidence
        .workflow_latency_ns
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if observed_workflows != BTreeSet::from(REQUIRED_WORKFLOWS) {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    for (tool, ceiling) in &spec.workflow_p95_ns {
        let samples = evidence
            .workflow_latency_ns
            .get(tool)
            .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
        if samples.len() != spec.sample_count || nearest_rank_p95(samples)? > *ceiling {
            return Err(ColdIndexEvidenceError::InvalidEvidence);
        }
    }
    Ok(())
}

/// Requires exactly one valid evidence record for every corpus repository.
///
/// # Errors
///
/// Returns [`ColdIndexEvidenceError`] for duplicate, missing, foreign, or
/// individually invalid evidence.
pub fn verify_cold_index_evidence_set(
    corpus: &ColdIndexCorpus,
    corpus_sha256: &str,
    evidence: &[ColdIndexEvidence],
    source_revision: &str,
    candidate_archive_sha256: &str,
) -> Result<(), ColdIndexEvidenceError> {
    if evidence.len() != corpus.repositories.len() {
        return Err(ColdIndexEvidenceError::IncompleteEvidenceSet);
    }
    let mut observed = BTreeSet::new();
    for record in evidence {
        verify_cold_index_evidence(
            corpus,
            corpus_sha256,
            record,
            source_revision,
            candidate_archive_sha256,
        )?;
        if !observed.insert(record.corpus_repository_id.as_str()) {
            return Err(ColdIndexEvidenceError::IncompleteEvidenceSet);
        }
    }
    let expected = corpus
        .repositories
        .iter()
        .map(|repository| repository.id.as_str())
        .collect::<BTreeSet<_>>();
    if observed != expected {
        return Err(ColdIndexEvidenceError::IncompleteEvidenceSet);
    }
    Ok(())
}

fn validate_corpus(corpus: &ColdIndexCorpus) -> Result<(), ColdIndexEvidenceError> {
    if corpus.schema != COLD_INDEX_CORPUS_SCHEMA
        || corpus.repositories.len() != COLD_INDEX_REPOSITORY_COUNT
        || !corpus
            .repositories
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id)
    {
        return Err(ColdIndexEvidenceError::InvalidCorpus);
    }
    let required_workflows = BTreeSet::from(REQUIRED_WORKFLOWS);
    for repository in &corpus.repositories {
        let workflows = repository
            .workflow_p95_ns
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let path = Path::new(&repository.expected_path);
        if repository.id.is_empty()
            || repository.id.len() > 64
            || !repository
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !repository.url.starts_with("https://github.com/")
            || !repository.url.ends_with(".git")
            || !is_sha1(&repository.revision)
            || repository.tracked_files == 0
            || repository.lookup_query.is_empty()
            || repository.lookup_query.len() > 256
            || repository.expected_path.is_empty()
            || repository.expected_path.len() > 1_024
            || path.is_absolute()
            || repository.expected_path.contains('\\')
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || !matches!(
                repository.primary_language.as_str(),
                "c" | "python" | "rust" | "typescript"
            )
            || !matches!(repository.minimum_tier.as_str(), "A" | "B" | "C" | "D")
            || matches!(
                repository.primary_language.as_str(),
                "python" | "rust" | "typescript"
            ) && !matches!(repository.minimum_tier.as_str(), "A" | "B")
            || repository.maximum_elapsed_ms == 0
            || repository.maximum_peak_rss_bytes == 0
            || repository.maximum_durable_bytes == 0
            || repository.maximum_durable_bytes_per_source_byte == 0
            || repository.maximum_durable_bytes_per_file == 0
            || repository.sample_count == 0
            || repository.sample_count > 1_000
            || repository.locate_p95_ns == 0
            || workflows != required_workflows
            || repository
                .workflow_p95_ns
                .values()
                .any(|ceiling| *ceiling == 0)
        {
            return Err(ColdIndexEvidenceError::InvalidCorpus);
        }
    }
    Ok(())
}

fn validate_terminal_operation(
    operation: &ColdIndexOperationEvidence,
) -> Result<(), ColdIndexEvidenceError> {
    if operation.operation_id.is_empty()
        || operation.state != "published"
        || operation.stage != "complete"
        || operation.revision == 0
        || operation.completed_units == 0
        || operation.completed_units != operation.total_units
        || operation.resources.peak_rss_bytes == 0
        || operation.progress_samples.len() < 2
        || operation.progress_samples.len() > MAX_COLD_INDEX_PROGRESS_SAMPLES
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let mut previous: Option<ColdIndexProgressEvidence> = None;
    for sample in &operation.progress_samples {
        if sample.revision == 0
            || sample.revision >= operation.revision
            || sample.total_units != operation.total_units
            || sample.completed_units > sample.total_units
        {
            return Err(ColdIndexEvidenceError::InvalidEvidence);
        }
        if let Some(previous) = previous
            && (sample.revision <= previous.revision
                || sample.completed_units < previous.completed_units
                || !resources_are_monotonic(previous.resources, sample.resources))
        {
            return Err(ColdIndexEvidenceError::InvalidEvidence);
        }
        previous = Some(*sample);
    }
    let first = operation
        .progress_samples
        .first()
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    let last = operation
        .progress_samples
        .last()
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    if last.completed_units <= first.completed_units
        || last.completed_units == 0
        || last.completed_units > operation.completed_units
        || !resources_are_monotonic(last.resources, operation.resources)
    {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    Ok(())
}

const fn resources_are_monotonic(
    previous: ColdIndexResourceEvidence,
    next: ColdIndexResourceEvidence,
) -> bool {
    previous.peak_rss_bytes <= next.peak_rss_bytes
        && previous.written_bytes <= next.written_bytes
        && previous.files_examined <= next.files_examined
        && previous.bytes_examined <= next.bytes_examined
}

fn exceeds_ratio(value: u64, denominator: u64, maximum_ratio: u64) -> bool {
    denominator
        .checked_mul(maximum_ratio)
        .is_none_or(|maximum| value > maximum)
}

fn tier_rank(tier: &str) -> Option<u8> {
    match tier {
        "A" => Some(0),
        "B" => Some(1),
        "C" => Some(2),
        "D" => Some(3),
        _ => None,
    }
}

fn nearest_rank_p95(samples: &[u64]) -> Result<u64, ColdIndexEvidenceError> {
    if samples.is_empty() || samples.contains(&0) {
        return Err(ColdIndexEvidenceError::InvalidEvidence);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = sorted
        .len()
        .checked_mul(95)
        .and_then(|value| value.checked_add(99))
        .map(|value| value / 100)
        .and_then(|value| value.checked_sub(1))
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)?;
    sorted
        .get(rank)
        .copied()
        .ok_or(ColdIndexEvidenceError::InvalidEvidence)
}

fn is_sha1(value: &str) -> bool {
    value.len() == 40
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

#[cfg(test)]
mod tests {
    use super::*;

    const CORPUS: &[u8] = include_bytes!("../../../benchmarks/cold-index-repositories.json");

    #[test]
    fn twenty_samples_measure_p95_without_promoting_one_outlier() {
        let mut twenty = vec![1_u64; 19];
        twenty.push(u64::MAX);
        assert_eq!(
            nearest_rank_p95(&twenty).expect("twenty positive samples are valid"),
            1
        );

        let mut ten = vec![1_u64; 9];
        ten.push(u64::MAX);
        assert_eq!(
            nearest_rank_p95(&ten).expect("ten positive samples are valid"),
            u64::MAX
        );
    }

    fn evidence() -> (ColdIndexCorpus, String, ColdIndexEvidence) {
        let corpus = decode_cold_index_corpus(CORPUS).expect("checked-in corpus validates");
        let corpus_sha256 = cold_index_corpus_sha256(CORPUS);
        let spec = corpus.repository("ripgrep").expect("ripgrep is registered");
        let resources = ColdIndexResourceEvidence {
            peak_rss_bytes: 256 * 1024 * 1024,
            written_bytes: 128 * 1024 * 1024,
            files_examined: spec.tracked_files,
            bytes_examined: 16 * 1024 * 1024,
        };
        let operation = |id: &str| ColdIndexOperationEvidence {
            operation_id: id.to_owned(),
            state: "published".to_owned(),
            stage: "complete".to_owned(),
            revision: 8,
            completed_units: 6,
            total_units: 6,
            resources,
            progress_samples: vec![
                ColdIndexProgressEvidence {
                    revision: 3,
                    completed_units: 1,
                    total_units: 6,
                    resources: ColdIndexResourceEvidence {
                        peak_rss_bytes: 64 * 1024 * 1024,
                        written_bytes: 0,
                        files_examined: 1,
                        bytes_examined: 16,
                    },
                },
                ColdIndexProgressEvidence {
                    revision: 6,
                    completed_units: 5,
                    total_units: 6,
                    resources,
                },
            ],
        };
        let latency = vec![1_000_000; spec.sample_count];
        let workflow_latency_ns = REQUIRED_WORKFLOWS
            .into_iter()
            .map(|tool| (tool.to_owned(), latency.clone()))
            .collect();
        let evidence = ColdIndexEvidence {
            schema: COLD_INDEX_EVIDENCE_SCHEMA.to_owned(),
            source_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            candidate_version: "1.0.0".to_owned(),
            candidate_archive_sha256: "a".repeat(64),
            daemon_sha256: "b".repeat(64),
            mcp_sha256: "c".repeat(64),
            corpus_sha256: corpus_sha256.clone(),
            corpus_repository_id: spec.id.clone(),
            repository_revision: spec.revision.clone(),
            tracked_files: spec.tracked_files,
            repository_id: "repo1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            structural_operation: operation("op1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            semantic_operation: operation("op1_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            structural_write_amplification_milli: resources
                .write_amplification_milli()
                .expect("fixture has examined source bytes"),
            semantic_write_amplification_milli: resources
                .write_amplification_milli()
                .expect("fixture has examined source bytes"),
            elapsed_ms: 60_000,
            durable_state_bytes: 64 * 1024 * 1024,
            primary_language_tier: ColdIndexTierEvidence {
                language: spec.primary_language.clone(),
                tier: spec.minimum_tier.clone(),
                indexed_files: 100,
            },
            restart: ColdIndexRestartEvidence {
                interrupted: ColdIndexInterruptedRecoveryEvidence {
                    operation_id: "op1_cccccccccccccccccccccccccccccccc".to_owned(),
                    repository_id: "repo1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    state_before_restart: "running".to_owned(),
                    revision_before_restart: 2,
                    resources_before_restart: ColdIndexResourceEvidence {
                        peak_rss_bytes: 64 * 1024 * 1024,
                        written_bytes: 0,
                        files_examined: 1,
                        bytes_examined: 16,
                    },
                    state_after_restart: "failed".to_owned(),
                    revision_after_restart: 3,
                    resources_after_restart: ColdIndexResourceEvidence {
                        peak_rss_bytes: 64 * 1024 * 1024,
                        written_bytes: 0,
                        files_examined: 1,
                        bytes_examined: 16,
                    },
                    repository_id_reused: true,
                },
                generation_before_restart: "gen1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
                generation_after_restart: "gen1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                repository_ready: true,
                structural_operation_recovered: true,
                semantic_operation_recovered: true,
                semantic_revision_after_restart: 8,
                semantic_resources_after_restart: resources,
                projected_journal_utf16_units: 261,
            },
            locate: ColdIndexLocateEvidence {
                query: spec.lookup_query.clone(),
                matched_path: spec.expected_path.clone(),
                symbol_id: "sym1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                latency_ns: latency,
            },
            workflow_latency_ns,
            repository_content_executed: false,
        };
        (corpus, corpus_sha256, evidence)
    }

    #[test]
    fn checked_in_corpus_is_exact_sorted_and_requires_semantic_tiers() {
        let corpus = decode_cold_index_corpus(CORPUS).expect("checked-in corpus validates");
        assert_eq!(corpus.repositories.len(), COLD_INDEX_REPOSITORY_COUNT);
        for id in ["cpython", "django", "typescript", "vscode"] {
            assert!(
                matches!(
                    corpus
                        .repository(id)
                        .expect("semantic repository exists")
                        .minimum_tier
                        .as_str(),
                    "A" | "B"
                ),
                "{id}"
            );
        }
    }

    #[test]
    fn verifier_enforces_terminal_tier_resource_recovery_and_latency_gates() {
        let (corpus, hash, valid) = evidence();
        verify_cold_index_evidence(
            &corpus,
            &hash,
            &valid,
            &valid.source_revision,
            &valid.candidate_archive_sha256,
        )
        .expect("complete evidence verifies");

        let mut cases = Vec::new();
        let mut nonterminal = valid.clone();
        nonterminal.semantic_operation.state = "running".to_owned();
        cases.push(nonterminal);
        let mut low_tier = valid.clone();
        low_tier.primary_language_tier.tier = "D".to_owned();
        cases.push(low_tier);
        let mut missing_semantic_progress = valid.clone();
        missing_semantic_progress
            .semantic_operation
            .progress_samples
            .clear();
        cases.push(missing_semantic_progress);
        let mut stale_semantic_recovery = valid.clone();
        stale_semantic_recovery
            .restart
            .semantic_resources_after_restart
            .bytes_examined = 0;
        cases.push(stale_semantic_recovery);
        let mut excessive_storage = valid.clone();
        excessive_storage.durable_state_bytes = u64::MAX;
        cases.push(excessive_storage);
        let mut incorrect_write_amplification = valid.clone();
        incorrect_write_amplification.structural_write_amplification_milli += 1;
        cases.push(incorrect_write_amplification);
        let mut changed_generation = valid.clone();
        changed_generation.restart.generation_after_restart = "other".to_owned();
        cases.push(changed_generation);
        let mut lost_interruption = valid.clone();
        lost_interruption.restart.interrupted.state_after_restart = "running".to_owned();
        cases.push(lost_interruption);
        let mut short_path = valid.clone();
        short_path.restart.projected_journal_utf16_units = 260;
        cases.push(short_path);
        let mut slow = valid.clone();
        slow.locate.latency_ns[..2].fill(u64::MAX);
        cases.push(slow);

        for invalid in cases {
            assert_eq!(
                verify_cold_index_evidence(
                    &corpus,
                    &hash,
                    &invalid,
                    &valid.source_revision,
                    &valid.candidate_archive_sha256,
                ),
                Err(ColdIndexEvidenceError::InvalidEvidence)
            );
        }
    }

    #[test]
    fn canonical_document_and_complete_set_fail_closed() {
        let (corpus, hash, valid) = evidence();
        let encoded = encode_cold_index_evidence(&valid).expect("evidence encodes");
        assert_eq!(
            decode_cold_index_evidence(&encoded).expect("canonical evidence decodes"),
            valid
        );
        let mut noncanonical = encoded;
        noncanonical.extend_from_slice(b"\n");
        assert_eq!(
            decode_cold_index_evidence(&noncanonical),
            Err(ColdIndexEvidenceError::InvalidEvidence)
        );
        assert_eq!(
            verify_cold_index_evidence_set(
                &corpus,
                &hash,
                &[valid.clone(), valid],
                "0123456789abcdef0123456789abcdef01234567",
                &"a".repeat(64),
            ),
            Err(ColdIndexEvidenceError::IncompleteEvidenceSet)
        );
    }
}
