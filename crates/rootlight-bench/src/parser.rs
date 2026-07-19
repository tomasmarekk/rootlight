//! Seeded parser benchmark scheduling, execution, and aggregation.
//!
//! All parser work uses `ParseProvider` through `execute_parse`; syntax-only
//! runs remain semantically ineligible until a later extraction probe supplies facts.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, EncodingId, GenerationBoundSnapshot, IncludedRange, LanguageId,
    MemoryAdmissionPolicy, ParseOutput, ParseProvider, ParseRequest, execute_parse,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::SourceRef;
use rootlight_vfs::SourceSnapshot;

use crate::model::MILLION_PPM;
use crate::{
    Availability, BundleError, BundleLimits, CoverageEvidence, DatasetEntry, EvidenceValue,
    MAX_SEMANTIC_CALIBRATION_ERROR_PPM, MIN_SEMANTIC_PRECISION_PPM, MIN_SEMANTIC_RECALL_PPM,
    MetricDistribution, ProcessTreeSample, ProcessTreeSampler, QualityEvidence,
    RESULT_BUNDLE_SCHEMA_VERSION, RawSample, ResultSummary, SEMANTIC_QUALITY_RUBRIC_ID,
    SampleOutcome, SemanticQualityMeasurement,
};

const MAX_SAMPLE_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_INCLUDED_RANGES_PER_INPUT: usize = 4_096;

/// One immutable source input bound to a dataset manifest entry.
#[derive(Debug, Clone)]
pub struct ParserDatasetInput {
    /// Dataset manifest entry.
    pub entry: DatasetEntry,
    /// Stable VFS source snapshot.
    pub snapshot: SourceSnapshot,
    /// Generation-bound full-file source reference.
    pub source: SourceRef,
    /// Optional sorted disjoint included ranges.
    pub included_ranges: Vec<IncludedRange>,
}

/// Explicit deterministic parser benchmark policy.
#[derive(Debug, Clone)]
pub struct ParserBenchmarkConfig {
    /// Randomization seed.
    pub seed: u64,
    /// Unmeasured warm-up rounds.
    pub warmup_rounds: u32,
    /// Retained measured rounds.
    pub trial_rounds: u32,
    /// Per-sample deadline.
    pub timeout: Duration,
    /// Explicit parser request bounds.
    pub limits: AnalysisLimits,
    /// Memory admission policy selected for this run.
    pub memory_policy: MemoryAdmissionPolicy,
    /// Checked dataset, sample-count, and evidence resource ceilings.
    pub evidence_limits: BundleLimits,
}

/// Supplies normalized semantic facts and corpus-backed quality measurements.
pub trait SemanticFactProbe: Send + Sync {
    /// Returns a normalized fact count, or `None` when extraction is unavailable.
    fn semantic_fact_count(&self, output: &ParseOutput) -> Option<u64>;

    /// Returns corpus-backed quality metrics used by the semantic rubric.
    ///
    /// The default keeps count-only probes ineligible. Implementations must
    /// return observed precision, recall, and calibration error before the
    /// harness can report semantic availability.
    fn semantic_quality(&self) -> SemanticQualityMeasurement {
        SemanticQualityMeasurement::unavailable("semantic_quality_not_measured")
    }
}

/// Syntax-only probe that keeps parser performance evidence ineligible.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableSemanticFacts;

impl SemanticFactProbe for UnavailableSemanticFacts {
    fn semantic_fact_count(&self, _output: &ParseOutput) -> Option<u64> {
        None
    }
}

/// Evidence produced by one parser benchmark execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserBenchmarkEvidence {
    /// Retained warm-up and measured samples.
    pub raw_samples: Vec<RawSample>,
    /// Aggregate distributions and eligibility.
    pub summary: ResultSummary,
    /// Attempt and parser coverage.
    pub coverage: CoverageEvidence,
    /// Semantic quality eligibility.
    pub quality: QualityEvidence,
}

