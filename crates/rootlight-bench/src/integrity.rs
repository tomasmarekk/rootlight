//! Strict fixed-artifact decoding and cross-artifact result validation.
//!
//! The same bounded wire checks protect publication and later verification so
//! checksums cannot legitimize internally contradictory benchmark evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};

use crate::bundle::{
    AGENT_TRAJECTORIES_FILE, BUILD_PROVENANCE_FILE, BundleError, COMMAND_FILE, COVERAGE_FILE,
    DATASET_MANIFEST_FILE, ENVIRONMENT_FILE, QUALITY_FILE, RAW_SAMPLES_FILE, SUMMARY_FILE,
};
use crate::parser::{semantic_fact_eligibility, semantic_quality_eligibility};
use crate::{
    AgentTrajectory, Availability, BenchmarkCommand, BuildProvenance, BundleLimits,
    CoverageEvidence, DatasetManifest, EnvironmentEvidence, EvidenceValue, MetricDistribution,
    QualityEvidence, RESULT_BUNDLE_SCHEMA_VERSION, RawSample, ResultSummary,
    SEMANTIC_QUALITY_RUBRIC_ID, SampleOutcome, SemanticQualityMeasurement,
    decode_benchmark_command, decode_dataset_manifest,
};

const FIXED_ARTIFACTS: [&str; 9] = [
    ENVIRONMENT_FILE,
    DATASET_MANIFEST_FILE,
    BUILD_PROVENANCE_FILE,
    COMMAND_FILE,
    RAW_SAMPLES_FILE,
    SUMMARY_FILE,
    COVERAGE_FILE,
    QUALITY_FILE,
    AGENT_TRAJECTORIES_FILE,
];

#[derive(Debug)]
struct FixedArtifacts {
    environment: EnvironmentEvidence,
    dataset_manifest: DatasetManifest,
    build_provenance: BuildProvenance,
    command: BenchmarkCommand,
    raw_samples: Vec<RawSample>,
    summary: ResultSummary,
    coverage: CoverageEvidence,
    quality: QualityEvidence,
    agent_trajectories: Vec<AgentTrajectory>,
}

pub(crate) fn is_fixed_artifact(relative: &str) -> bool {
    FIXED_ARTIFACTS.contains(&relative)
}

pub(crate) fn validate_fixed_artifacts(
    artifacts: &BTreeMap<String, Vec<u8>>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    let fixed = decode_fixed_artifacts(artifacts, limits)?;
    validate_fixed_bundle(&fixed, limits)
}

fn decode_fixed_artifacts(
    artifacts: &BTreeMap<String, Vec<u8>>,
    limits: BundleLimits,
) -> Result<FixedArtifacts, BundleError> {
    for name in FIXED_ARTIFACTS {
        if !artifacts.contains_key(name) {
            return Err(BundleError::ArtifactSetMismatch);
        }
    }
    let manifest_bytes = fixed_bytes(artifacts, DATASET_MANIFEST_FILE)?;
    validate_json_bytes(manifest_bytes, limits)?;
    let dataset_manifest =
        decode_dataset_manifest(manifest_bytes, limits).map_err(map_decode_error)?;
    let command_bytes = fixed_bytes(artifacts, COMMAND_FILE)?;
    validate_json_bytes(command_bytes, limits)?;
    let command = decode_benchmark_command(command_bytes, limits).map_err(map_decode_error)?;
    Ok(FixedArtifacts {
        environment: decode_json(fixed_bytes(artifacts, ENVIRONMENT_FILE)?, limits)?,
        dataset_manifest,
        build_provenance: decode_json(fixed_bytes(artifacts, BUILD_PROVENANCE_FILE)?, limits)?,
        command,
        raw_samples: decode_json_lines(
            fixed_bytes(artifacts, RAW_SAMPLES_FILE)?,
            limits.max_raw_samples,
            limits,
            "raw_sample_count",
        )?,
        summary: decode_json(fixed_bytes(artifacts, SUMMARY_FILE)?, limits)?,
        coverage: decode_json(fixed_bytes(artifacts, COVERAGE_FILE)?, limits)?,
        quality: decode_json(fixed_bytes(artifacts, QUALITY_FILE)?, limits)?,
        agent_trajectories: decode_json_lines(
            fixed_bytes(artifacts, AGENT_TRAJECTORIES_FILE)?,
            limits.max_agent_trajectories,
            limits,
            "agent_trajectory_count",
        )?,
    })
}

