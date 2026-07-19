//! Canonical single-file CI evidence for the bounded parser fallback.
//!
//! The envelope intentionally excludes wall-clock and process-tree metrics.
//! Those measurements are not reproducible on shared CI hosts and remain
//! explicitly unavailable until the audited benchmark publication path exists.

use std::{collections::BTreeMap, fmt, io};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};
use sha2::{Digest as _, Sha256};

use crate::{
    Availability, BenchmarkCommand, BuildProvenance, DatasetManifest, EnvironmentEvidence,
    EvidenceValue, ParserBenchmarkEvidence, RawSample, SampleOutcome,
};

/// Schema written by the deterministic parser CI evidence executable.
pub const PARSER_CI_ENVELOPE_SCHEMA_VERSION: &str = "rootlight.parser-ci-evidence/1";
/// Hard ceiling for one encoded parser CI evidence envelope.
pub const PARSER_CI_MAX_ENVELOPE_BYTES: usize = 256 * 1024;

const BENCHMARK_ID: &str = "rootlight-parser-benchmark-v1";
const DATASET_ID: &str = "rootlight-parser-micro-v1";
const EXPECTED_DATASET_RECORDS: usize = 4;
const EXPECTED_SEED: u64 = 0x524f_4f54_4c49_4748;
const EXPECTED_WARMUP_ROUNDS: u32 = 1;
const EXPECTED_TRIAL_ROUNDS: u32 = 10;
const EXPECTED_TIMEOUT_MS: u64 = 2_000;
const EXPECTED_SAMPLE_RECORDS: usize =
    EXPECTED_DATASET_RECORDS * (EXPECTED_WARMUP_ROUNDS as usize + EXPECTED_TRIAL_ROUNDS as usize);
const RECORD_SET_COUNT: u64 = 2;
const MAX_RECORD_SET_BYTES: usize = 128 * 1024;
const MAX_ID_BYTES: usize = 128;
const MAX_BUILD_TOKEN_BYTES: usize = 256;
const MAX_SYNTAX_ITEMS_PER_SAMPLE: u64 = 250_000;
const EXPECTED_DATASET_RECORD_BYTES: u64 = 825;
const EXPECTED_DATASET_RECORD_SHA256: &str =
    "990f996b75507cd26bef61dbaeace8975bb7e5b81113de74a623448f926dc074";
const EXPECTED_SAMPLE_RECORD_BYTES: u64 = 7_200;
const EXPECTED_SAMPLE_RECORD_SHA256: &str =
    "eb98d07b2bd81e2918fe465424060768b49f72b31133d6434d25f29aa25127cc";
const EXPECTED_TOTAL_SOURCE_BYTES: u64 = 2_497;
const EXPECTED_TOTAL_PHYSICAL_LINES: u64 = 121;
const EXPECTED_TOTAL_SYNTAX_NODES: u64 = 1_265;
const EXPECTED_TOTAL_SYNTAX_FACTS: u64 = 473;

const EXPECTED_DATASET: [ExpectedDatasetRecord; EXPECTED_DATASET_RECORDS] = [
    ExpectedDatasetRecord {
        id: "java-basic",
        grammar_family: "java",
        language: "java",
        source_sha256: "c984dc80af1fc2333cde40dd09df8e380a7e7dde508700c487fbe739e8c06d4e",
        source_bytes: 84,
        physical_lines: 3,
    },
    ExpectedDatasetRecord {
        id: "javascript-basic",
        grammar_family: "javascript",
        language: "javascript",
        source_sha256: "5b63136552577a64d788dc3cd4552739d0d60f9e1adb63ec4dfb6932d56fc75d",
        source_bytes: 46,
        physical_lines: 3,
    },
    ExpectedDatasetRecord {
        id: "python-basic",
        grammar_family: "python",
        language: "python",
        source_sha256: "b414e3e8a1cc84d091471727544c353f60db59fc2bea10ee562ece77410915be",
        source_bytes: 49,
        physical_lines: 2,
    },
    ExpectedDatasetRecord {
        id: "rust-basic",
        grammar_family: "rust",
        language: "rust",
        source_sha256: "821d282d75c051d9a2a445ad8ef1551004aba5b3322d7a354c8a2abcd15af1e6",
        source_bytes: 48,
        physical_lines: 3,
    },
];

#[derive(Debug, Clone, Copy)]
struct ExpectedDatasetRecord {
    id: &'static str,
    grammar_family: &'static str,
    language: &'static str,
    source_sha256: &'static str,
    source_bytes: u64,
    physical_lines: u64,
}

/// One canonical, source-free parser CI evidence document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParserCiEvidenceEnvelope {
    schema_version: String,
    benchmark_id: String,
    evidence_status: ParserCiEvidenceStatus,
    source_revision: String,
    binary_sha256: String,
    compiler: String,
    target: String,
    build_profile: String,
    dataset_id: String,
    dataset_revision: String,
    seed: u64,
    warmup_rounds: u32,
    trial_rounds: u32,
    timeout_ms: u64,
    record_set_count: u64,
    dataset_record_count: u64,
    dataset_record_bytes: u64,
    dataset_record_sha256: String,
    sample_record_count: u64,
    sample_record_bytes: u64,
    sample_record_sha256: String,
    total_source_bytes: u64,
    total_physical_lines: u64,
    total_syntax_nodes: u64,
    total_syntax_facts: u64,
    #[serde(deserialize_with = "deserialize_dataset_records")]
    dataset_records: Vec<ParserCiDatasetRecord>,
    #[serde(deserialize_with = "deserialize_sample_records")]
    sample_records: Vec<ParserCiSampleRecord>,
}