/// Runs a seeded bounded parser benchmark through the standard SDK boundary.
///
/// Warm-up and measured schedules are shuffled independently from the supplied
/// seed. Every sample is retained, including failures, timeouts, and outliers.
///
/// # Errors
///
/// Returns [`ParserRunError`] for invalid policy, manifest disagreement,
/// request construction, or elapsed-counter overflow.
pub fn run_parser_benchmark<P, S, E>(
    provider: &P,
    inputs: &[ParserDatasetInput],
    config: &ParserBenchmarkConfig,
    sampler: &S,
    semantic_probe: &E,
) -> Result<ParserBenchmarkEvidence, ParserRunError>
where
    P: ParseProvider + ?Sized,
    S: ProcessTreeSampler + ?Sized,
    E: SemanticFactProbe + ?Sized,
{
    let limits = validate_config(config, inputs)?;
    validate_inputs(inputs, limits, &config.limits)?;
    let schedule = build_schedule(
        inputs.len(),
        config.warmup_rounds,
        config.trial_rounds,
        config.seed,
        limits.max_raw_samples,
    )?;
    let mut raw_samples = Vec::new();
    raw_samples
        .try_reserve_exact(schedule.len())
        .map_err(|_| ParserRunError::AllocationFailed)?;
    let mut entry_status = BTreeMap::new();
    for (ordinal, scheduled) in schedule.into_iter().enumerate() {
        let input = inputs
            .get(scheduled.input_index)
            .ok_or(ParserRunError::ScheduleInvariant)?;
        let request = parse_request(input, &config.limits)?;
        let deadline = Instant::now()
            .checked_add(config.timeout)
            .ok_or(ParserRunError::DeadlineOverflow)?;
        let cancellation = Cancellation::with_deadline(deadline);
        let resource_sample = sampler.begin();
        let started = Instant::now();
        let result = execute_parse(provider, &request, config.memory_policy, &cancellation);
        let elapsed = started.elapsed();
        let resources = resource_sample.finish();
        let elapsed_ns =
            u64::try_from(elapsed.as_nanos()).map_err(|_| ParserRunError::ElapsedOverflow)?;
        let ordinal = u64::try_from(ordinal).map_err(|_| ParserRunError::SampleCountOverflow)?;
        let sample = sample_from_result(
            ordinal,
            scheduled.phase,
            input,
            elapsed_ns,
            resources,
            result,
            semantic_probe,
        )?;
        if scheduled.phase == SamplePhase::Trial {
            update_entry_status(&mut entry_status, &input.entry.id, &sample.outcome);
        }
        raw_samples.push(sample);
    }

    mark_outliers(&mut raw_samples)?;
    let fact_eligibility = semantic_fact_eligibility(&raw_samples);
    let semantic_quality = semantic_probe.semantic_quality();
    let semantic_eligibility = semantic_quality_eligibility(&fact_eligibility, &semantic_quality);
    let summary = summarize(&raw_samples, semantic_eligibility.clone())?;
    let committed_entries = entry_status
        .values()
        .filter(|status| **status == EntryStatus::Succeeded)
        .count();
    let parser_status: BTreeMap<String, String> = entry_status
        .into_iter()
        .map(|(id, status)| (id, status.as_str().to_owned()))
        .collect();
    let coverage = CoverageEvidence {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        attempted_entries: u64::try_from(parser_status.len())
            .map_err(|_| ParserRunError::SampleCountOverflow)?,
        committed_entries: u64::try_from(committed_entries)
            .map_err(|_| ParserRunError::SampleCountOverflow)?,
        skipped: BTreeMap::new(),
        parser_status,
    };
    let quality = QualityEvidence {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        rubric_id: SEMANTIC_QUALITY_RUBRIC_ID.to_owned(),
        semantic_eligibility,
        precision_ppm: semantic_quality.precision_ppm,
        recall_ppm: semantic_quality.recall_ppm,
        expected_calibration_error_ppm: semantic_quality.expected_calibration_error_ppm,
        unsupported_cases: semantic_quality.unsupported_cases,
    };
    Ok(ParserBenchmarkEvidence {
        raw_samples,
        summary,
        coverage,
        quality,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SamplePhase {
    Warmup,
    Trial,
}

impl SamplePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Trial => "trial",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScheduledSample {
    pub(crate) phase: SamplePhase,
    pub(crate) input_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryStatus {
    Succeeded,
    Cancelled,
    TimedOut,
    Failed,
}

impl EntryStatus {
    const fn severity(self) -> u8 {
        match self {
            Self::Succeeded => 0,
            Self::Cancelled => 1,
            Self::TimedOut => 2,
            Self::Failed => 3,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Failed => "failed",
        }
    }

    fn from_outcome(outcome: &SampleOutcome) -> Self {
        match outcome {
            SampleOutcome::Succeeded => Self::Succeeded,
            SampleOutcome::Failed { .. } => Self::Failed,
            SampleOutcome::TimedOut => Self::TimedOut,
            SampleOutcome::Cancelled => Self::Cancelled,
        }
    }
}

fn update_entry_status(
    statuses: &mut BTreeMap<String, EntryStatus>,
    entry_id: &str,
    outcome: &SampleOutcome,
) {
    let observed = EntryStatus::from_outcome(outcome);
    statuses
        .entry(entry_id.to_owned())
        .and_modify(|current| {
            if observed.severity() > current.severity() {
                *current = observed;
            }
        })
        .or_insert(observed);
}

fn validate_config(
    config: &ParserBenchmarkConfig,
    inputs: &[ParserDatasetInput],
) -> Result<BundleLimits, ParserRunError> {
    let limits = config
        .evidence_limits
        .validate()
        .map_err(ParserRunError::InputLimits)?;
    if inputs.is_empty() {
        return Err(ParserRunError::EmptyDataset);
    }
    if inputs.len() > limits.max_manifest_entries {
        return Err(ParserRunError::TooManyDatasetEntries);
    }
    if config.trial_rounds == 0 {
        return Err(ParserRunError::ZeroTrialRounds);
    }
    if config.timeout.is_zero() {
        return Err(ParserRunError::ZeroTimeout);
    }
    if config.timeout > MAX_SAMPLE_TIMEOUT {
        return Err(ParserRunError::TimeoutTooLarge);
    }
    Ok(limits)
}

fn validate_inputs(
    inputs: &[ParserDatasetInput],
    limits: BundleLimits,
    analysis_limits: &AnalysisLimits,
) -> Result<(), ParserRunError> {
    let mut prior_id: Option<&str> = None;
    let mut declared_total = 0_u64;
    let mut observed_total = 0_u64;
    let analysis_source_limit = u64::try_from(analysis_limits.max_source_bytes())
        .map_err(|_| ParserRunError::SourceSizeOverflow)?;
    let range_limit = analysis_limits
        .max_embedded_ranges()
        .min(MAX_INCLUDED_RANGES_PER_INPUT);
    for input in inputs {
        if prior_id.is_some_and(|prior| prior >= input.entry.id.as_str()) {
            return Err(ParserRunError::DatasetOrder);
        }
        prior_id = Some(&input.entry.id);
        validate_dataset_label(&input.entry.id, limits, ParserRunError::InvalidDatasetId)?;
        validate_dataset_label(
            &input.entry.grammar_family,
            limits,
            ParserRunError::InvalidGrammarFamily,
        )?;
        validate_dataset_label(
            &input.entry.language,
            limits,
            ParserRunError::InvalidLanguage,
        )?;
        validate_relative_path(&input.entry.relative_path, limits)?;
        if input.included_ranges.len() > range_limit {
            return Err(ParserRunError::TooManyIncludedRanges);
        }
        if input.entry.source_bytes > limits.max_snapshot_bytes
            || input.entry.source_bytes > analysis_source_limit
        {
            return Err(ParserRunError::SnapshotTooLarge);
        }
        declared_total = checked_dataset_total(
            declared_total,
            input.entry.source_bytes,
            limits.max_dataset_source_bytes,
        )?;
        let observed_bytes = u64::try_from(input.snapshot.content().len())
            .map_err(|_| ParserRunError::SourceSizeOverflow)?;
        if observed_bytes > limits.max_snapshot_bytes || observed_bytes > analysis_source_limit {
            return Err(ParserRunError::SnapshotTooLarge);
        }
        observed_total = checked_dataset_total(
            observed_total,
            observed_bytes,
            limits.max_dataset_source_bytes,
        )?;
        if observed_bytes != input.entry.source_bytes {
            return Err(ParserRunError::SourceSizeMismatch);
        }
    }
    for input in inputs {
        validate_source_digest(&input.entry.source_sha256)?;
        let observed_hash = sha256_hex(input.snapshot.content());
        if observed_hash != input.entry.source_sha256 {
            return Err(ParserRunError::SourceHashMismatch);
        }
        let observed_lines = physical_lines(input.snapshot.content())?;
        if observed_lines != input.entry.physical_lines {
            return Err(ParserRunError::PhysicalLineMismatch);
        }
    }
    Ok(())
}

fn checked_dataset_total(current: u64, bytes: u64, limit: u64) -> Result<u64, ParserRunError> {
    let total = current
        .checked_add(bytes)
        .ok_or(ParserRunError::DatasetTooLarge)?;
    if total > limit {
        return Err(ParserRunError::DatasetTooLarge);
    }
    Ok(total)
}

fn validate_dataset_label(
    value: &str,
    limits: BundleLimits,
    error: ParserRunError,
) -> Result<(), ParserRunError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(error);
    }
    Ok(())
}

fn validate_relative_path(value: &str, limits: BundleLimits) -> Result<(), ParserRunError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || value.chars().any(char::is_control)
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains("//")
        || value.split('/').any(|component| {
            component.is_empty() || matches!(component, "." | "..") || component.ends_with('.')
        })
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
    {
        return Err(ParserRunError::InvalidRelativePath);
    }
    Ok(())
}

fn validate_source_digest(value: &str) -> Result<(), ParserRunError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ParserRunError::InvalidSourceDigest);
    }
    Ok(())
}

