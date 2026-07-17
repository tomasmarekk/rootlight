//! Strict fixed-artifact decoding and cross-artifact result validation.
//!
//! The same bounded wire checks protect publication and later verification so
//! checksums cannot legitimize internally contradictory benchmark evidence.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use sha2::{Digest as _, Sha256};

use crate::bundle::{
    AGENT_TRAJECTORIES_FILE, BUILD_PROVENANCE_FILE, BundleError, COMMAND_FILE, COVERAGE_FILE,
    DATASET_MANIFEST_FILE, ENVIRONMENT_FILE, FIXED_ARTIFACTS, QUALITY_FILE, RAW_SAMPLES_FILE,
    SUMMARY_FILE, json_bytes, json_lines,
};
use crate::decode::{CollectionKind, preflight_artifact_collection};
use crate::parser::{
    ScheduledSample, build_schedule, outlier_fences, semantic_fact_eligibility,
    semantic_quality_eligibility_from_values, summarize,
};
use crate::{
    AgentTrajectory, Availability, BuildProvenance, BundleLimits, CoverageEvidence,
    DatasetManifest, EnvironmentEvidence, EvidenceValue, MetricDistribution, QualityEvidence,
    RESULT_BUNDLE_SCHEMA_VERSION, RawSample, ResultSummary, SEMANTIC_QUALITY_RUBRIC_ID,
    SampleOutcome, decode_benchmark_command, decode_dataset_manifest,
};

#[derive(Debug)]
struct FixedArtifacts {
    environment: EnvironmentEvidence,
    dataset_manifest: DatasetManifest,
    build_provenance: BuildProvenance,
    schedule: Vec<ScheduledSample>,
    raw_samples: Vec<RawSample>,
    summary: ResultSummary,
    coverage: CoverageEvidence,
    quality: QualityEvidence,
    agent_trajectories: Vec<AgentTrajectory>,
}

pub(crate) fn is_fixed_artifact(relative: &str) -> bool {
    FIXED_ARTIFACTS.contains(&relative)
}

pub(crate) trait FixedArtifactSource {
    fn artifact_bytes(&self, name: &str) -> Option<&[u8]>;
}

pub(crate) fn validate_fixed_artifacts<S>(
    artifacts: &S,
    limits: BundleLimits,
) -> Result<(), BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    let fixed = decode_fixed_artifacts(artifacts, limits)?;
    validate_fixed_bundle(&fixed, limits)
}

fn decode_fixed_artifacts<S>(
    artifacts: &S,
    limits: BundleLimits,
) -> Result<FixedArtifacts, BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    for name in FIXED_ARTIFACTS {
        if artifacts.artifact_bytes(name).is_none() {
            return Err(BundleError::ArtifactSetMismatch);
        }
    }
    let manifest_bytes = fixed_bytes(artifacts, DATASET_MANIFEST_FILE)?;
    validate_json_bytes(manifest_bytes, limits)?;
    let dataset_manifest =
        decode_dataset_manifest(manifest_bytes, limits).map_err(map_decode_error)?;
    validate_canonical_json(manifest_bytes, &dataset_manifest, limits)?;
    let command_bytes = fixed_bytes(artifacts, COMMAND_FILE)?;
    validate_json_bytes(command_bytes, limits)?;
    let command = decode_benchmark_command(command_bytes, limits).map_err(map_decode_error)?;
    validate_canonical_json(command_bytes, &command, limits)?;
    let schedule = build_schedule(
        dataset_manifest.entries.len(),
        command.warmup_rounds,
        command.trial_rounds,
        command.seed,
        limits.max_raw_samples,
    )
    .map_err(map_parser_integrity_error)?;
    for field in [
        "adapter_versions",
        "grammar_versions",
        "grammar_source_package_checksums",
        "grammar_hashes",
    ] {
        preflight_fixed_collection(
            artifacts,
            ENVIRONMENT_FILE,
            &[field, "value"],
            CollectionKind::Object,
            limits.max_manifest_entries,
            "evidence_map_entry_count",
            false,
            limits,
        )?;
    }
    preflight_fixed_collection(
        artifacts,
        BUILD_PROVENANCE_FILE,
        &["features"],
        CollectionKind::Array,
        limits.max_command_arguments,
        "feature_count",
        true,
        limits,
    )?;
    preflight_fixed_collection(
        artifacts,
        SUMMARY_FILE,
        &["families"],
        CollectionKind::Object,
        limits.max_manifest_entries,
        "summary_family_count",
        true,
        limits,
    )?;
    for field in ["skipped", "parser_status"] {
        preflight_fixed_collection(
            artifacts,
            COVERAGE_FILE,
            &[field],
            CollectionKind::Object,
            limits.max_manifest_entries,
            "coverage_entry_count",
            true,
            limits,
        )?;
    }
    preflight_fixed_collection(
        artifacts,
        QUALITY_FILE,
        &["unsupported_cases"],
        CollectionKind::Object,
        limits.max_manifest_entries,
        "unsupported_case_count",
        true,
        limits,
    )?;
    Ok(FixedArtifacts {
        environment: decode_json(fixed_bytes(artifacts, ENVIRONMENT_FILE)?, limits)?,
        dataset_manifest,
        build_provenance: decode_json(fixed_bytes(artifacts, BUILD_PROVENANCE_FILE)?, limits)?,
        raw_samples: decode_json_lines(
            fixed_bytes(artifacts, RAW_SAMPLES_FILE)?,
            schedule.len(),
            limits,
            "raw_sample_count",
        )?,
        schedule,
        summary: decode_json(fixed_bytes(artifacts, SUMMARY_FILE)?, limits)?,
        coverage: decode_json(fixed_bytes(artifacts, COVERAGE_FILE)?, limits)?,
        quality: decode_json(fixed_bytes(artifacts, QUALITY_FILE)?, limits)?,
        agent_trajectories: decode_agent_trajectories(
            fixed_bytes(artifacts, AGENT_TRAJECTORIES_FILE)?,
            limits,
        )?,
    })
}