fn fixed_bytes<'a>(
    artifacts: &'a BTreeMap<String, Vec<u8>>,
    name: &str,
) -> Result<&'a [u8], BundleError> {
    artifacts
        .get(name)
        .map(Vec::as_slice)
        .ok_or(BundleError::ArtifactSetMismatch)
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8], limits: BundleLimits) -> Result<T, BundleError> {
    validate_json_bytes(bytes, limits)?;
    serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|_| BundleError::InvalidArtifactEncoding)
}

fn decode_json_lines<T: DeserializeOwned>(
    bytes: &[u8],
    maximum_count: usize,
    limits: BundleLimits,
    resource: &'static str,
) -> Result<Vec<T>, BundleError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    validate_artifact_size(bytes, limits)?;
    if !bytes.ends_with(b"\n") {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    let mut values = Vec::new();
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        if line.is_empty() || line.contains(&b'\r') {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        if values.len() >= maximum_count {
            return Err(BundleError::LimitExceeded { resource });
        }
        let value =
            serde_json::from_slice(line).map_err(|_| BundleError::InvalidArtifactEncoding)?;
        values.push(value);
    }
    Ok(values)
}

fn validate_json_bytes(bytes: &[u8], limits: BundleLimits) -> Result<(), BundleError> {
    validate_artifact_size(bytes, limits)?;
    if bytes.len() <= 1
        || !bytes.ends_with(b"\n")
        || bytes[..bytes.len() - 1]
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_artifact_size(bytes: &[u8], limits: BundleLimits) -> Result<(), BundleError> {
    let length = u64::try_from(bytes.len()).map_err(|_| BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    if length > limits.max_artifact_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
    }
    Ok(())
}

fn map_decode_error(error: crate::DecodeError) -> BundleError {
    match error {
        crate::DecodeError::Limits(source) => source,
        crate::DecodeError::LimitExceeded { resource } => BundleError::LimitExceeded { resource },
        _ => BundleError::InvalidArtifactEncoding,
    }
}

fn validate_fixed_bundle(fixed: &FixedArtifacts, limits: BundleLimits) -> Result<(), BundleError> {
    validate_environment(&fixed.environment, limits)?;
    validate_manifest_revision(&fixed.dataset_manifest)?;
    validate_build_provenance(&fixed.environment, &fixed.build_provenance, limits)?;
    validate_samples(
        &fixed.dataset_manifest,
        &fixed.command,
        &fixed.raw_samples,
        limits,
    )?;
    validate_summary(&fixed.raw_samples, &fixed.summary, limits)?;
    validate_coverage(
        &fixed.dataset_manifest,
        &fixed.raw_samples,
        &fixed.coverage,
        limits,
    )?;
    validate_quality(&fixed.raw_samples, &fixed.summary, &fixed.quality, limits)?;
    validate_trajectories(&fixed.agent_trajectories, limits)
}

fn validate_environment(
    environment: &EnvironmentEvidence,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(&environment.schema_version)?;
    validate_label(&environment.feature_profile, limits)?;
    for value in [
        &environment.cpu_model,
        &environment.cpu_topology,
        &environment.operating_system,
        &environment.kernel,
        &environment.filesystem,
        &environment.storage_device,
        &environment.power_mode,
        &environment.container_limits,
        &environment.compiler,
        &environment.sqlite,
        &environment.locale,
        &environment.background_process_policy,
        &environment.clock_source,
    ] {
        validate_string_evidence(value, limits, StringKind::Text)?;
    }
    validate_evidence_reason(&environment.ram_bytes, limits)?;
    validate_string_evidence(&environment.binary_sha256, limits, StringKind::Digest)?;
    validate_map_evidence(&environment.adapter_versions, limits, StringKind::Text)?;
    validate_map_evidence(&environment.grammar_versions, limits, StringKind::Text)?;
    validate_map_evidence(
        &environment.grammar_source_package_checksums,
        limits,
        StringKind::Digest,
    )?;
    validate_map_evidence(&environment.grammar_hashes, limits, StringKind::Digest)?;
    validate_availability(&environment.process_tree_accounting, limits)
}

fn validate_manifest_revision(manifest: &DatasetManifest) -> Result<(), BundleError> {
    let mut hasher = Sha256::new();
    for entry in &manifest.entries {
        hash_length_prefixed(&mut hasher, entry.id.as_bytes())?;
        hash_length_prefixed(&mut hasher, entry.source_sha256.as_bytes())?;
    }
    let expected = format!("sha256:{}", hex_digest(hasher.finalize()));
    if manifest.revision != expected {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_build_provenance(
    environment: &EnvironmentEvidence,
    provenance: &BuildProvenance,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(&provenance.schema_version)?;
    validate_revision(&provenance.source_revision)?;
    validate_label(&provenance.build_profile, limits)?;
    validate_label(&provenance.target, limits)?;
    validate_sorted_labels(&provenance.features, limits)?;
    let EvidenceValue::Observed {
        value: binary_sha256,
    } = &environment.binary_sha256
    else {
        return Err(BundleError::ArtifactInvariantViolation);
    };
    if provenance.binary_revision != format!("sha256:{binary_sha256}")
        || provenance.build_profile != environment.feature_profile
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_samples(
    manifest: &DatasetManifest,
    command: &BenchmarkCommand,
    samples: &[RawSample],
    limits: BundleLimits,
) -> Result<(), BundleError> {
    let rounds = usize::try_from(
        command
            .warmup_rounds
            .checked_add(command.trial_rounds)
            .ok_or(BundleError::ArtifactInvariantViolation)?,
    )
    .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let expected_count = manifest
        .entries
        .len()
        .checked_mul(rounds)
        .ok_or(BundleError::ArtifactInvariantViolation)?;
    if samples.len() != expected_count {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    let entries = manifest
        .entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut phase_counts = BTreeMap::<(&str, &str), usize>::new();
    for (ordinal, sample) in samples.iter().enumerate() {
        validate_schema(&sample.schema_version)?;
        let expected_ordinal =
            u64::try_from(ordinal).map_err(|_| BundleError::ArtifactInvariantViolation)?;
        if sample.ordinal != expected_ordinal {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        validate_label(&sample.dataset_entry_id, limits)?;
        validate_label(&sample.grammar_family, limits)?;
        if !matches!(sample.phase.as_str(), "warmup" | "trial") {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        let entry = entries
            .get(sample.dataset_entry_id.as_str())
            .ok_or(BundleError::ArtifactInvariantViolation)?;
        if sample.grammar_family != entry.grammar_family
            || sample.source_bytes != entry.source_bytes
            || sample.physical_lines != entry.physical_lines
        {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        validate_observation(&sample.semantic_facts, limits)?;
        validate_observation(&sample.process_tree_cpu_ns, limits)?;
        validate_observation(&sample.process_tree_peak_rss_bytes, limits)?;
        validate_sample_outcome(sample, limits)?;
        *phase_counts
            .entry((sample.phase.as_str(), sample.dataset_entry_id.as_str()))
            .or_default() += 1;
    }
    let warmup_rounds = usize::try_from(command.warmup_rounds)
        .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let trial_rounds = usize::try_from(command.trial_rounds)
        .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    for entry in &manifest.entries {
        if phase_counts
            .get(&("warmup", entry.id.as_str()))
            .copied()
            .unwrap_or(0)
            != warmup_rounds
            || phase_counts
                .get(&("trial", entry.id.as_str()))
                .copied()
                .unwrap_or(0)
                != trial_rounds
        {
            return Err(BundleError::ArtifactInvariantViolation);
        }
    }
    Ok(())
}

fn validate_sample_outcome(sample: &RawSample, limits: BundleLimits) -> Result<(), BundleError> {
    match &sample.outcome {
        SampleOutcome::Succeeded => {}
        SampleOutcome::Failed { error_code } => {
            validate_reason(error_code, limits)?;
        }
        SampleOutcome::TimedOut | SampleOutcome::Cancelled => {}
    }
    if !matches!(sample.outcome, SampleOutcome::Succeeded)
        && (sample.syntax_nodes != 0
            || sample.syntax_facts != 0
            || !matches!(sample.semantic_facts, EvidenceValue::Unavailable { .. }))
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_summary(
    samples: &[RawSample],
    summary: &ResultSummary,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(&summary.schema_version)?;
    validate_label(&summary.benchmark_id, limits)?;
    validate_availability(&summary.semantic_eligibility, limits)?;
    validate_availability(&summary.confidence_intervals, limits)?;
    if summary.families.len() > limits.max_manifest_entries {
        return Err(BundleError::LimitExceeded {
            resource: "summary_family_count",
        });
    }
    let mut family_counts = BTreeMap::<&str, (u64, u64)>::new();
    let mut failed = 0_u64;
    let mut timed_out = 0_u64;
    let mut cancelled = 0_u64;
    for sample in samples.iter().filter(|sample| sample.phase == "trial") {
        match sample.outcome {
            SampleOutcome::Succeeded => {
                let counts = family_counts
                    .entry(sample.grammar_family.as_str())
                    .or_default();
                counts.0 = counts
                    .0
                    .checked_add(1)
                    .ok_or(BundleError::ArtifactInvariantViolation)?;
                counts.1 = counts
                    .1
                    .checked_add(u64::from(sample.is_outlier))
                    .ok_or(BundleError::ArtifactInvariantViolation)?;
            }
            SampleOutcome::Failed { .. } => {
                failed = failed
                    .checked_add(1)
                    .ok_or(BundleError::ArtifactInvariantViolation)?;
            }
            SampleOutcome::TimedOut => {
                timed_out = timed_out
                    .checked_add(1)
                    .ok_or(BundleError::ArtifactInvariantViolation)?;
            }
            SampleOutcome::Cancelled => {
                cancelled = cancelled
                    .checked_add(1)
                    .ok_or(BundleError::ArtifactInvariantViolation)?;
            }
        }
    }
    let expected_families = family_counts.keys().copied().collect::<BTreeSet<_>>();
    let actual_families = summary
        .families
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected_families != actual_families
        || summary.failed_samples != failed
        || summary.timed_out_samples != timed_out
        || summary.cancelled_samples != cancelled
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    for (family, distribution) in &summary.families {
        validate_label(family, limits)?;
        validate_distribution(distribution, limits)?;
        let expected = family_counts
            .get(family.as_str())
            .ok_or(BundleError::ArtifactInvariantViolation)?;
        if distribution.sample_count != expected.0 || distribution.outlier_count != expected.1 {
            return Err(BundleError::ArtifactInvariantViolation);
        }
    }
    Ok(())
}

fn validate_distribution(
    distribution: &MetricDistribution,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    for value in [
        &distribution.p50_ns,
        &distribution.p95_ns,
        &distribution.p99_ns,
        &distribution.physical_lines_per_second,
        &distribution.files_per_second,
        &distribution.syntax_nodes_per_second,
        &distribution.syntax_facts_per_source_byte_ppm,
    ] {
        validate_observation(value, limits)?;
    }
    if distribution.outlier_count > distribution.sample_count {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_coverage(
    manifest: &DatasetManifest,
    samples: &[RawSample],
    coverage: &CoverageEvidence,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(&coverage.schema_version)?;
    if coverage.skipped.len() > limits.max_manifest_entries
        || coverage.parser_status.len() > limits.max_manifest_entries
    {
        return Err(BundleError::LimitExceeded {
            resource: "coverage_entry_count",
        });
    }
    if !coverage.skipped.is_empty() {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    let mut statuses = manifest
        .entries
        .iter()
        .map(|entry| (entry.id.clone(), "succeeded".to_owned()))
        .collect::<BTreeMap<_, _>>();
    for sample in samples.iter().filter(|sample| sample.phase == "trial") {
        let observed = outcome_status(&sample.outcome);
        let status = statuses
            .get_mut(&sample.dataset_entry_id)
            .ok_or(BundleError::ArtifactInvariantViolation)?;
        if status_severity(observed) > status_severity(status) {
            *status = observed.to_owned();
        }
    }
    for (entry, status) in &coverage.parser_status {
        validate_label(entry, limits)?;
        if !matches!(
            status.as_str(),
            "succeeded" | "cancelled" | "timed_out" | "failed"
        ) {
            return Err(BundleError::InvalidArtifactEncoding);
        }
    }
    let attempted =
        u64::try_from(statuses.len()).map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let committed = u64::try_from(
        statuses
            .values()
            .filter(|status| **status == "succeeded")
            .count(),
    )
    .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    if coverage.parser_status != statuses
        || coverage.attempted_entries != attempted
        || coverage.committed_entries != committed
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn outcome_status(outcome: &SampleOutcome) -> &'static str {
    match outcome {
        SampleOutcome::Succeeded => "succeeded",
        SampleOutcome::Cancelled => "cancelled",
        SampleOutcome::TimedOut => "timed_out",
        SampleOutcome::Failed { .. } => "failed",
    }
}

fn status_severity(status: &str) -> u8 {
    match status {
        "succeeded" => 0,
        "cancelled" => 1,
        "timed_out" => 2,
        "failed" => 3,
        _ => u8::MAX,
    }
}

fn validate_quality(
    samples: &[RawSample],
    summary: &ResultSummary,
    quality: &QualityEvidence,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(&quality.schema_version)?;
    if quality.rubric_id != SEMANTIC_QUALITY_RUBRIC_ID {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    validate_availability(&quality.semantic_eligibility, limits)?;
    validate_observation(&quality.precision_ppm, limits)?;
    validate_observation(&quality.recall_ppm, limits)?;
    validate_observation(&quality.expected_calibration_error_ppm, limits)?;
    if quality.unsupported_cases.len() > limits.max_manifest_entries {
        return Err(BundleError::LimitExceeded {
            resource: "unsupported_case_count",
        });
    }
    for category in quality.unsupported_cases.keys() {
        validate_label(category, limits)?;
    }
    let measurement = SemanticQualityMeasurement {
        precision_ppm: quality.precision_ppm.clone(),
        recall_ppm: quality.recall_ppm.clone(),
        expected_calibration_error_ppm: quality.expected_calibration_error_ppm.clone(),
        unsupported_cases: quality.unsupported_cases.clone(),
    };
    let fact_eligibility = semantic_fact_eligibility(samples);
    let expected = semantic_quality_eligibility(&fact_eligibility, &measurement);
    if quality.semantic_eligibility != expected || summary.semantic_eligibility != expected {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_trajectories(
    trajectories: &[AgentTrajectory],
    limits: BundleLimits,
) -> Result<(), BundleError> {
    for trajectory in trajectories {
        validate_schema(&trajectory.schema_version)?;
        validate_label(&trajectory.task_id, limits)?;
        validate_availability(&trajectory.eligibility, limits)?;
        validate_evidence_reason(&trajectory.total_tokens, limits)?;
        if trajectory.tool_calls.len() > limits.max_command_arguments {
            return Err(BundleError::LimitExceeded {
                resource: "trajectory_tool_call_count",
            });
        }
        for tool_call in &trajectory.tool_calls {
            validate_text(tool_call, limits)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum StringKind {
    Text,
    Digest,
}

fn validate_string_evidence(
    value: &EvidenceValue<String>,
    limits: BundleLimits,
    kind: StringKind,
) -> Result<(), BundleError> {
    match value {
        EvidenceValue::Observed { value } | EvidenceValue::Target { value } => match kind {
            StringKind::Text => validate_text(value, limits),
            StringKind::Digest => validate_digest(value),
        },
        EvidenceValue::Unavailable { reason_code } => validate_reason(reason_code, limits),
    }
}

fn validate_map_evidence(
    value: &EvidenceValue<BTreeMap<String, String>>,
    limits: BundleLimits,
    value_kind: StringKind,
) -> Result<(), BundleError> {
    match value {
        EvidenceValue::Observed { value } | EvidenceValue::Target { value } => {
            if value.len() > limits.max_manifest_entries {
                return Err(BundleError::LimitExceeded {
                    resource: "evidence_map_entry_count",
                });
            }
            for (key, mapped) in value {
                validate_label(key, limits)?;
                match value_kind {
                    StringKind::Text => validate_text(mapped, limits)?,
                    StringKind::Digest => validate_digest(mapped)?,
                }
            }
            Ok(())
        }
        EvidenceValue::Unavailable { reason_code } => validate_reason(reason_code, limits),
    }
}

fn validate_observation<T>(
    value: &EvidenceValue<T>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    match value {
        EvidenceValue::Observed { .. } => Ok(()),
        EvidenceValue::Unavailable { reason_code } => validate_reason(reason_code, limits),
        EvidenceValue::Target { .. } => Err(BundleError::ArtifactInvariantViolation),
    }
}

fn validate_evidence_reason<T>(
    value: &EvidenceValue<T>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    if let EvidenceValue::Unavailable { reason_code } = value {
        validate_reason(reason_code, limits)?;
    }
    Ok(())
}

fn validate_availability(
    availability: &Availability,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    match availability {
        Availability::Available => Ok(()),
        Availability::Failed { reason_code } | Availability::Unavailable { reason_code } => {
            validate_reason(reason_code, limits)
        }
    }
}

fn validate_schema(schema: &str) -> Result<(), BundleError> {
    if schema != RESULT_BUNDLE_SCHEMA_VERSION {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_sorted_labels(values: &[String], limits: BundleLimits) -> Result<(), BundleError> {
    if values.len() > limits.max_command_arguments {
        return Err(BundleError::LimitExceeded {
            resource: "feature_count",
        });
    }
    let mut prior: Option<&str> = None;
    for value in values {
        validate_label(value, limits)?;
        if prior.is_some_and(|previous| value.as_str() <= previous) {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        prior = Some(value);
    }
    Ok(())
}

fn validate_label(value: &str, limits: BundleLimits) -> Result<(), BundleError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_reason(value: &str, limits: BundleLimits) -> Result<(), BundleError> {
    validate_label(value, limits)
}

fn validate_text(value: &str, limits: BundleLimits) -> Result<(), BundleError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || value.chars().any(char::is_control)
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), BundleError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), BundleError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), BundleError> {
    let length = u64::try_from(bytes.len()).map_err(|_| BundleError::ArtifactInvariantViolation)?;
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