pub(crate) fn build_schedule(
    input_count: usize,
    warmup_rounds: u32,
    trial_rounds: u32,
    seed: u64,
    max_samples: usize,
) -> Result<Vec<ScheduledSample>, ParserRunError> {
    let total_rounds = warmup_rounds
        .checked_add(trial_rounds)
        .ok_or(ParserRunError::SampleCountOverflow)?;
    let capacity = input_count
        .checked_mul(
            usize::try_from(total_rounds).map_err(|_| ParserRunError::SampleCountOverflow)?,
        )
        .ok_or(ParserRunError::SampleCountOverflow)?;
    if capacity > max_samples {
        return Err(ParserRunError::TooManySamples);
    }
    let mut schedule = Vec::new();
    schedule
        .try_reserve_exact(capacity)
        .map_err(|_| ParserRunError::AllocationFailed)?;
    let mut random = SplitMix64::new(seed);
    for round in 0..total_rounds {
        let phase = if round < warmup_rounds {
            SamplePhase::Warmup
        } else {
            SamplePhase::Trial
        };
        let mut indices = Vec::new();
        indices
            .try_reserve_exact(input_count)
            .map_err(|_| ParserRunError::AllocationFailed)?;
        indices.extend(0..input_count);
        seeded_shuffle(&mut indices, &mut random)?;
        schedule.extend(
            indices
                .into_iter()
                .map(|input_index| ScheduledSample { phase, input_index }),
        );
    }
    Ok(schedule)
}

fn seeded_shuffle(values: &mut [usize], random: &mut SplitMix64) -> Result<(), ParserRunError> {
    for upper in (1..values.len()).rev() {
        let modulus = upper
            .checked_add(1)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(ParserRunError::SampleCountOverflow)?;
        let selected = usize::try_from(random.next() % modulus)
            .map_err(|_| ParserRunError::SampleCountOverflow)?;
        values.swap(upper, selected);
    }
    Ok(())
}

fn parse_request<'a>(
    input: &'a ParserDatasetInput,
    limits: &'a AnalysisLimits,
) -> Result<ParseRequest<'a>, ParserRunError> {
    let source = GenerationBoundSnapshot::new(&input.snapshot, &input.source)?;
    let language = LanguageId::new(&input.entry.language)?;
    let encoding = EncodingId::new("utf-8")?;
    ParseRequest::new(
        source,
        language,
        encoding,
        input.included_ranges.clone(),
        limits,
    )
    .map_err(ParserRunError::Request)
}

fn sample_from_result<E: SemanticFactProbe + ?Sized>(
    ordinal: u64,
    phase: SamplePhase,
    input: &ParserDatasetInput,
    elapsed_ns: u64,
    resources: crate::ProcessTreeMeasurement,
    result: Result<ParseOutput, AdapterError>,
    semantic_probe: &E,
) -> Result<RawSample, ParserRunError> {
    let (syntax_nodes, syntax_facts, semantic_facts, outcome) = match result {
        Ok(output) => (
            u64::try_from(output.report().resources().syntax_nodes())
                .map_err(|_| ParserRunError::MetricOverflow)?,
            u64::try_from(output.facts().len()).map_err(|_| ParserRunError::MetricOverflow)?,
            semantic_probe.semantic_fact_count(&output).map_or_else(
                || EvidenceValue::unavailable("semantic_extraction_not_integrated"),
                EvidenceValue::observed,
            ),
            SampleOutcome::Succeeded,
        ),
        Err(AdapterError::Cancelled { reason }) => {
            let outcome = match reason {
                CancellationReason::DeadlineExceeded => SampleOutcome::TimedOut,
                CancellationReason::ClientRequest
                | CancellationReason::ParentCancelled
                | CancellationReason::Shutdown
                | CancellationReason::ResourceLimit => SampleOutcome::Cancelled,
                _ => SampleOutcome::Cancelled,
            };
            (
                0,
                0,
                EvidenceValue::unavailable("parse_not_committed"),
                outcome,
            )
        }
        Err(error) => (
            0,
            0,
            EvidenceValue::unavailable("parse_not_committed"),
            SampleOutcome::Failed {
                error_code: adapter_error_code(&error).to_owned(),
            },
        ),
    };
    Ok(RawSample {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        ordinal,
        phase: phase.as_str().to_owned(),
        dataset_entry_id: input.entry.id.clone(),
        grammar_family: input.entry.grammar_family.clone(),
        elapsed_ns,
        source_bytes: input.entry.source_bytes,
        physical_lines: input.entry.physical_lines,
        syntax_nodes,
        syntax_facts,
        semantic_facts,
        process_tree_cpu_ns: resources.cpu_ns,
        process_tree_peak_rss_bytes: resources.peak_rss_bytes,
        outcome,
        is_outlier: false,
    })
}

pub(crate) fn mark_outliers(samples: &mut [RawSample]) -> Result<(), ParserRunError> {
    let fences = outlier_fences(samples)?;
    for sample in samples {
        if sample.phase == "trial"
            && matches!(sample.outcome, SampleOutcome::Succeeded)
            && let Some((lower, upper)) = fences.get(&sample.grammar_family)
        {
            let elapsed = u128::from(sample.elapsed_ns);
            sample.is_outlier = elapsed < *lower || elapsed > *upper;
        }
    }
    Ok(())
}

pub(crate) fn outlier_fences(
    samples: &[RawSample],
) -> Result<BTreeMap<String, (u128, u128)>, ParserRunError> {
    let mut families = BTreeMap::<String, Vec<u64>>::new();
    for sample in samples.iter().filter(|sample| {
        sample.phase == "trial" && matches!(sample.outcome, SampleOutcome::Succeeded)
    }) {
        let durations = families.entry(sample.grammar_family.clone()).or_default();
        durations
            .try_reserve(1)
            .map_err(|_| ParserRunError::AllocationFailed)?;
        durations.push(sample.elapsed_ns);
    }
    families
        .into_iter()
        .map(|(family, mut durations)| {
            durations.sort_unstable();
            let q1 = percentile(&durations, 25).ok_or(ParserRunError::MetricOverflow)?;
            let q3 = percentile(&durations, 75).ok_or(ParserRunError::MetricOverflow)?;
            let q1 = u128::from(q1);
            let q3 = u128::from(q3);
            let iqr = q3.checked_sub(q1).ok_or(ParserRunError::MetricOverflow)?;
            let spread = iqr
                .checked_mul(3)
                .and_then(|value| value.checked_div(2))
                .ok_or(ParserRunError::MetricOverflow)?;
            // Saturation represents the mathematical lower fence clipped to
            // the unsigned duration domain; it does not hide a conversion.
            let lower = q1.saturating_sub(spread);
            let upper = q3
                .checked_add(spread)
                .ok_or(ParserRunError::MetricOverflow)?;
            Ok((family, (lower, upper)))
        })
        .collect::<Result<BTreeMap<_, _>, ParserRunError>>()
}