#[allow(clippy::too_many_arguments)]
fn preflight_fixed_collection<S>(
    artifacts: &S,
    artifact: &str,
    path: &[&'static str],
    kind: CollectionKind,
    maximum: usize,
    resource: &'static str,
    required: bool,
    limits: BundleLimits,
) -> Result<(), BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    let bytes = fixed_bytes(artifacts, artifact)?;
    validate_json_bytes(bytes, limits)?;
    preflight_artifact_collection(
        &bytes[..bytes.len() - 1],
        path,
        kind,
        maximum,
        resource,
        required,
    )
}

fn fixed_bytes<'a, S>(artifacts: &'a S, name: &str) -> Result<&'a [u8], BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    artifacts
        .artifact_bytes(name)
        .ok_or(BundleError::ArtifactSetMismatch)
}

fn decode_json<T: DeserializeOwned + serde::Serialize>(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<T, BundleError> {
    validate_json_bytes(bytes, limits)?;
    let value = serde_json::from_slice(&bytes[..bytes.len() - 1])
        .map_err(|_| BundleError::InvalidArtifactEncoding)?;
    validate_canonical_json(bytes, &value, limits)?;
    Ok(value)
}

fn decode_json_lines<T: DeserializeOwned + serde::Serialize>(
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
    let line_count = bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .count();
    if line_count > maximum_count {
        return Err(BundleError::LimitExceeded { resource });
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(line_count)
        .map_err(|_| BundleError::AllocationFailed)?;
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
    let canonical_limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    if json_lines(&values, canonical_limit)? != bytes {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(values)
}

fn decode_agent_trajectories(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<Vec<AgentTrajectory>, BundleError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    validate_artifact_size(bytes, limits)?;
    if !bytes.ends_with(b"\n") {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    let lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
    let line_count = lines.clone().count();
    if line_count > limits.max_agent_trajectories {
        return Err(BundleError::LimitExceeded {
            resource: "agent_trajectory_count",
        });
    }
    for line in lines {
        if line.is_empty() || line.contains(&b'\r') {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        preflight_artifact_collection(
            line,
            &["tool_calls"],
            CollectionKind::Array,
            limits.max_command_arguments,
            "trajectory_tool_call_count",
            true,
        )?;
    }
    Err(BundleError::UnsupportedTrajectorySchema)
}

fn validate_canonical_json(
    bytes: &[u8],
    value: &impl serde::Serialize,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    let canonical_limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    if json_bytes(value, canonical_limit)? != bytes {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
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
        crate::DecodeError::AllocationFailed => BundleError::AllocationFailed,
        crate::DecodeError::InvalidSchema => BundleError::UnsupportedSchemaVersion,
        _ => BundleError::InvalidArtifactEncoding,
    }
}

fn map_parser_integrity_error(error: crate::ParserRunError) -> BundleError {
    match error {
        crate::ParserRunError::AllocationFailed => BundleError::AllocationFailed,
        _ => BundleError::ArtifactInvariantViolation,
    }
}

fn validate_fixed_bundle(fixed: &FixedArtifacts, limits: BundleLimits) -> Result<(), BundleError> {
    validate_environment(&fixed.environment, limits)?;
    validate_manifest_revision(&fixed.dataset_manifest)?;
    validate_build_provenance(&fixed.environment, &fixed.build_provenance, limits)?;
    validate_samples(
        &fixed.dataset_manifest,
        &fixed.schedule,
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
    schedule: &[ScheduledSample],
    samples: &[RawSample],
    limits: BundleLimits,
) -> Result<(), BundleError> {
    if samples.len() != schedule.len() {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    for (ordinal, (sample, scheduled)) in samples.iter().zip(schedule).enumerate() {
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
        let entry = manifest
            .entries
            .get(scheduled.input_index)
            .ok_or(BundleError::ArtifactInvariantViolation)?;
        if sample.phase != scheduled.phase.as_str()
            || sample.dataset_entry_id != entry.id
            || sample.grammar_family != entry.grammar_family
            || sample.source_bytes != entry.source_bytes
            || sample.physical_lines != entry.physical_lines
        {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        validate_observation(&sample.semantic_facts, limits)?;
        validate_observation(&sample.process_tree_cpu_ns, limits)?;
        validate_observation(&sample.process_tree_peak_rss_bytes, limits)?;
        validate_sample_outcome(sample, limits)?;
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
    for (family, distribution) in &summary.families {
        validate_label(family, limits)?;
        validate_distribution(distribution, limits)?;
    }
    let fences = outlier_fences(samples).map_err(map_parser_integrity_error)?;
    for sample in samples {
        let expected =
            if sample.phase == "trial" && matches!(sample.outcome, SampleOutcome::Succeeded) {
                fences
                    .get(&sample.grammar_family)
                    .is_some_and(|(lower, upper)| {
                        let elapsed = u128::from(sample.elapsed_ns);
                        elapsed < *lower || elapsed > *upper
                    })
            } else {
                false
            };
        if sample.is_outlier != expected {
            return Err(BundleError::ArtifactInvariantViolation);
        }
    }
    let expected = summarize(samples, summary.semantic_eligibility.clone())
        .map_err(map_parser_integrity_error)?;
    if *summary != expected {
        return Err(BundleError::ArtifactInvariantViolation);
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
    let mut statuses = Vec::new();
    statuses
        .try_reserve_exact(manifest.entries.len())
        .map_err(|_| BundleError::AllocationFailed)?;
    statuses.extend(
        manifest
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), "succeeded")),
    );
    for sample in samples.iter().filter(|sample| sample.phase == "trial") {
        let observed = outcome_status(&sample.outcome);
        let index = statuses
            .binary_search_by_key(&sample.dataset_entry_id.as_str(), |(entry, _)| *entry)
            .map_err(|_| BundleError::ArtifactInvariantViolation)?;
        let status = &mut statuses[index].1;
        if status_severity(observed) > status_severity(status) {
            *status = observed;
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
            .iter()
            .filter(|(_, status)| *status == "succeeded")
            .count(),
    )
    .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let statuses_match = coverage
        .parser_status
        .iter()
        .map(|(entry, status)| (entry.as_str(), status.as_str()))
        .eq(statuses.iter().copied());
    if !statuses_match
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
        return Err(BundleError::UnsupportedRubricVersion);
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
    let fact_eligibility = semantic_fact_eligibility(samples);
    let expected = semantic_quality_eligibility_from_values(
        &fact_eligibility,
        &quality.precision_ppm,
        &quality.recall_ppm,
        &quality.expected_calibration_error_ppm,
    );
    if quality.semantic_eligibility != expected || summary.semantic_eligibility != expected {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_trajectories(
    trajectories: &[AgentTrajectory],
    _limits: BundleLimits,
) -> Result<(), BundleError> {
    if !trajectories.is_empty() {
        return Err(BundleError::UnsupportedTrajectorySchema);
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
        return Err(BundleError::UnsupportedSchemaVersion);
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