impl ParserCiEvidenceEnvelope {
    /// Returns the fixed number of retained parser samples.
    #[must_use]
    pub const fn sample_record_count(&self) -> u64 {
        self.sample_record_count
    }

    /// Returns the digest of the canonical sample-record array.
    #[must_use]
    pub fn sample_record_sha256(&self) -> &str {
        &self.sample_record_sha256
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ParserCiEvidenceStatus {
    DeterministicFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParserCiDatasetRecord {
    id: String,
    grammar_family: String,
    language: String,
    source_sha256: String,
    source_bytes: u64,
    physical_lines: u64,
    generated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParserCiSampleRecord {
    ordinal: u64,
    phase: String,
    dataset_entry_id: String,
    grammar_family: String,
    source_bytes: u64,
    physical_lines: u64,
    syntax_nodes: u64,
    syntax_facts: u64,
}

/// Builds the deterministic projection of one successful bounded parser run.
///
/// The projection retains schedule, input identity, syntax counts, and output
/// counts. It rejects failed samples and records timing, process accounting,
/// and semantic quality only through the closed fallback status.
///
/// # Errors
///
/// Returns [`ParserCiEvidenceError`] when inputs are inconsistent, unbounded,
/// ineligible, or not the closed parser evidence contract.
pub fn build_parser_ci_evidence(
    source_revision: &str,
    environment: &EnvironmentEvidence,
    dataset: &DatasetManifest,
    provenance: &BuildProvenance,
    command: &BenchmarkCommand,
    evidence: &ParserBenchmarkEvidence,
) -> Result<ParserCiEvidenceEnvelope, ParserCiEvidenceError> {
    let binary_sha256 = require_observed_string(&environment.binary_sha256)?;
    let compiler = require_observed_string(&environment.compiler)?;
    require_build_inputs(
        source_revision,
        binary_sha256,
        compiler,
        environment,
        provenance,
        command,
    )?;
    require_evidence_status(evidence)?;

    let dataset_records = dataset_records(dataset)?;
    let sample_records = sample_records(&dataset_records, &evidence.raw_samples)?;
    let (dataset_record_bytes, dataset_record_sha256) = record_set_identity(&dataset_records)?;
    let (sample_record_bytes, sample_record_sha256) = record_set_identity(&sample_records)?;
    let totals = sample_totals(&sample_records)?;

    let envelope = ParserCiEvidenceEnvelope {
        schema_version: PARSER_CI_ENVELOPE_SCHEMA_VERSION.to_owned(),
        benchmark_id: BENCHMARK_ID.to_owned(),
        evidence_status: ParserCiEvidenceStatus::DeterministicFallback,
        source_revision: source_revision.to_owned(),
        binary_sha256: binary_sha256.to_owned(),
        compiler: compiler.to_owned(),
        target: provenance.target.clone(),
        build_profile: provenance.build_profile.clone(),
        dataset_id: dataset.dataset_id.clone(),
        dataset_revision: dataset.revision.clone(),
        seed: command.seed,
        warmup_rounds: command.warmup_rounds,
        trial_rounds: command.trial_rounds,
        timeout_ms: command.timeout_ms,
        record_set_count: RECORD_SET_COUNT,
        dataset_record_count: usize_to_u64(dataset_records.len())?,
        dataset_record_bytes,
        dataset_record_sha256,
        sample_record_count: usize_to_u64(sample_records.len())?,
        sample_record_bytes,
        sample_record_sha256,
        total_source_bytes: totals.source_bytes,
        total_physical_lines: totals.physical_lines,
        total_syntax_nodes: totals.syntax_nodes,
        total_syntax_facts: totals.syntax_facts,
        dataset_records,
        sample_records,
    };
    validate_envelope(&envelope)?;
    Ok(envelope)
}

/// Encodes an envelope as one canonical bounded JSON document.
///
/// # Errors
///
/// Returns [`ParserCiEvidenceError`] if the envelope is invalid or exceeds the
/// fixed single-file ceiling.
pub fn encode_parser_ci_evidence(
    envelope: &ParserCiEvidenceEnvelope,
) -> Result<Vec<u8>, ParserCiEvidenceError> {
    validate_envelope(envelope)?;
    bounded_json(envelope, PARSER_CI_MAX_ENVELOPE_BYTES)
}

/// Strictly decodes and verifies one canonical bounded JSON envelope.
///
/// Unknown fields, duplicate fields, noncanonical encoding, oversized input,
/// inconsistent counts, byte lengths, digests, schedules, or source identities
/// are rejected.
///
/// # Errors
///
/// Returns [`ParserCiEvidenceError`] for every malformed or inconsistent input.
pub fn decode_parser_ci_evidence(
    encoded: &[u8],
) -> Result<ParserCiEvidenceEnvelope, ParserCiEvidenceError> {
    if encoded.is_empty() || encoded.len() > PARSER_CI_MAX_ENVELOPE_BYTES {
        return Err(ParserCiEvidenceError::LimitExceeded {
            resource: "envelope_bytes",
        });
    }
    let mut deserializer = serde_json::Deserializer::from_slice(encoded);
    let envelope = ParserCiEvidenceEnvelope::deserialize(&mut deserializer)
        .map_err(|_| ParserCiEvidenceError::Decode)?;
    deserializer
        .end()
        .map_err(|_| ParserCiEvidenceError::Decode)?;
    validate_envelope(&envelope)?;
    let canonical = bounded_json(&envelope, PARSER_CI_MAX_ENVELOPE_BYTES)?;
    if canonical != encoded {
        return Err(ParserCiEvidenceError::Noncanonical);
    }
    Ok(envelope)
}

/// Verifies a canonical envelope without retaining it.
///
/// # Errors
///
/// Returns the same failures as [`decode_parser_ci_evidence`].
pub fn verify_parser_ci_evidence(encoded: &[u8]) -> Result<(), ParserCiEvidenceError> {
    decode_parser_ci_evidence(encoded).map(|_| ())
}

fn require_build_inputs(
    source_revision: &str,
    binary_sha256: &str,
    compiler: &str,
    environment: &EnvironmentEvidence,
    provenance: &BuildProvenance,
    command: &BenchmarkCommand,
) -> Result<(), ParserCiEvidenceError> {
    if !is_lower_hex_revision(source_revision)
        || environment.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || provenance.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || provenance.source_revision != source_revision
        || provenance.binary_revision != format!("sha256:{binary_sha256}")
        || environment.feature_profile != provenance.build_profile
        || !provenance.features.is_empty()
        || command.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || command.subcommand != "parser-evidence"
        || !command.arguments.is_empty()
        || command.seed != EXPECTED_SEED
        || command.warmup_rounds != EXPECTED_WARMUP_ROUNDS
        || command.trial_rounds != EXPECTED_TRIAL_ROUNDS
        || command.timeout_ms != EXPECTED_TIMEOUT_MS
        || !is_sha256(binary_sha256)
        || !is_compiler_identity(compiler)
        || !is_build_token(&provenance.target)
        || !is_build_token(&provenance.build_profile)
    {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    Ok(())
}

fn require_evidence_status(
    evidence: &ParserBenchmarkEvidence,
) -> Result<(), ParserCiEvidenceError> {
    let semantic_unavailable = |status: &Availability| {
        matches!(
            status,
            Availability::Unavailable { reason_code }
                if reason_code == "semantic_extraction_not_integrated"
        )
    };
    if evidence.raw_samples.len() != EXPECTED_SAMPLE_RECORDS
        || evidence.summary.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || evidence.summary.benchmark_id != BENCHMARK_ID
        || !semantic_unavailable(&evidence.summary.semantic_eligibility)
        || evidence.summary.failed_samples != 0
        || evidence.summary.timed_out_samples != 0
        || evidence.summary.cancelled_samples != 0
        || evidence.coverage.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || evidence.coverage.attempted_entries != EXPECTED_DATASET_RECORDS as u64
        || evidence.coverage.committed_entries != EXPECTED_DATASET_RECORDS as u64
        || !evidence.coverage.skipped.is_empty()
        || evidence.coverage.parser_status.len() != EXPECTED_DATASET_RECORDS
        || evidence
            .coverage
            .parser_status
            .values()
            .any(|status| status != "succeeded")
        || evidence.quality.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || !semantic_unavailable(&evidence.quality.semantic_eligibility)
    {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    Ok(())
}

fn dataset_records(
    dataset: &DatasetManifest,
) -> Result<Vec<ParserCiDatasetRecord>, ParserCiEvidenceError> {
    if dataset.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
        || dataset.dataset_id != DATASET_ID
        || dataset.entries.len() != EXPECTED_DATASET_RECORDS
        || dataset.scope_rule != "embedded_fixture_set_v1"
        || dataset.loc_counting_rule != "physical_lines_newline_terminated_v1"
    {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    let mut records = Vec::new();
    records
        .try_reserve_exact(EXPECTED_DATASET_RECORDS)
        .map_err(|_| ParserCiEvidenceError::AllocationFailed)?;
    for (entry, expected) in dataset.entries.iter().zip(EXPECTED_DATASET) {
        if entry.id != expected.id
            || entry.grammar_family != expected.grammar_family
            || entry.language != expected.language
            || entry.source_sha256 != expected.source_sha256
            || entry.source_bytes != expected.source_bytes
            || entry.physical_lines != expected.physical_lines
            || !is_sha256(&entry.source_sha256)
            || entry.generated
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
        records.push(ParserCiDatasetRecord {
            id: entry.id.clone(),
            grammar_family: entry.grammar_family.clone(),
            language: entry.language.clone(),
            source_sha256: entry.source_sha256.clone(),
            source_bytes: entry.source_bytes,
            physical_lines: entry.physical_lines,
            generated: entry.generated,
        });
    }
    if dataset.revision != dataset_revision(&records)? {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    Ok(records)
}

fn sample_records(
    dataset_records: &[ParserCiDatasetRecord],
    samples: &[RawSample],
) -> Result<Vec<ParserCiSampleRecord>, ParserCiEvidenceError> {
    if samples.len() != EXPECTED_SAMPLE_RECORDS {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    let by_id: BTreeMap<&str, &ParserCiDatasetRecord> = dataset_records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut warmups = BTreeMap::<&str, u32>::new();
    let mut trials = BTreeMap::<&str, u32>::new();
    let mut records = Vec::new();
    records
        .try_reserve_exact(EXPECTED_SAMPLE_RECORDS)
        .map_err(|_| ParserCiEvidenceError::AllocationFailed)?;
    for (ordinal, sample) in samples.iter().enumerate() {
        let dataset = by_id
            .get(sample.dataset_entry_id.as_str())
            .copied()
            .ok_or(ParserCiEvidenceError::InvalidEnvelope)?;
        let phase_counts = match sample.phase.as_str() {
            "warmup" => &mut warmups,
            "trial" => &mut trials,
            _ => return Err(ParserCiEvidenceError::InvalidEnvelope),
        };
        let count = phase_counts.entry(dataset.id.as_str()).or_default();
        *count = count
            .checked_add(1)
            .ok_or(ParserCiEvidenceError::InvalidEnvelope)?;
        if sample.schema_version != crate::RESULT_BUNDLE_SCHEMA_VERSION
            || sample.ordinal != usize_to_u64(ordinal)?
            || sample.grammar_family != dataset.grammar_family
            || sample.source_bytes != dataset.source_bytes
            || sample.physical_lines != dataset.physical_lines
            || sample.syntax_nodes == 0
            || sample.syntax_nodes > MAX_SYNTAX_ITEMS_PER_SAMPLE
            || sample.syntax_facts == 0
            || sample.syntax_facts > MAX_SYNTAX_ITEMS_PER_SAMPLE
            || !matches!(
                sample.semantic_facts,
                EvidenceValue::Unavailable { ref reason_code }
                    if reason_code == "semantic_extraction_not_integrated"
            )
            || !matches!(
                sample.process_tree_cpu_ns,
                EvidenceValue::Unavailable { ref reason_code }
                    if reason_code == "process_tree_cpu_sampler_unavailable"
            )
            || !matches!(
                sample.process_tree_peak_rss_bytes,
                EvidenceValue::Unavailable { ref reason_code }
                    if reason_code == "process_tree_rss_sampler_unavailable"
            )
            || sample.outcome != SampleOutcome::Succeeded
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
        records.push(ParserCiSampleRecord {
            ordinal: sample.ordinal,
            phase: sample.phase.clone(),
            dataset_entry_id: sample.dataset_entry_id.clone(),
            grammar_family: sample.grammar_family.clone(),
            source_bytes: sample.source_bytes,
            physical_lines: sample.physical_lines,
            syntax_nodes: sample.syntax_nodes,
            syntax_facts: sample.syntax_facts,
        });
    }
    for dataset in dataset_records {
        if warmups.get(dataset.id.as_str()) != Some(&EXPECTED_WARMUP_ROUNDS)
            || trials.get(dataset.id.as_str()) != Some(&EXPECTED_TRIAL_ROUNDS)
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
    }
    Ok(records)
}

fn validate_envelope(envelope: &ParserCiEvidenceEnvelope) -> Result<(), ParserCiEvidenceError> {
    if envelope.schema_version != PARSER_CI_ENVELOPE_SCHEMA_VERSION
        || envelope.benchmark_id != BENCHMARK_ID
        || envelope.evidence_status != ParserCiEvidenceStatus::DeterministicFallback
        || !is_lower_hex_revision(&envelope.source_revision)
        || !is_sha256(&envelope.binary_sha256)
        || !is_compiler_identity(&envelope.compiler)
        || !is_build_token(&envelope.target)
        || !is_build_token(&envelope.build_profile)
        || envelope.dataset_id != DATASET_ID
        || envelope.seed != EXPECTED_SEED
        || envelope.warmup_rounds != EXPECTED_WARMUP_ROUNDS
        || envelope.trial_rounds != EXPECTED_TRIAL_ROUNDS
        || envelope.timeout_ms != EXPECTED_TIMEOUT_MS
        || envelope.record_set_count != RECORD_SET_COUNT
        || envelope.dataset_records.len() != EXPECTED_DATASET_RECORDS
        || envelope.sample_records.len() != EXPECTED_SAMPLE_RECORDS
        || envelope.dataset_record_count != EXPECTED_DATASET_RECORDS as u64
        || envelope.sample_record_count != EXPECTED_SAMPLE_RECORDS as u64
    {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    validate_dataset_record_values(&envelope.dataset_records)?;
    if envelope.dataset_revision != dataset_revision(&envelope.dataset_records)? {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    validate_sample_record_values(&envelope.dataset_records, &envelope.sample_records)?;
    let (dataset_bytes, dataset_sha256) = record_set_identity(&envelope.dataset_records)?;
    let (sample_bytes, sample_sha256) = record_set_identity(&envelope.sample_records)?;
    let totals = sample_totals(&envelope.sample_records)?;
    if dataset_bytes != EXPECTED_DATASET_RECORD_BYTES
        || dataset_sha256 != EXPECTED_DATASET_RECORD_SHA256
        || sample_bytes != EXPECTED_SAMPLE_RECORD_BYTES
        || sample_sha256 != EXPECTED_SAMPLE_RECORD_SHA256
        || totals.source_bytes != EXPECTED_TOTAL_SOURCE_BYTES
        || totals.physical_lines != EXPECTED_TOTAL_PHYSICAL_LINES
        || totals.syntax_nodes != EXPECTED_TOTAL_SYNTAX_NODES
        || totals.syntax_facts != EXPECTED_TOTAL_SYNTAX_FACTS
        || envelope.dataset_record_bytes != dataset_bytes
        || envelope.dataset_record_sha256 != dataset_sha256
        || envelope.sample_record_bytes != sample_bytes
        || envelope.sample_record_sha256 != sample_sha256
        || envelope.total_source_bytes != totals.source_bytes
        || envelope.total_physical_lines != totals.physical_lines
        || envelope.total_syntax_nodes != totals.syntax_nodes
        || envelope.total_syntax_facts != totals.syntax_facts
    {
        return Err(ParserCiEvidenceError::DigestMismatch);
    }
    Ok(())
}

fn validate_dataset_record_values(
    records: &[ParserCiDatasetRecord],
) -> Result<(), ParserCiEvidenceError> {
    if records.len() != EXPECTED_DATASET_RECORDS {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    for (record, expected) in records.iter().zip(EXPECTED_DATASET) {
        if record.id != expected.id
            || record.grammar_family != expected.grammar_family
            || record.language != expected.language
            || record.source_sha256 != expected.source_sha256
            || record.source_bytes != expected.source_bytes
            || record.physical_lines != expected.physical_lines
            || !is_sha256(&record.source_sha256)
            || record.generated
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
    }
    Ok(())
}

fn validate_sample_record_values(
    datasets: &[ParserCiDatasetRecord],
    samples: &[ParserCiSampleRecord],
) -> Result<(), ParserCiEvidenceError> {
    let by_id: BTreeMap<&str, &ParserCiDatasetRecord> = datasets
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    let mut warmups = BTreeMap::<&str, u32>::new();
    let mut trials = BTreeMap::<&str, u32>::new();
    for (ordinal, sample) in samples.iter().enumerate() {
        let dataset = by_id
            .get(sample.dataset_entry_id.as_str())
            .copied()
            .ok_or(ParserCiEvidenceError::InvalidEnvelope)?;
        let phase_counts = match sample.phase.as_str() {
            "warmup" => &mut warmups,
            "trial" => &mut trials,
            _ => return Err(ParserCiEvidenceError::InvalidEnvelope),
        };
        let count = phase_counts.entry(dataset.id.as_str()).or_default();
        *count = count
            .checked_add(1)
            .ok_or(ParserCiEvidenceError::InvalidEnvelope)?;
        if sample.ordinal != usize_to_u64(ordinal)?
            || sample.grammar_family != dataset.grammar_family
            || sample.source_bytes != dataset.source_bytes
            || sample.physical_lines != dataset.physical_lines
            || sample.syntax_nodes == 0
            || sample.syntax_nodes > MAX_SYNTAX_ITEMS_PER_SAMPLE
            || sample.syntax_facts == 0
            || sample.syntax_facts > MAX_SYNTAX_ITEMS_PER_SAMPLE
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
    }
    for dataset in datasets {
        if warmups.get(dataset.id.as_str()) != Some(&EXPECTED_WARMUP_ROUNDS)
            || trials.get(dataset.id.as_str()) != Some(&EXPECTED_TRIAL_ROUNDS)
        {
            return Err(ParserCiEvidenceError::InvalidEnvelope);
        }
    }
    Ok(())
}

fn dataset_revision(records: &[ParserCiDatasetRecord]) -> Result<String, ParserCiEvidenceError> {
    let mut hasher = Sha256::new();
    for record in records {
        hash_length_prefixed(&mut hasher, record.id.as_bytes())?;
        hash_length_prefixed(&mut hasher, record.source_sha256.as_bytes())?;
    }
    Ok(format!("sha256:{}", hex_digest(hasher.finalize())))
}

fn record_set_identity<T: Serialize + ?Sized>(
    records: &T,
) -> Result<(u64, String), ParserCiEvidenceError> {
    let encoded = bounded_json(records, MAX_RECORD_SET_BYTES)?;
    Ok((usize_to_u64(encoded.len())?, sha256_hex(&encoded)))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SampleTotals {
    source_bytes: u64,
    physical_lines: u64,
    syntax_nodes: u64,
    syntax_facts: u64,
}

fn sample_totals(samples: &[ParserCiSampleRecord]) -> Result<SampleTotals, ParserCiEvidenceError> {
    samples.iter().try_fold(
        SampleTotals::default(),
        |totals, sample| -> Result<SampleTotals, ParserCiEvidenceError> {
            Ok(SampleTotals {
                source_bytes: totals
                    .source_bytes
                    .checked_add(sample.source_bytes)
                    .ok_or(ParserCiEvidenceError::InvalidEnvelope)?,
                physical_lines: totals
                    .physical_lines
                    .checked_add(sample.physical_lines)
                    .ok_or(ParserCiEvidenceError::InvalidEnvelope)?,
                syntax_nodes: totals
                    .syntax_nodes
                    .checked_add(sample.syntax_nodes)
                    .ok_or(ParserCiEvidenceError::InvalidEnvelope)?,
                syntax_facts: totals
                    .syntax_facts
                    .checked_add(sample.syntax_facts)
                    .ok_or(ParserCiEvidenceError::InvalidEnvelope)?,
            })
        },
    )
}

fn bounded_json<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, ParserCiEvidenceError> {
    let mut output = CiBuffer::new(limit);
    let result = serde_json::to_writer(&mut output, value);
    if output.allocation_failed {
        return Err(ParserCiEvidenceError::AllocationFailed);
    }
    if output.exceeded {
        return Err(ParserCiEvidenceError::LimitExceeded {
            resource: "encoded_bytes",
        });
    }
    result.map_err(|_| ParserCiEvidenceError::Encode)?;
    Ok(output.bytes)
}

#[derive(Debug)]
struct CiBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
    allocation_failed: bool,
}

impl CiBuffer {
    const fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
            allocation_failed: false,
        }
    }
}

impl io::Write for CiBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.limit)
        {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "CI evidence limit exceeded",
            ));
        }
        if self.bytes.try_reserve(bytes.len()).is_err() {
            self.allocation_failed = true;
            return Err(io::Error::other("CI evidence allocation failed"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn deserialize_dataset_records<'de, D>(
    deserializer: D,
) -> Result<Vec<ParserCiDatasetRecord>, D::Error>
where
    D: Deserializer<'de>,
{
    struct DatasetRecordsVisitor;

    impl<'de> Visitor<'de> for DatasetRecordsVisitor {
        type Value = Vec<ParserCiDatasetRecord>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("the fixed four-record parser dataset")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut records = Vec::new();
            records
                .try_reserve_exact(EXPECTED_DATASET_RECORDS)
                .map_err(|_| serde::de::Error::custom("dataset allocation failed"))?;
            while let Some(record) = sequence.next_element()? {
                if records.len() == EXPECTED_DATASET_RECORDS {
                    return Err(serde::de::Error::custom("too many dataset records"));
                }
                records.push(record);
            }
            if records.len() != EXPECTED_DATASET_RECORDS {
                return Err(serde::de::Error::custom("wrong dataset record count"));
            }
            Ok(records)
        }
    }

    deserializer.deserialize_seq(DatasetRecordsVisitor)
}

fn deserialize_sample_records<'de, D>(
    deserializer: D,
) -> Result<Vec<ParserCiSampleRecord>, D::Error>
where
    D: Deserializer<'de>,
{
    struct SampleRecordsVisitor;

    impl<'de> Visitor<'de> for SampleRecordsVisitor {
        type Value = Vec<ParserCiSampleRecord>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("the fixed parser sample schedule")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut records = Vec::new();
            records
                .try_reserve_exact(EXPECTED_SAMPLE_RECORDS)
                .map_err(|_| serde::de::Error::custom("sample allocation failed"))?;
            while let Some(record) = sequence.next_element()? {
                if records.len() == EXPECTED_SAMPLE_RECORDS {
                    return Err(serde::de::Error::custom("too many sample records"));
                }
                records.push(record);
            }
            if records.len() != EXPECTED_SAMPLE_RECORDS {
                return Err(serde::de::Error::custom("wrong sample record count"));
            }
            Ok(records)
        }
    }

    deserializer.deserialize_seq(SampleRecordsVisitor)
}

fn require_observed_string(
    evidence: &EvidenceValue<String>,
) -> Result<&str, ParserCiEvidenceError> {
    match evidence {
        EvidenceValue::Observed { value } => Ok(value),
        EvidenceValue::Target { .. } | EvidenceValue::Unavailable { .. } => {
            Err(ParserCiEvidenceError::InvalidEnvelope)
        }
    }
}

fn is_lower_hex_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(is_lower_hex)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(is_lower_hex)
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

fn is_compiler_identity(value: &str) -> bool {
    let Some(version) = value.strip_prefix("rustc-") else {
        return false;
    };
    let mut components = version.split('.');
    (0..3).all(|_| {
        components.next().is_some_and(|component| {
            !component.is_empty()
                && component.len() <= 3
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && (component.len() == 1 || !component.starts_with('0'))
        })
    }) && components.next().is_none()
}

fn is_build_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_BUILD_TOKEN_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ParserCiEvidenceError> {
    if bytes.len() > MAX_ID_BYTES {
        return Err(ParserCiEvidenceError::InvalidEnvelope);
    }
    hasher.update(usize_to_u64(bytes.len())?.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn usize_to_u64(value: usize) -> Result<u64, ParserCiEvidenceError> {
    u64::try_from(value).map_err(|_| ParserCiEvidenceError::InvalidEnvelope)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(Sha256::digest(bytes))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;

        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Strict CI evidence construction or verification failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParserCiEvidenceError {
    /// The closed evidence contract or one of its invariants was violated.
    #[error("parser CI evidence is invalid")]
    InvalidEnvelope,
    /// A fixed encoded-input or output ceiling was exceeded.
    #[error("parser CI evidence limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// A bounded allocation could not be reserved.
    #[error("parser CI evidence allocation failed")]
    AllocationFailed,
    /// Canonical serialization failed.
    #[error("parser CI evidence encoding failed")]
    Encode,
    /// Strict JSON decoding failed.
    #[error("parser CI evidence decoding failed")]
    Decode,
    /// Valid JSON did not use the one accepted canonical encoding.
    #[error("parser CI evidence encoding is not canonical")]
    Noncanonical,
    /// A recorded byte count, total, or SHA-256 did not match its records.
    #[error("parser CI evidence record identity is invalid")]
    DigestMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CoverageEvidence, DatasetEntry, MetricDistribution, QualityEvidence, ResultSummary,
    };

    const SAMPLE_DATASET_ORDER: [usize; EXPECTED_SAMPLE_RECORDS] = [
        2, 3, 0, 1, 0, 2, 3, 1, 3, 1, 0, 2, 3, 2, 0, 1, 2, 0, 1, 3, 3, 2, 0, 1, 0, 3, 2, 1, 2, 0,
        3, 1, 1, 2, 3, 0, 3, 2, 0, 1, 2, 3, 0, 1,
    ];

    fn fixture_inputs() -> (
        String,
        EnvironmentEvidence,
        DatasetManifest,
        BuildProvenance,
        BenchmarkCommand,
        ParserBenchmarkEvidence,
    ) {
        let source_revision = "0123456789abcdef0123456789abcdef01234567".to_owned();
        let binary_sha256 = "00".repeat(32);
        let dataset_records = EXPECTED_DATASET
            .iter()
            .map(|expected| ParserCiDatasetRecord {
                id: expected.id.to_owned(),
                grammar_family: expected.grammar_family.to_owned(),
                language: expected.language.to_owned(),
                source_sha256: expected.source_sha256.to_owned(),
                source_bytes: expected.source_bytes,
                physical_lines: expected.physical_lines,
                generated: false,
            })
            .collect::<Vec<_>>();
        let dataset = DatasetManifest {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            dataset_id: DATASET_ID.to_owned(),
            revision: dataset_revision(&dataset_records).expect("fixture revision is valid"),
            scope_rule: "embedded_fixture_set_v1".to_owned(),
            loc_counting_rule: "physical_lines_newline_terminated_v1".to_owned(),
            entries: EXPECTED_DATASET
                .iter()
                .map(|expected| DatasetEntry {
                    id: expected.id.to_owned(),
                    grammar_family: expected.grammar_family.to_owned(),
                    language: expected.language.to_owned(),
                    relative_path: format!("{}.fixture", expected.id),
                    source_sha256: expected.source_sha256.to_owned(),
                    source_bytes: expected.source_bytes,
                    physical_lines: expected.physical_lines,
                    generated: false,
                })
                .collect(),
        };
        let environment = EnvironmentEvidence {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            cpu_model: EvidenceValue::unavailable("not_collected"),
            cpu_topology: EvidenceValue::unavailable("not_collected"),
            ram_bytes: EvidenceValue::unavailable("not_collected"),
            operating_system: EvidenceValue::observed("linux".to_owned()),
            kernel: EvidenceValue::unavailable("not_collected"),
            filesystem: EvidenceValue::unavailable("not_collected"),
            storage_device: EvidenceValue::unavailable("not_collected"),
            power_mode: EvidenceValue::unavailable("not_collected"),
            container_limits: EvidenceValue::unavailable("not_collected"),
            compiler: EvidenceValue::observed("rustc-1.90.0".to_owned()),
            binary_sha256: EvidenceValue::observed(binary_sha256.clone()),
            feature_profile: "release".to_owned(),
            sqlite: EvidenceValue::unavailable("not_in_scope"),
            adapter_versions: EvidenceValue::unavailable("not_collected"),
            grammar_versions: EvidenceValue::unavailable("not_collected"),
            grammar_source_package_checksums: EvidenceValue::unavailable("not_collected"),
            grammar_hashes: EvidenceValue::unavailable("not_collected"),
            locale: EvidenceValue::unavailable("not_collected"),
            background_process_policy: EvidenceValue::unavailable("not_collected"),
            clock_source: EvidenceValue::observed("std_instant_monotonic".to_owned()),
            process_tree_accounting: Availability::Unavailable {
                reason_code: "platform_process_tree_sampler_not_integrated".to_owned(),
            },
        };
        let provenance = BuildProvenance {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            source_revision: source_revision.clone(),
            binary_revision: format!("sha256:{binary_sha256}"),
            build_profile: "release".to_owned(),
            features: Vec::new(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
        };
        let command = BenchmarkCommand {
            schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            subcommand: "parser-evidence".to_owned(),
            arguments: Vec::new(),
            seed: 0x524f_4f54_4c49_4748,
            warmup_rounds: EXPECTED_WARMUP_ROUNDS,
            trial_rounds: EXPECTED_TRIAL_ROUNDS,
            timeout_ms: 2_000,
        };
        let raw_samples = SAMPLE_DATASET_ORDER
            .iter()
            .copied()
            .enumerate()
            .map(|(ordinal, dataset_index)| {
                let dataset = EXPECTED_DATASET[dataset_index];
                let (syntax_nodes, syntax_facts) = match dataset_index {
                    0 => (37, 11),
                    1 => (22, 10),
                    2 => (29, 13),
                    3 => (27, 9),
                    _ => unreachable!("the fixed schedule uses four datasets"),
                };
                RawSample {
                    schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                    ordinal: u64::try_from(ordinal).expect("fixture ordinal fits"),
                    phase: if ordinal < EXPECTED_DATASET_RECORDS {
                        "warmup"
                    } else {
                        "trial"
                    }
                    .to_owned(),
                    dataset_entry_id: dataset.id.to_owned(),
                    grammar_family: dataset.grammar_family.to_owned(),
                    elapsed_ns: u64::try_from(ordinal + 1).expect("fixture elapsed value fits"),
                    source_bytes: dataset.source_bytes,
                    physical_lines: dataset.physical_lines,
                    syntax_nodes,
                    syntax_facts,
                    semantic_facts: EvidenceValue::unavailable(
                        "semantic_extraction_not_integrated",
                    ),
                    process_tree_cpu_ns: EvidenceValue::unavailable(
                        "process_tree_cpu_sampler_unavailable",
                    ),
                    process_tree_peak_rss_bytes: EvidenceValue::unavailable(
                        "process_tree_rss_sampler_unavailable",
                    ),
                    outcome: SampleOutcome::Succeeded,
                    is_outlier: false,
                }
            })
            .collect();
        let semantic_unavailable = Availability::Unavailable {
            reason_code: "semantic_extraction_not_integrated".to_owned(),
        };
        let evidence = ParserBenchmarkEvidence {
            raw_samples,
            summary: ResultSummary {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                benchmark_id: BENCHMARK_ID.to_owned(),
                semantic_eligibility: semantic_unavailable.clone(),
                families: BTreeMap::<String, MetricDistribution>::new(),
                failed_samples: 0,
                timed_out_samples: 0,
                cancelled_samples: 0,
                confidence_intervals: Availability::Unavailable {
                    reason_code: "not_computed".to_owned(),
                },
            },
            coverage: CoverageEvidence {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                attempted_entries: EXPECTED_DATASET_RECORDS as u64,
                committed_entries: EXPECTED_DATASET_RECORDS as u64,
                skipped: BTreeMap::new(),
                parser_status: EXPECTED_DATASET
                    .iter()
                    .map(|dataset| (dataset.id.to_owned(), "succeeded".to_owned()))
                    .collect(),
            },
            quality: QualityEvidence {
                schema_version: crate::RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                rubric_id: crate::SEMANTIC_QUALITY_RUBRIC_ID.to_owned(),
                semantic_eligibility: semantic_unavailable,
                precision_ppm: EvidenceValue::unavailable("not_measured"),
                recall_ppm: EvidenceValue::unavailable("not_measured"),
                expected_calibration_error_ppm: EvidenceValue::unavailable("not_measured"),
                unsupported_cases: BTreeMap::new(),
            },
        };
        (
            source_revision,
            environment,
            dataset,
            provenance,
            command,
            evidence,
        )
    }

    fn fixture_envelope() -> ParserCiEvidenceEnvelope {
        let (source_revision, environment, dataset, provenance, command, evidence) =
            fixture_inputs();
        build_parser_ci_evidence(
            &source_revision,
            &environment,
            &dataset,
            &provenance,
            &command,
            &evidence,
        )
        .expect("fixed CI evidence is valid")
    }

    #[test]
    fn fixed_envelope_is_canonical_deterministic_and_strictly_verified() {
        let envelope = fixture_envelope();

        let first = encode_parser_ci_evidence(&envelope).expect("fixture encodes");
        let second = encode_parser_ci_evidence(&envelope).expect("fixture re-encodes");
        let decoded = decode_parser_ci_evidence(&first).expect("fixture strictly decodes");

        assert_eq!(first, second);
        assert_eq!(decoded, envelope);
        assert_eq!(envelope.dataset_record_bytes, EXPECTED_DATASET_RECORD_BYTES);
        assert_eq!(
            envelope.dataset_record_sha256,
            EXPECTED_DATASET_RECORD_SHA256
        );
        assert_eq!(envelope.sample_record_bytes, EXPECTED_SAMPLE_RECORD_BYTES);
        assert_eq!(envelope.sample_record_sha256, EXPECTED_SAMPLE_RECORD_SHA256);
        assert!(
            !String::from_utf8(first)
                .expect("canonical JSON is UTF-8")
                .contains("fn add")
        );
    }

    #[test]
    fn strict_decoder_rejects_whitespace_unknown_fields_and_digest_tampering() {
        let encoded = encode_parser_ci_evidence(&fixture_envelope()).expect("fixture encodes");

        let mut whitespace = encoded.clone();
        whitespace.push(b'\n');
        assert_eq!(
            decode_parser_ci_evidence(&whitespace),
            Err(ParserCiEvidenceError::Noncanonical)
        );

        let mut unknown = b"{\"unknown\":0,".to_vec();
        unknown.extend_from_slice(&encoded[1..]);
        assert_eq!(
            decode_parser_ci_evidence(&unknown),
            Err(ParserCiEvidenceError::Decode)
        );

        let mut tampered = String::from_utf8(encoded).expect("canonical JSON is UTF-8");
        tampered = tampered.replacen("\"syntax_facts\":13", "\"syntax_facts\":12", 1);
        assert_eq!(
            decode_parser_ci_evidence(tampered.as_bytes()),
            Err(ParserCiEvidenceError::DigestMismatch)
        );
    }

    #[test]
    fn builder_rejects_failed_or_nonfixed_benchmark_contracts() {
        let (source_revision, environment, dataset, provenance, command, mut evidence) =
            fixture_inputs();
        evidence.raw_samples[0].outcome = SampleOutcome::TimedOut;

        let result = build_parser_ci_evidence(
            &source_revision,
            &environment,
            &dataset,
            &provenance,
            &command,
            &evidence,
        );

        assert_eq!(result, Err(ParserCiEvidenceError::InvalidEnvelope));

        let (source_revision, environment, dataset, provenance, mut command, evidence) =
            fixture_inputs();
        command.seed = EXPECTED_SEED + 1;
        assert_eq!(
            build_parser_ci_evidence(
                &source_revision,
                &environment,
                &dataset,
                &provenance,
                &command,
                &evidence,
            ),
            Err(ParserCiEvidenceError::InvalidEnvelope)
        );

        command.seed = EXPECTED_SEED;
        command.timeout_ms = EXPECTED_TIMEOUT_MS + 1;
        assert_eq!(
            build_parser_ci_evidence(
                &source_revision,
                &environment,
                &dataset,
                &provenance,
                &command,
                &evidence,
            ),
            Err(ParserCiEvidenceError::InvalidEnvelope)
        );
    }

    #[test]
    fn bounded_record_decoders_reject_extra_entries_before_retention() {
        let dataset = r#"[{"id":"java-basic","grammar_family":"java","language":"java","source_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","source_bytes":1,"physical_lines":1,"generated":false},{"id":"javascript-basic","grammar_family":"javascript","language":"javascript","source_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","source_bytes":1,"physical_lines":1,"generated":false},{"id":"python-basic","grammar_family":"python","language":"python","source_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","source_bytes":1,"physical_lines":1,"generated":false},{"id":"rust-basic","grammar_family":"rust","language":"rust","source_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","source_bytes":1,"physical_lines":1,"generated":false},{"id":"extra","grammar_family":"rust","language":"rust","source_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","source_bytes":1,"physical_lines":1,"generated":false}]"#;
        let mut deserializer = serde_json::Deserializer::from_str(dataset);

        let result = deserialize_dataset_records(&mut deserializer);

        assert!(result.is_err());
    }

    #[test]
    fn compiler_identity_is_closed_and_source_free() {
        assert!(is_compiler_identity("rustc-1.90.0"));
        assert!(!is_compiler_identity("rustc 1.90.0 (C:\\private)"));
        assert!(!is_compiler_identity("rustc-01.90.0"));
        assert!(!is_compiler_identity("clang-19.0.0"));
    }
}