pub(crate) fn summarize(
    samples: &[RawSample],
    semantic_eligibility: Availability,
) -> Result<ResultSummary, ParserRunError> {
    let mut grouped = BTreeMap::<String, Vec<&RawSample>>::new();
    let mut failed_samples = 0_u64;
    let mut timed_out_samples = 0_u64;
    let mut cancelled_samples = 0_u64;
    for sample in samples.iter().filter(|sample| sample.phase == "trial") {
        match sample.outcome {
            SampleOutcome::Succeeded => {
                let family_samples = grouped.entry(sample.grammar_family.clone()).or_default();
                family_samples
                    .try_reserve(1)
                    .map_err(|_| ParserRunError::AllocationFailed)?;
                family_samples.push(sample);
            }
            SampleOutcome::Failed { .. } => {
                failed_samples = failed_samples
                    .checked_add(1)
                    .ok_or(ParserRunError::MetricOverflow)?;
            }
            SampleOutcome::TimedOut => {
                timed_out_samples = timed_out_samples
                    .checked_add(1)
                    .ok_or(ParserRunError::MetricOverflow)?;
            }
            SampleOutcome::Cancelled => {
                cancelled_samples = cancelled_samples
                    .checked_add(1)
                    .ok_or(ParserRunError::MetricOverflow)?;
            }
        }
    }
    let families = grouped
        .into_iter()
        .map(|(family, samples)| distribution(&samples).map(|value| (family, value)))
        .collect::<Result<BTreeMap<_, _>, ParserRunError>>()?;
    Ok(ResultSummary {
        schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
        benchmark_id: "rootlight-parser-benchmark-v1".to_owned(),
        semantic_eligibility,
        families,
        failed_samples,
        timed_out_samples,
        cancelled_samples,
        confidence_intervals: Availability::Unavailable {
            reason_code: "bootstrap_confidence_interval_not_integrated".to_owned(),
        },
    })
}

pub(crate) fn semantic_fact_eligibility(samples: &[RawSample]) -> Availability {
    let mut trials = samples.iter().filter(|sample| sample.phase == "trial");
    let Some(first_trial) = trials.next() else {
        return Availability::Failed {
            reason_code: "no_measured_samples".to_owned(),
        };
    };
    let mut trials = std::iter::once(first_trial).chain(trials);
    if trials
        .clone()
        .any(|sample| !matches!(sample.outcome, SampleOutcome::Succeeded))
    {
        return Availability::Failed {
            reason_code: "incomplete_measured_run".to_owned(),
        };
    }
    if trials.clone().any(|sample| {
        matches!(
            sample.semantic_facts,
            EvidenceValue::Unavailable { .. } | EvidenceValue::Target { .. }
        )
    }) {
        return Availability::Unavailable {
            reason_code: "semantic_extraction_not_integrated".to_owned(),
        };
    }
    if trials.any(|sample| matches!(sample.semantic_facts, EvidenceValue::Observed { value: 0 })) {
        return Availability::Failed {
            reason_code: "semantic_facts_empty".to_owned(),
        };
    }
    Availability::Available
}

pub(crate) fn semantic_quality_eligibility(
    fact_eligibility: &Availability,
    quality: &SemanticQualityMeasurement,
) -> Availability {
    semantic_quality_eligibility_from_values(
        fact_eligibility,
        &quality.precision_ppm,
        &quality.recall_ppm,
        &quality.expected_calibration_error_ppm,
    )
}

pub(crate) fn semantic_quality_eligibility_from_values(
    fact_eligibility: &Availability,
    precision: &EvidenceValue<u64>,
    recall: &EvidenceValue<u64>,
    calibration_error: &EvidenceValue<u64>,
) -> Availability {
    if !matches!(fact_eligibility, Availability::Available) {
        return fact_eligibility.clone();
    }
    let (
        EvidenceValue::Observed {
            value: precision_ppm,
        },
        EvidenceValue::Observed { value: recall_ppm },
        EvidenceValue::Observed {
            value: calibration_error_ppm,
        },
    ) = (precision, recall, calibration_error)
    else {
        return Availability::Unavailable {
            reason_code: "semantic_quality_not_measured".to_owned(),
        };
    };
    if *precision_ppm > MILLION_PPM
        || *recall_ppm > MILLION_PPM
        || *calibration_error_ppm > MILLION_PPM
    {
        return Availability::Failed {
            reason_code: "semantic_quality_metric_out_of_range".to_owned(),
        };
    }
    if *precision_ppm < MIN_SEMANTIC_PRECISION_PPM {
        return Availability::Failed {
            reason_code: "semantic_precision_below_threshold".to_owned(),
        };
    }
    if *recall_ppm < MIN_SEMANTIC_RECALL_PPM {
        return Availability::Failed {
            reason_code: "semantic_recall_below_threshold".to_owned(),
        };
    }
    if *calibration_error_ppm > MAX_SEMANTIC_CALIBRATION_ERROR_PPM {
        return Availability::Failed {
            reason_code: "semantic_calibration_above_threshold".to_owned(),
        };
    }
    Availability::Available
}

fn distribution(samples: &[&RawSample]) -> Result<MetricDistribution, ParserRunError> {
    let mut durations = Vec::new();
    durations
        .try_reserve_exact(samples.len())
        .map_err(|_| ParserRunError::AllocationFailed)?;
    durations.extend(samples.iter().map(|sample| sample.elapsed_ns));
    durations.sort_unstable();
    let elapsed = checked_sum(samples, |sample| sample.elapsed_ns)?;
    let lines = checked_sum(samples, |sample| sample.physical_lines)?;
    let nodes = checked_sum(samples, |sample| sample.syntax_nodes)?;
    let facts = checked_sum(samples, |sample| sample.syntax_facts)?;
    let source_bytes = checked_sum(samples, |sample| sample.source_bytes)?;
    let file_count = u128::try_from(samples.len()).map_err(|_| ParserRunError::MetricOverflow)?;
    Ok(MetricDistribution {
        sample_count: u64::try_from(samples.len()).map_err(|_| ParserRunError::MetricOverflow)?,
        p50_ns: observed_percentile(&durations, 50),
        p95_ns: observed_percentile(&durations, 95),
        p99_ns: observed_percentile(&durations, 99),
        physical_lines_per_second: rate_per_second(lines, elapsed),
        files_per_second: rate_per_second(file_count, elapsed),
        syntax_nodes_per_second: rate_per_second(nodes, elapsed),
        syntax_facts_per_source_byte_ppm: ratio_ppm(facts, source_bytes),
        outlier_count: u64::try_from(samples.iter().filter(|sample| sample.is_outlier).count())
            .map_err(|_| ParserRunError::MetricOverflow)?,
    })
}

fn checked_sum(
    samples: &[&RawSample],
    value: impl Fn(&RawSample) -> u64,
) -> Result<u128, ParserRunError> {
    samples.iter().try_fold(0_u128, |total, sample| {
        total
            .checked_add(u128::from(value(sample)))
            .ok_or(ParserRunError::MetricOverflow)
    })
}

fn observed_percentile(values: &[u64], percent: usize) -> EvidenceValue<u64> {
    percentile(values, percent).map_or_else(
        || EvidenceValue::unavailable("no_successful_samples"),
        EvidenceValue::observed,
    )
}

fn percentile(values: &[u64], percent: usize) -> Option<u64> {
    if values.is_empty() || percent == 0 || percent > 100 {
        return None;
    }
    let rank = percent
        .checked_mul(values.len())?
        .checked_add(99)?
        .checked_div(100)?;
    values.get(rank.checked_sub(1)?).copied()
}

fn rate_per_second(units: u128, elapsed_ns: u128) -> EvidenceValue<u64> {
    if elapsed_ns == 0 {
        return EvidenceValue::unavailable("zero_elapsed_time");
    }
    let Some(value) = units
        .checked_mul(1_000_000_000)
        .map(|scaled| scaled / elapsed_ns)
    else {
        return EvidenceValue::unavailable("metric_overflow");
    };
    u64::try_from(value).map_or_else(
        |_| EvidenceValue::unavailable("metric_overflow"),
        EvidenceValue::observed,
    )
}

fn ratio_ppm(numerator: u128, denominator: u128) -> EvidenceValue<u64> {
    if denominator == 0 {
        return EvidenceValue::unavailable("zero_source_bytes");
    }
    let Some(value) = numerator
        .checked_mul(1_000_000)
        .map(|scaled| scaled / denominator)
    else {
        return EvidenceValue::unavailable("metric_overflow");
    };
    u64::try_from(value).map_or_else(
        |_| EvidenceValue::unavailable("metric_overflow"),
        EvidenceValue::observed,
    )
}

fn physical_lines(bytes: &[u8]) -> Result<u64, ParserRunError> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let newline_count = bytes.iter().filter(|byte| **byte == b'\n').count();
    let trailing_line = usize::from(bytes.last() != Some(&b'\n'));
    let line_count = newline_count
        .checked_add(trailing_line)
        .ok_or(ParserRunError::SourceSizeOverflow)?;
    u64::try_from(line_count).map_err(|_| ParserRunError::SourceSizeOverflow)
}

fn adapter_error_code(error: &AdapterError) -> &'static str {
    match error {
        AdapterError::RejectedRequest(_) => "request_rejected",
        AdapterError::Cancelled { .. } => "cancelled",
        AdapterError::Sink(_) => "sink_rejected",
        AdapterError::InvalidReport(_) => "invalid_report",
        AdapterError::ProviderFailed { .. } => "provider_failed",
        _ => "adapter_error",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[derive(Debug, Clone, Copy)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

/// Parser benchmark admission or execution setup failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParserRunError {
    /// Dataset and evidence limits are invalid.
    #[error("parser benchmark input limits are invalid")]
    InputLimits(#[source] BundleError),
    /// No dataset entries were supplied.
    #[error("parser benchmark dataset is empty")]
    EmptyDataset,
    /// Dataset entry count exceeds the configured hard ceiling.
    #[error("parser benchmark dataset entry count exceeds the limit")]
    TooManyDatasetEntries,
    /// No measured rounds were requested.
    #[error("parser benchmark trial rounds must be nonzero")]
    ZeroTrialRounds,
    /// The per-sample timeout was zero.
    #[error("parser benchmark timeout must be nonzero")]
    ZeroTimeout,
    /// The per-sample timeout exceeds the documented hard ceiling.
    #[error("parser benchmark timeout exceeds the hard limit")]
    TimeoutTooLarge,
    /// The deterministic sample count exceeds its hard limit.
    #[error("parser benchmark sample count exceeds the hard limit")]
    TooManySamples,
    /// Sample-count arithmetic overflowed.
    #[error("parser benchmark sample count is not representable")]
    SampleCountOverflow,
    /// Dataset entries were not strictly ordered by ID.
    #[error("parser benchmark dataset entries are not in canonical order")]
    DatasetOrder,
    /// A stable dataset entry ID is invalid.
    #[error("parser benchmark dataset ID is invalid")]
    InvalidDatasetId,
    /// A grammar-family label is invalid.
    #[error("parser benchmark grammar family is invalid")]
    InvalidGrammarFamily,
    /// A language label is invalid.
    #[error("parser benchmark language is invalid")]
    InvalidLanguage,
    /// A repository-relative path is invalid.
    #[error("parser benchmark relative path is invalid")]
    InvalidRelativePath,
    /// A manifest digest is not canonical lowercase SHA-256.
    #[error("parser benchmark source digest is invalid")]
    InvalidSourceDigest,
    /// One declared or observed snapshot exceeds the configured ceiling.
    #[error("parser benchmark snapshot exceeds the byte limit")]
    SnapshotTooLarge,
    /// Included ranges exceed the configured or benchmark hard ceiling.
    #[error("parser benchmark included ranges exceed the limit")]
    TooManyIncludedRanges,
    /// Aggregate declared or observed dataset bytes exceed the ceiling.
    #[error("parser benchmark dataset exceeds the byte limit")]
    DatasetTooLarge,
    /// Manifest and snapshot source sizes differ.
    #[error("parser benchmark source size differs from its manifest")]
    SourceSizeMismatch,
    /// Source size cannot be represented.
    #[error("parser benchmark source size is not representable")]
    SourceSizeOverflow,
    /// Manifest and snapshot hashes differ.
    #[error("parser benchmark source hash differs from its manifest")]
    SourceHashMismatch,
    /// Manifest and observed physical line counts differ.
    #[error("parser benchmark physical line count differs from its manifest")]
    PhysicalLineMismatch,
    /// The generation-bound snapshot was invalid.
    #[error(transparent)]
    Snapshot(#[from] rootlight_adapter_sdk::SnapshotError),
    /// A bounded parser request could not be constructed.
    #[error(transparent)]
    Request(#[from] rootlight_adapter_sdk::RequestError),
    /// A language or encoding label was invalid.
    #[error(transparent)]
    Label(#[from] rootlight_adapter_sdk::LabelError),
    /// The monotonic deadline was not representable.
    #[error("parser benchmark deadline is not representable")]
    DeadlineOverflow,
    /// The elapsed duration did not fit the result schema.
    #[error("parser benchmark elapsed duration is not representable")]
    ElapsedOverflow,
    /// A retained metric or counter is not representable.
    #[error("parser benchmark metric is not representable")]
    MetricOverflow,
    /// A bounded in-memory reservation could not be satisfied.
    #[error("parser benchmark allocation failed")]
    AllocationFailed,
    /// An internal schedule referred outside the validated dataset.
    #[error("parser benchmark schedule invariant failed")]
    ScheduleInvariant,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use rootlight_adapter_sdk::{
        BatchThresholds, CoverageReport, DiagnosticCode, MemoryEnforcement, ParseCapabilities,
        ParseReport, StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxFactSink, SyntaxKindLabel,
        testkit::MockParseProvider,
    };
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::{AnalysisTier, CoverageStatus, IrLimits, SourceSpan};
    use rootlight_vfs::{RelativePath, RepositoryRoot};

    use super::*;
    use crate::{ProcessTreeMeasurement, ProcessTreeSample, UnavailableProcessTreeSampler};

    struct FixedSemanticFacts(u64);

    impl SemanticFactProbe for FixedSemanticFacts {
        fn semantic_fact_count(&self, _output: &ParseOutput) -> Option<u64> {
            Some(self.0)
        }
    }

    struct MeasuredSemanticFacts {
        count: u64,
        precision_ppm: u64,
        recall_ppm: u64,
        calibration_error_ppm: u64,
    }

    impl SemanticFactProbe for MeasuredSemanticFacts {
        fn semantic_fact_count(&self, _output: &ParseOutput) -> Option<u64> {
            Some(self.count)
        }

        fn semantic_quality(&self) -> SemanticQualityMeasurement {
            SemanticQualityMeasurement {
                precision_ppm: EvidenceValue::observed(self.precision_ppm),
                recall_ppm: EvidenceValue::observed(self.recall_ppm),
                expected_calibration_error_ppm: EvidenceValue::observed(self.calibration_error_ppm),
                unsupported_cases: BTreeMap::new(),
            }
        }
    }

    #[derive(Debug)]
    struct FakeSampler;

    impl ProcessTreeSampler for FakeSampler {
        type Sample = FakeSample;

        fn begin(&self) -> Self::Sample {
            FakeSample
        }
    }

    struct FakeSample;

    impl ProcessTreeSample for FakeSample {
        fn finish(self) -> ProcessTreeMeasurement {
            ProcessTreeMeasurement {
                cpu_ns: EvidenceValue::observed(10),
                peak_rss_bytes: EvidenceValue::observed(20),
            }
        }
    }

    #[derive(Debug)]
    struct TrackingSampler {
        active: Arc<AtomicUsize>,
        begins: Arc<AtomicUsize>,
        finishes: Arc<AtomicUsize>,
    }

    impl ProcessTreeSampler for TrackingSampler {
        type Sample = TrackingSample;

        fn begin(&self) -> Self::Sample {
            self.begins.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            TrackingSample {
                active: Arc::clone(&self.active),
                finishes: Arc::clone(&self.finishes),
            }
        }
    }

    struct TrackingSample {
        active: Arc<AtomicUsize>,
        finishes: Arc<AtomicUsize>,
    }

    impl ProcessTreeSample for TrackingSample {
        fn finish(self) -> ProcessTreeMeasurement {
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.finishes.fetch_add(1, Ordering::SeqCst);
            ProcessTreeMeasurement {
                cpu_ns: EvidenceValue::unavailable("test_sampler_unavailable"),
                peak_rss_bytes: EvidenceValue::unavailable("test_sampler_unavailable"),
            }
        }
    }

    struct ScopeCheckingProvider {
        inner: MockParseProvider,
        active: Arc<AtomicUsize>,
    }

    impl ParseProvider for ScopeCheckingProvider {
        fn capabilities(&self) -> &ParseCapabilities {
            self.inner.capabilities()
        }

        fn parse(
            &self,
            request: &ParseRequest<'_>,
            sink: &mut dyn SyntaxFactSink,
            cancellation: &Cancellation,
        ) -> Result<ParseReport, AdapterError> {
            assert_eq!(self.active.load(Ordering::SeqCst), 1);
            self.inner.parse(request, sink, cancellation)
        }
    }

    struct FailThenSucceedProvider {
        inner: MockParseProvider,
        calls: AtomicUsize,
    }

    impl ParseProvider for FailThenSucceedProvider {
        fn capabilities(&self) -> &ParseCapabilities {
            self.inner.capabilities()
        }

        fn parse(
            &self,
            request: &ParseRequest<'_>,
            sink: &mut dyn SyntaxFactSink,
            cancellation: &Cancellation,
        ) -> Result<ParseReport, AdapterError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(AdapterError::ProviderFailed {
                    code: DiagnosticCode::new("forced_failure")
                        .expect("test failure code is valid"),
                });
            }
            self.inner.parse(request, sink, cancellation)
        }
    }

    #[test]
    fn seeded_schedule_is_repeatable_and_retains_warmups() {
        let fixture = fixture();
        let provider = provider(&fixture.source);
        let config = config(2, 3);

        let first = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config,
            &FakeSampler,
            &FixedSemanticFacts(1),
        )
        .expect("first benchmark succeeds");
        let second = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config,
            &FakeSampler,
            &FixedSemanticFacts(1),
        )
        .expect("second benchmark succeeds");

        let first_order = first
            .raw_samples
            .iter()
            .map(|sample| (&sample.phase, &sample.dataset_entry_id))
            .collect::<Vec<_>>();
        let second_order = second
            .raw_samples
            .iter()
            .map(|sample| (&sample.phase, &sample.dataset_entry_id))
            .collect::<Vec<_>>();
        assert_eq!(first_order, second_order);
        assert_eq!(first.raw_samples.len(), 5);
        assert_eq!(first.coverage.attempted_entries, 1);
        assert_eq!(first.coverage.committed_entries, 1);
        assert_eq!(
            first.summary.semantic_eligibility,
            Availability::Unavailable {
                reason_code: "semantic_quality_not_measured".to_owned()
            }
        );
        assert_eq!(
            first.summary.semantic_eligibility,
            first.quality.semantic_eligibility
        );
        assert!(first.raw_samples.iter().all(|sample| {
            matches!(sample.process_tree_cpu_ns, EvidenceValue::Observed { .. })
        }));
    }

    #[test]
    fn syntax_only_run_cannot_be_semantically_eligible() {
        let fixture = fixture();
        let provider = provider(&fixture.source);

        let evidence = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 1),
            &UnavailableProcessTreeSampler,
            &UnavailableSemanticFacts,
        )
        .expect("syntax benchmark succeeds");

        assert!(matches!(
            evidence.summary.semantic_eligibility,
            Availability::Unavailable { .. }
        ));
    }

    #[test]
    fn measured_quality_at_the_rubric_boundary_is_semantically_eligible() {
        let fixture = fixture();
        let provider = provider(&fixture.source);
        let probe = MeasuredSemanticFacts {
            count: 1,
            precision_ppm: MIN_SEMANTIC_PRECISION_PPM,
            recall_ppm: MIN_SEMANTIC_RECALL_PPM,
            calibration_error_ppm: MAX_SEMANTIC_CALIBRATION_ERROR_PPM,
        };

        let evidence = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 1),
            &UnavailableProcessTreeSampler,
            &probe,
        )
        .expect("quality-backed benchmark succeeds");

        assert_eq!(
            evidence.summary.semantic_eligibility,
            Availability::Available
        );
        assert_eq!(
            evidence.summary.semantic_eligibility,
            evidence.quality.semantic_eligibility
        );
        assert_eq!(
            evidence.quality.precision_ppm,
            EvidenceValue::observed(MIN_SEMANTIC_PRECISION_PPM)
        );
    }

    #[test]
    fn semantic_quality_rubric_rejects_each_failed_or_unmeasured_metric() {
        let facts_available = Availability::Available;
        let cases = [
            (
                SemanticQualityMeasurement {
                    precision_ppm: EvidenceValue::observed(MIN_SEMANTIC_PRECISION_PPM - 1),
                    recall_ppm: EvidenceValue::observed(MIN_SEMANTIC_RECALL_PPM),
                    expected_calibration_error_ppm: EvidenceValue::observed(
                        MAX_SEMANTIC_CALIBRATION_ERROR_PPM,
                    ),
                    unsupported_cases: BTreeMap::new(),
                },
                Availability::Failed {
                    reason_code: "semantic_precision_below_threshold".to_owned(),
                },
            ),
            (
                SemanticQualityMeasurement {
                    precision_ppm: EvidenceValue::observed(MIN_SEMANTIC_PRECISION_PPM),
                    recall_ppm: EvidenceValue::observed(MIN_SEMANTIC_RECALL_PPM - 1),
                    expected_calibration_error_ppm: EvidenceValue::observed(
                        MAX_SEMANTIC_CALIBRATION_ERROR_PPM,
                    ),
                    unsupported_cases: BTreeMap::new(),
                },
                Availability::Failed {
                    reason_code: "semantic_recall_below_threshold".to_owned(),
                },
            ),
            (
                SemanticQualityMeasurement {
                    precision_ppm: EvidenceValue::observed(MIN_SEMANTIC_PRECISION_PPM),
                    recall_ppm: EvidenceValue::observed(MIN_SEMANTIC_RECALL_PPM),
                    expected_calibration_error_ppm: EvidenceValue::observed(
                        MAX_SEMANTIC_CALIBRATION_ERROR_PPM + 1,
                    ),
                    unsupported_cases: BTreeMap::new(),
                },
                Availability::Failed {
                    reason_code: "semantic_calibration_above_threshold".to_owned(),
                },
            ),
            (
                SemanticQualityMeasurement::unavailable("quality_corpus_not_integrated"),
                Availability::Unavailable {
                    reason_code: "semantic_quality_not_measured".to_owned(),
                },
            ),
        ];

        for (quality, expected) in cases {
            assert_eq!(
                semantic_quality_eligibility(&facts_available, &quality),
                expected
            );
        }
    }

    #[test]
    fn empty_semantic_extraction_is_a_failed_run() {
        let fixture = fixture();
        let provider = provider(&fixture.source);

        let evidence = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 1),
            &UnavailableProcessTreeSampler,
            &FixedSemanticFacts(0),
        )
        .expect("syntax benchmark succeeds");

        assert_eq!(
            evidence.summary.semantic_eligibility,
            Availability::Failed {
                reason_code: "semantic_facts_empty".to_owned()
            }
        );
    }

    #[test]
    fn sampler_scope_encloses_every_parse_call() {
        let fixture = fixture();
        let active = Arc::new(AtomicUsize::new(0));
        let begins = Arc::new(AtomicUsize::new(0));
        let finishes = Arc::new(AtomicUsize::new(0));
        let provider = ScopeCheckingProvider {
            inner: provider(&fixture.source),
            active: Arc::clone(&active),
        };
        let sampler = TrackingSampler {
            active: Arc::clone(&active),
            begins: Arc::clone(&begins),
            finishes: Arc::clone(&finishes),
        };

        run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(1, 2),
            &sampler,
            &FixedSemanticFacts(1),
        )
        .expect("scoped benchmark succeeds");

        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(begins.load(Ordering::SeqCst), 3);
        assert_eq!(finishes.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn entry_failure_is_sticky_across_later_success() {
        let fixture = fixture();
        let provider = FailThenSucceedProvider {
            inner: provider(&fixture.source),
            calls: AtomicUsize::new(0),
        };

        let evidence = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 2),
            &FakeSampler,
            &FixedSemanticFacts(1),
        )
        .expect("failed and successful samples are retained");

        assert_eq!(evidence.summary.failed_samples, 1);
        assert_eq!(evidence.coverage.attempted_entries, 1);
        assert_eq!(evidence.coverage.committed_entries, 0);
        assert_eq!(
            evidence.coverage.parser_status.get("rust-main"),
            Some(&"failed".to_owned())
        );
    }

    #[test]
    fn cancellation_is_retained_and_keeps_entry_uncommitted() {
        let fixture = fixture();
        let provider = provider(&fixture.source)
            .with_cancellation_after_batches(0, CancellationReason::ClientRequest);

        let evidence = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 1),
            &UnavailableProcessTreeSampler,
            &FixedSemanticFacts(1),
        )
        .expect("cancelled sample remains valid evidence");

        assert_eq!(evidence.summary.cancelled_samples, 1);
        assert_eq!(evidence.coverage.attempted_entries, 1);
        assert_eq!(evidence.coverage.committed_entries, 0);
        assert!(matches!(
            evidence.raw_samples[0].outcome,
            SampleOutcome::Cancelled
        ));
    }

    #[test]
    fn size_preflight_precedes_digest_and_content_scans() {
        let mut fixture = fixture();
        fixture.input.entry.source_sha256 = "AB".repeat(32);
        let provider = provider(&fixture.source);
        let mut limited = config(0, 1);
        limited.evidence_limits.max_snapshot_bytes = 1;

        let error = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &limited,
            &UnavailableProcessTreeSampler,
            &UnavailableSemanticFacts,
        )
        .expect_err("oversized input is rejected before digest validation");
        assert!(
            matches!(error, ParserRunError::SnapshotTooLarge),
            "unexpected preflight error: {error:?}"
        );

        let error = run_parser_benchmark(
            &provider,
            std::slice::from_ref(&fixture.input),
            &config(0, 1),
            &UnavailableProcessTreeSampler,
            &UnavailableSemanticFacts,
        )
        .expect_err("uppercase typed digest is rejected");
        assert!(matches!(error, ParserRunError::InvalidSourceDigest));
    }

    #[test]
    fn dataset_schedule_range_and_timeout_limits_are_preflighted() {
        let fixture = fixture();
        let provider = provider(&fixture.source);

        let mut total_limited = config(0, 1);
        total_limited.evidence_limits.max_dataset_source_bytes = 1;
        assert!(matches!(
            run_parser_benchmark(
                &provider,
                std::slice::from_ref(&fixture.input),
                &total_limited,
                &UnavailableProcessTreeSampler,
                &UnavailableSemanticFacts,
            ),
            Err(ParserRunError::DatasetTooLarge)
        ));

        let mut second_input = fixture.input.clone();
        second_input.entry.id = "rust-main-2".to_owned();
        let mut entry_limited = config(0, 1);
        entry_limited.evidence_limits.max_manifest_entries = 1;
        assert!(matches!(
            run_parser_benchmark(
                &provider,
                &[fixture.input.clone(), second_input],
                &entry_limited,
                &UnavailableProcessTreeSampler,
                &UnavailableSemanticFacts,
            ),
            Err(ParserRunError::TooManyDatasetEntries)
        ));

        let mut sample_limited = config(0, 2);
        sample_limited.evidence_limits.max_raw_samples = 1;
        assert!(matches!(
            run_parser_benchmark(
                &provider,
                std::slice::from_ref(&fixture.input),
                &sample_limited,
                &UnavailableProcessTreeSampler,
                &UnavailableSemanticFacts,
            ),
            Err(ParserRunError::TooManySamples)
        ));

        let mut timeout_limited = config(0, 1);
        timeout_limited.timeout = MAX_SAMPLE_TIMEOUT + Duration::from_nanos(1);
        assert!(matches!(
            run_parser_benchmark(
                &provider,
                std::slice::from_ref(&fixture.input),
                &timeout_limited,
                &UnavailableProcessTreeSampler,
                &UnavailableSemanticFacts,
            ),
            Err(ParserRunError::TimeoutTooLarge)
        ));

        let mut ranged_input = fixture.input.clone();
        let range = IncludedRange::new(
            SourceSpan::new(
                ranged_input.source.span().file(),
                0,
                ranged_input.source.span().end_byte(),
            )
            .expect("included range span is valid"),
            LanguageId::new("rust").expect("range language is valid"),
        );
        ranged_input.included_ranges = vec![range; 9];
        assert!(matches!(
            run_parser_benchmark(
                &provider,
                std::slice::from_ref(&ranged_input),
                &config(0, 1),
                &UnavailableProcessTreeSampler,
                &UnavailableSemanticFacts,
            ),
            Err(ParserRunError::TooManyIncludedRanges)
        ));
    }

    struct Fixture {
        _temporary: tempfile::TempDir,
        source: SourceRef,
        input: ParserDatasetInput,
    }

    fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().expect("temporary root is available");
        // macOS exposes its default temporary directory through the `/var`
        // alias. Resolve it before exercising VFS, whose no-follow boundary
        // intentionally rejects aliased repository roots.
        let root = fs::canonicalize(temporary.path()).expect("temporary root canonicalizes");
        fs::write(root.join("lib.rs"), b"fn main() {}\n").expect("fixture source is written");
        let repository_id = RepositoryId::from_bytes([7; 16]);
        let repository = RepositoryRoot::open(repository_id, &root).expect("fixture root opens");
        let relative = RelativePath::parse(Path::new("lib.rs")).expect("relative path is valid");
        let snapshot = repository
            .snapshot(&relative, 4096)
            .expect("snapshot is stable");
        let end = u64::try_from(snapshot.content().len()).expect("fixture length fits");
        let source = SourceRef::new(
            repository_id,
            GenerationId::from_bytes([8; 20]),
            SourceSpan::new(snapshot.file(), 0, end).expect("source span is valid"),
            snapshot.content_hash(),
            None,
        );
        let entry = DatasetEntry {
            id: "rust-main".to_owned(),
            grammar_family: "rust".to_owned(),
            language: "rust".to_owned(),
            relative_path: "lib.rs".to_owned(),
            source_sha256: sha256_hex(snapshot.content()),
            source_bytes: end,
            physical_lines: 1,
            generated: false,
        };
        let input = ParserDatasetInput {
            entry,
            snapshot,
            source: source.clone(),
            included_ranges: Vec::new(),
        };
        Fixture {
            _temporary: temporary,
            source,
            input,
        }
    }

    fn provider(source: &SourceRef) -> MockParseProvider {
        let capabilities = ParseCapabilities::new(
            vec![LanguageId::new("rust").expect("language is valid")],
            vec![EncodingId::new("utf-8").expect("encoding is valid")],
            4096,
            4096,
            32,
            8,
            true,
            true,
            true,
            1,
            MemoryEnforcement::AccountedInProcess,
        )
        .expect("capabilities are valid");
        let fact = SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Declaration,
            SourceSpan::new(source.span().file(), 0, 1).expect("fact span is valid"),
            1,
            SyntaxKindLabel::new("function_item").expect("syntax label is valid"),
        );
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Complete,
            usize::try_from(source.span().end_byte()).expect("fixture length fits"),
            usize::try_from(source.span().end_byte()).expect("fixture length fits"),
            0,
            Vec::new(),
        )
        .expect("coverage is valid");
        MockParseProvider::new(capabilities, vec![fact], Vec::new(), coverage)
            .with_reported_memory_bytes(1)
    }

    fn config(warmup_rounds: u32, trial_rounds: u32) -> ParserBenchmarkConfig {
        let batch = BatchThresholds::new(16, 32 * 1024, 8, 4096).expect("batch limits are valid");
        let stream = StreamLimits::new(16, 128, 1024 * 1024, 32, 32 * 1024, 32 * 1024, batch)
            .expect("stream limits are valid");
        let limits = AnalysisLimits::new(
            4096,
            4096,
            32,
            8,
            1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .expect("analysis limits are valid");
        ParserBenchmarkConfig {
            seed: 42,
            warmup_rounds,
            trial_rounds,
            timeout: Duration::from_secs(1),
            limits,
            memory_policy: MemoryAdmissionPolicy::RequireHardOrAccounted,
            evidence_limits: BundleLimits::default(),
        }
    }
}
