//! Source-free performance evidence for public MCP tools and cancellable work.
//!
//! This module keeps preregistered targets separate from observations, retains
//! every terminal sample, and derives percentile claims only from reconciled
//! distributions with enough successful measurements.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{EvidenceValue, token_accounting::sha256_hex};

/// Performance evidence schema emitted by this module.
pub const PERFORMANCE_EVIDENCE_SCHEMA_VERSION: &str = "1.0";
/// Minimum successful primary-state samples required for a p99 claim.
pub const MIN_PRIMARY_SUCCESS_SAMPLES: u64 = 100;
/// Maximum retained samples accepted by one evidence package.
pub const MAX_PERFORMANCE_SAMPLES: usize = 1_000_000;
/// Maximum bytes accepted for a canonical performance evidence package.
pub const MAX_PERFORMANCE_EVIDENCE_BYTES: usize = 256 * 1024 * 1024;
/// Exact public MCP tool inventory covered by the performance protocol.
pub const PUBLIC_MCP_TOOLS: [&str; 19] = [
    "repo.index",
    "repo.status",
    "repo.list",
    "operation.status",
    "code.locate",
    "symbol.explain",
    "symbol.relationships",
    "flow.trace",
    "change.impact",
    "tests.select",
    "architecture.overview",
    "architecture.cycles",
    "code.dead",
    "history.compare",
    "plan.change",
    "context.pack",
    "source.read",
    "query.advanced",
    "query.batch",
];

const MAX_IDENTIFIER_BYTES: usize = 128;

/// Whether a sample uses a newly started or retained MCP process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// The sample includes a newly initialized MCP process.
    Cold,
    /// The sample reuses an initialized MCP process.
    Warm,
}

/// Immutable fixture size class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureScale {
    /// The preregistered small fixture.
    Small,
    /// The preregistered large fixture.
    Large,
}

/// Expected successful-result completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultCompleteness {
    /// The request is expected to complete its declared domain.
    Complete,
    /// The request intentionally exercises a declared resource limit.
    Truncated,
}

/// Cache state retained independently from process and fixture state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// No logical result cache is expected to be warm.
    Cold,
    /// Repeated requests may use a verified logical cache.
    Warm,
    /// The tool contract has no meaningful logical cache state.
    NotApplicable,
}

/// One non-mixable performance distribution key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceCondition {
    /// Stable condition identifier.
    pub condition_id: String,
    /// MCP process lifecycle state.
    pub process_state: ProcessState,
    /// Fixture size class.
    pub fixture_scale: FixtureScale,
    /// Expected completeness class.
    pub completeness: ResultCompleteness,
    /// Logical cache state.
    pub cache_state: CacheState,
    /// Concurrency used by this condition.
    pub concurrency: u32,
}

/// Terminal disposition for a retained performance sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PerformanceSampleOutcome {
    /// The public tool returned a schema-valid successful result.
    Succeeded,
    /// The public tool returned a stable source-free failure code.
    Failed {
        /// Stable failure code.
        error_code: String,
    },
    /// The monotonic sample deadline elapsed.
    TimedOut,
    /// The request was cancelled.
    Cancelled,
    /// The preregistered exclusion rule matched.
    Excluded {
        /// Stable exclusion reason.
        reason_code: String,
    },
}

/// Result dimensions retained for each sample where the metric applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceDimensions {
    /// Storage or execution rows examined.
    pub rows: EvidenceValue<u64>,
    /// Relationship edges examined.
    pub edges: EvidenceValue<u64>,
    /// Maximum traversal depth reached.
    pub traversal_depth: EvidenceValue<u64>,
    /// Result items returned.
    pub result_items: EvidenceValue<u64>,
    /// Source bytes inspected or returned.
    pub source_bytes: EvidenceValue<u64>,
    /// Exact serialized JSON response bytes.
    pub response_json_bytes: EvidenceValue<u64>,
    /// Deterministic server estimate retained for comparison.
    pub estimated_tokens: EvidenceValue<u64>,
    /// Tokens counted by the exact tokenizer declared in the manifest.
    pub actual_tokens: EvidenceValue<u64>,
    /// Public tool calls represented by this sample.
    pub calls: u64,
}

/// One retained raw public-tool performance sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceRawSample {
    /// Sample schema version.
    pub schema_version: String,
    /// Stable public tool identifier.
    pub tool_id: String,
    /// Stable condition identifier.
    pub condition_id: String,
    /// Zero-based ordinal within the tool-condition distribution.
    pub ordinal: u64,
    /// Whether this is a warm-up or measured sample.
    pub phase: SamplePhase,
    /// Monotonic end-to-end elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Process-tree CPU nanoseconds under the declared sampling method.
    pub process_tree_cpu_ns: EvidenceValue<u64>,
    /// Process-tree peak resident bytes under the declared sampling method.
    pub process_tree_peak_rss_bytes: EvidenceValue<u64>,
    /// Tool-specific counters and exact wire sizes.
    pub dimensions: PerformanceDimensions,
    /// Terminal disposition retained without outlier deletion.
    pub outcome: PerformanceSampleOutcome,
}

/// Whether a retained sample contributes to a measured distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplePhase {
    /// Calibration or cache warm-up that does not contribute to claims.
    Warmup,
    /// A preregistered measured attempt.
    Measured,
}

/// Exact resource-accounting method and its precision limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceMeasurementMethod {
    /// Stable method identifier.
    pub method_id: String,
    /// Stable operating-system family.
    pub platform: String,
    /// Polling interval in microseconds, or unavailable with a reason.
    pub polling_interval_us: EvidenceValue<u64>,
    /// CPU clock resolution in nanoseconds, or unavailable with a reason.
    pub cpu_resolution_ns: EvidenceValue<u64>,
    /// RSS resolution in bytes, or unavailable with a reason.
    pub rss_resolution_bytes: EvidenceValue<u64>,
    /// Whether descendants are included.
    pub process_tree_included: bool,
    /// Stable caveat codes in canonical order.
    pub caveat_codes: Vec<String>,
}

/// Exact build, environment, fixture, tokenizer, and binary identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceEnvironmentManifest {
    /// Manifest schema version.
    pub schema_version: String,
    /// Exact Git revision.
    pub source_revision: String,
    /// SHA-256 of the exact preregistered protocol.
    pub protocol_sha256: String,
    /// Exact `rustc -Vv` digest.
    pub rustc_verbose_sha256: String,
    /// Exact Cargo metadata or lock digest.
    pub dependency_graph_sha256: String,
    /// Rust target triple.
    pub target_triple: String,
    /// Operating-system identity.
    pub operating_system: String,
    /// CPU architecture.
    pub architecture: String,
    /// Source-free CPU model identifier.
    pub cpu_model: String,
    /// Physical or configured logical CPU count.
    pub cpu_count: u32,
    /// Installed or constrained memory bytes.
    pub memory_bytes: u64,
    /// Cargo build profile.
    pub build_profile: String,
    /// Enabled Cargo features in canonical order.
    pub features: Vec<String>,
    /// Exact benchmarked binary digests by stable binary ID.
    pub binary_sha256: BTreeMap<String, String>,
    /// Exact fixture digests by stable fixture ID.
    pub fixture_sha256: BTreeMap<String, String>,
    /// Exact tokenizer identity.
    pub tokenizer_id: String,
    /// Exact tokenizer vocabulary or configuration digest.
    pub tokenizer_sha256: String,
    /// Monotonic clock identity.
    pub monotonic_clock: String,
    /// Background-process policy.
    pub background_process_policy: String,
    /// Process-tree measurement contract.
    pub resource_method: ResourceMeasurementMethod,
}

/// Preregistered rule for handling unavailable measurements at a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailablePolicy {
    /// Preserve the result as a documented fallback.
    Fallback,
    /// Block the gate because the measurement is mandatory.
    Block,
}

/// Threshold provenance; targets never share an observation field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdClass {
    /// Normative gate from the accepted performance protocol.
    Gate,
    /// Informational target that cannot block this evidence package.
    Aspirational,
    /// Relative comparison against an accepted prior baseline.
    Regression,
}

/// Metric selected by a preregistered upper threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdMetric {
    /// Successful-sample p50 wall latency.
    WallLatencyP50Ns,
    /// Successful-sample p95 wall latency.
    WallLatencyP95Ns,
    /// Successful-sample p99 wall latency.
    WallLatencyP99Ns,
    /// Failed and timed-out attempts per million measured attempts.
    ReliabilityFailureRatePpm,
    /// Successful-sample peak RSS p99.
    PeakRssP99Bytes,
    /// Cancellation-observation p99.
    CancellationLatencyP99Ns,
}

/// One fixed threshold registered before samples are collected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceThreshold {
    /// Stable threshold identifier.
    pub threshold_id: String,
    /// Target tool or cancellation class.
    pub subject_id: String,
    /// Condition identifier, or `all` for a subject-wide threshold.
    pub condition_id: String,
    /// Threshold provenance.
    pub class: ThresholdClass,
    /// Selected metric.
    pub metric: ThresholdMetric,
    /// Inclusive upper bound in the metric's declared unit.
    pub upper_bound: u64,
    /// Result when the selected metric is unavailable.
    pub unavailable_policy: UnavailablePolicy,
}

/// Primary-state measurement requirement for one public tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolMeasurementPlan {
    /// The tool requires a measured primary distribution.
    Required {
        /// Stable public tool identifier.
        tool_id: String,
        /// Primary condition identifier.
        primary_condition_id: String,
        /// Minimum successful samples for percentile claims.
        minimum_success_samples: u64,
    },
    /// A reviewed protocol says the measurement does not apply.
    NotApplicable {
        /// Stable public tool identifier.
        tool_id: String,
        /// Stable reviewed reason.
        reason_code: String,
        /// Accountable reviewer role, not a person's name.
        reviewer_role: String,
    },
}

impl ToolMeasurementPlan {
    fn tool_id(&self) -> &str {
        match self {
            Self::Required { tool_id, .. } | Self::NotApplicable { tool_id, .. } => tool_id,
        }
    }
}

/// A cancellable long-running class declared by the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum CancellationClassPlan {
    /// The class exposes a deterministic admission hook and must be measured.
    Required {
        /// Stable class identifier.
        class_id: String,
        /// Public tool implementing the class.
        tool_id: String,
        /// Minimum successful cancellation observations.
        minimum_success_samples: u64,
        /// Maximum bounded work allowed after cancellation is observed.
        maximum_post_cancel_work_ns: u64,
    },
    /// The class has no supported public cancellation boundary.
    NotApplicable {
        /// Stable class identifier.
        class_id: String,
        /// Stable reviewed reason.
        reason_code: String,
        /// Accountable reviewer role.
        reviewer_role: String,
    },
}

impl CancellationClassPlan {
    fn class_id(&self) -> &str {
        match self {
            Self::Required { class_id, .. } | Self::NotApplicable { class_id, .. } => class_id,
        }
    }
}

/// Fixed protocol applied to one performance evidence package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceProtocol {
    /// Protocol schema version.
    pub schema_version: String,
    /// Stable protocol identifier.
    pub protocol_id: String,
    /// Warm-up samples per measured distribution.
    pub warmup_samples: u64,
    /// Monotonic timeout for each measured attempt.
    pub timeout_ms: u64,
    /// Global sample concurrency.
    pub concurrency: u32,
    /// Exact process and fixture conditions.
    pub conditions: Vec<PerformanceCondition>,
    /// Exact public-tool measurement plans.
    pub tools: Vec<ToolMeasurementPlan>,
    /// Cancellable long-running class plans.
    pub cancellation_classes: Vec<CancellationClassPlan>,
    /// Fixed targets and regression bounds.
    pub thresholds: Vec<PerformanceThreshold>,
    /// Stable exclusion rules registered before the run.
    pub exclusion_reason_codes: Vec<String>,
}

/// One raw cancellation observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationRawSample {
    /// Sample schema version.
    pub schema_version: String,
    /// Stable cancellable class identifier.
    pub class_id: String,
    /// Public tool identifier.
    pub tool_id: String,
    /// Zero-based ordinal within the class.
    pub ordinal: u64,
    /// Notification-to-hook-observation latency in nanoseconds.
    pub cancellation_latency_ns: u64,
    /// Bounded work observed after cancellation.
    pub post_cancel_work_ns: u64,
    /// Terminal cancellation outcome.
    pub outcome: PerformanceSampleOutcome,
}

/// Nearest-rank distribution over retained observed values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedDistribution {
    /// Number of values in the distribution.
    pub sample_count: u64,
    /// Minimum observed value.
    pub minimum: u64,
    /// Nearest-rank median.
    pub p50: u64,
    /// Nearest-rank 95th percentile.
    pub p95: u64,
    /// Nearest-rank 99th percentile.
    pub p99: u64,
    /// Maximum observed value.
    pub maximum: u64,
}

/// Reconciled counters for one measured condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleReconciliation {
    /// All measured attempts retained for the condition.
    pub attempted: u64,
    /// Successful measured attempts.
    pub succeeded: u64,
    /// Failed measured attempts.
    pub failed: u64,
    /// Timed-out measured attempts.
    pub timed_out: u64,
    /// Cancelled measured attempts.
    pub cancelled: u64,
    /// Preregistered exclusions.
    pub excluded: u64,
}

/// Aggregate observations for one tool-condition distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceAggregate {
    /// Stable public tool identifier.
    pub tool_id: String,
    /// Stable condition identifier.
    pub condition_id: String,
    /// Reconciled terminal counts.
    pub reconciliation: SampleReconciliation,
    /// Successful-sample wall latency.
    pub wall_latency_ns: EvidenceValue<ObservedDistribution>,
    /// Successful-sample process-tree CPU.
    pub process_tree_cpu_ns: EvidenceValue<ObservedDistribution>,
    /// Successful-sample process-tree peak RSS.
    pub process_tree_peak_rss_bytes: EvidenceValue<ObservedDistribution>,
    /// Exact response JSON bytes.
    pub response_json_bytes: EvidenceValue<ObservedDistribution>,
    /// Actual tokenizer counts.
    pub actual_tokens: EvidenceValue<ObservedDistribution>,
    /// Storage or execution rows, where applicable.
    pub rows: EvidenceValue<ObservedDistribution>,
    /// Relationship edges, where applicable.
    pub edges: EvidenceValue<ObservedDistribution>,
    /// Traversal depth, where applicable.
    pub traversal_depth: EvidenceValue<ObservedDistribution>,
    /// Result items, where applicable.
    pub result_items: EvidenceValue<ObservedDistribution>,
    /// Source bytes, where applicable.
    pub source_bytes: EvidenceValue<ObservedDistribution>,
    /// Calls represented by each sample.
    pub calls: EvidenceValue<ObservedDistribution>,
    /// Failed and timed-out attempts per million non-excluded attempts.
    pub reliability_failure_rate_ppm: EvidenceValue<u64>,
}

/// Aggregate observations for one cancellable class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancellationAggregate {
    /// Stable cancellable class identifier.
    pub class_id: String,
    /// Public tool identifier.
    pub tool_id: String,
    /// Reconciled terminal counts.
    pub reconciliation: SampleReconciliation,
    /// Notification-to-observation latency.
    pub cancellation_latency_ns: EvidenceValue<ObservedDistribution>,
    /// Post-cancel work.
    pub post_cancel_work_ns: EvidenceValue<ObservedDistribution>,
}

/// Final threshold disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDisposition {
    /// Every applicable mandatory threshold passed.
    Pass,
    /// Mandatory evidence is unavailable under a preregistered fallback rule.
    Fallback,
    /// A mandatory threshold or evidence invariant failed.
    Blocked,
}

/// One evaluated target with observed and target values kept distinct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdEvaluation {
    /// Stable threshold identifier.
    pub threshold_id: String,
    /// Threshold provenance.
    pub class: ThresholdClass,
    /// Inclusive target bound.
    pub target_upper_bound: u64,
    /// Observed metric, never a target placeholder.
    pub observed: EvidenceValue<u64>,
    /// Threshold result.
    pub disposition: GateDisposition,
}

/// Optional comparison against one accepted prior package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionComparison {
    /// Accepted prior evidence digest.
    pub baseline_sha256: String,
    /// Whether binary, fixture, and environment identities are comparable.
    pub comparable_environment: bool,
    /// Stable caveat when exact comparability is unavailable.
    pub caveat_code: String,
    /// Per-threshold signed change in parts per million.
    pub metric_delta_ppm: BTreeMap<String, i64>,
}

/// Complete source-free performance evidence package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerformanceEvidencePackage {
    /// Evidence schema version.
    pub schema_version: String,
    /// Fixed protocol.
    pub protocol: PerformanceProtocol,
    /// Exact environment and input identities.
    pub environment: PerformanceEnvironmentManifest,
    /// Every retained public-tool sample.
    pub raw_samples: Vec<PerformanceRawSample>,
    /// Every retained cancellation observation.
    pub cancellation_samples: Vec<CancellationRawSample>,
    /// Canonically ordered tool-condition aggregates.
    pub aggregates: Vec<PerformanceAggregate>,
    /// Canonically ordered cancellation aggregates.
    pub cancellation_aggregates: Vec<CancellationAggregate>,
    /// Canonically ordered target evaluations.
    pub threshold_evaluations: Vec<ThresholdEvaluation>,
    /// Overall performance evaluation result.
    pub disposition: GateDisposition,
    /// Optional prior-baseline comparison.
    pub regression: Option<RegressionComparison>,
    /// Stable residual limitation codes.
    pub residual_limitations: Vec<String>,
}

/// Performance evidence construction or validation failure.
#[derive(Debug, Error)]
pub enum PerformanceEvidenceError {
    /// A schema version is unsupported.
    #[error("unsupported performance evidence schema")]
    UnsupportedSchema,
    /// A stable identifier, reason, or platform label is invalid.
    #[error("performance evidence contains an invalid normalized identifier")]
    InvalidIdentifier,
    /// A required SHA-256 digest is malformed.
    #[error("performance evidence contains an invalid sha256 digest")]
    InvalidDigest,
    /// The environment manifest is bound to a different protocol.
    #[error("performance environment protocol digest does not match")]
    ProtocolDigestMismatch,
    /// The protocol does not cover the exact public tool inventory.
    #[error("performance protocol does not cover the exact public tool inventory")]
    ToolInventory,
    /// The protocol contains an invalid or duplicate condition.
    #[error("performance protocol contains an invalid condition")]
    Condition,
    /// Raw sample identity, ordering, or counters are inconsistent.
    #[error("performance raw samples do not reconcile")]
    SampleReconciliation,
    /// A primary distribution lacks the preregistered successful denominator.
    #[error("performance primary distribution has insufficient successful samples")]
    InsufficientPrimarySamples,
    /// Cancellation evidence is missing or violates post-cancel bounds.
    #[error("cancellation evidence does not satisfy its preregistered plan")]
    CancellationEvidence,
    /// Observed fields contain target values.
    #[error("performance observations cannot contain target values")]
    TargetInObservation,
    /// A threshold references an unknown subject, condition, or metric.
    #[error("performance threshold is not applicable to its declared subject")]
    InvalidThreshold,
    /// Canonical JSON exceeds the evidence boundary.
    #[error("performance evidence exceeds its encoded byte limit")]
    EncodedTooLarge,
    /// JSON encoding or decoding failed.
    #[error("performance evidence json failed")]
    Json(#[from] serde_json::Error),
}

/// Builds reconciled aggregates and target evaluations from retained samples.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when the protocol, environment,
/// samples, cancellation evidence, or thresholds are invalid.
pub fn build_performance_evidence(
    protocol: PerformanceProtocol,
    environment: PerformanceEnvironmentManifest,
    raw_samples: Vec<PerformanceRawSample>,
    cancellation_samples: Vec<CancellationRawSample>,
    regression: Option<RegressionComparison>,
    residual_limitations: Vec<String>,
) -> Result<PerformanceEvidencePackage, PerformanceEvidenceError> {
    validate_protocol(&protocol)?;
    validate_environment(&environment)?;
    if environment.protocol_sha256 != performance_protocol_sha256(&protocol)? {
        return Err(PerformanceEvidenceError::ProtocolDigestMismatch);
    }
    if raw_samples.len().saturating_add(cancellation_samples.len()) > MAX_PERFORMANCE_SAMPLES {
        return Err(PerformanceEvidenceError::EncodedTooLarge);
    }
    validate_reason_codes(&residual_limitations)?;
    if let Some(comparison) = &regression {
        validate_regression(comparison)?;
    }

    let aggregates = aggregate_tool_samples(&protocol, &raw_samples)?;
    let cancellation_aggregates = aggregate_cancellation_samples(&protocol, &cancellation_samples)?;
    let threshold_evaluations =
        evaluate_thresholds(&protocol, &aggregates, &cancellation_aggregates)?;
    let disposition = overall_disposition(&threshold_evaluations);
    let package = PerformanceEvidencePackage {
        schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
        protocol,
        environment,
        raw_samples,
        cancellation_samples,
        aggregates,
        cancellation_aggregates,
        threshold_evaluations,
        disposition,
        regression,
        residual_limitations,
    };
    validate_performance_evidence(&package)?;
    Ok(package)
}

/// Validates a complete performance package and recomputes all aggregates.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when any identity, count, percentile,
/// threshold result, or privacy boundary differs from the canonical result.
pub fn validate_performance_evidence(
    package: &PerformanceEvidencePackage,
) -> Result<(), PerformanceEvidenceError> {
    if package.schema_version != PERFORMANCE_EVIDENCE_SCHEMA_VERSION {
        return Err(PerformanceEvidenceError::UnsupportedSchema);
    }
    validate_protocol(&package.protocol)?;
    validate_environment(&package.environment)?;
    if package.environment.protocol_sha256 != performance_protocol_sha256(&package.protocol)? {
        return Err(PerformanceEvidenceError::ProtocolDigestMismatch);
    }
    validate_reason_codes(&package.residual_limitations)?;
    if let Some(comparison) = &package.regression {
        validate_regression(comparison)?;
    }
    let aggregates = aggregate_tool_samples(&package.protocol, &package.raw_samples)?;
    let cancellation_aggregates =
        aggregate_cancellation_samples(&package.protocol, &package.cancellation_samples)?;
    let evaluations =
        evaluate_thresholds(&package.protocol, &aggregates, &cancellation_aggregates)?;
    if package.aggregates != aggregates
        || package.cancellation_aggregates != cancellation_aggregates
        || package.threshold_evaluations != evaluations
        || package.disposition != overall_disposition(&evaluations)
    {
        return Err(PerformanceEvidenceError::SampleReconciliation);
    }
    Ok(())
}

/// Encodes a validated package as canonical compact JSON.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when validation, serialization, or the
/// encoded-size boundary fails.
pub fn encode_performance_evidence(
    package: &PerformanceEvidencePackage,
) -> Result<Vec<u8>, PerformanceEvidenceError> {
    validate_performance_evidence(package)?;
    let bytes = serde_json::to_vec(package)?;
    if bytes.len() > MAX_PERFORMANCE_EVIDENCE_BYTES {
        return Err(PerformanceEvidenceError::EncodedTooLarge);
    }
    Ok(bytes)
}

/// Decodes and validates canonical performance evidence.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when the input is oversized, malformed,
/// or inconsistent.
pub fn decode_performance_evidence(
    bytes: &[u8],
) -> Result<PerformanceEvidencePackage, PerformanceEvidenceError> {
    if bytes.len() > MAX_PERFORMANCE_EVIDENCE_BYTES {
        return Err(PerformanceEvidenceError::EncodedTooLarge);
    }
    let package: PerformanceEvidencePackage = serde_json::from_slice(bytes)?;
    validate_performance_evidence(&package)?;
    Ok(package)
}

/// Calculates a nearest-rank distribution without interpolation.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError::SampleReconciliation`] for an empty
/// input because an empty set has no observed percentiles.
pub fn nearest_rank_distribution(
    values: &[u64],
) -> Result<ObservedDistribution, PerformanceEvidenceError> {
    if values.is_empty() {
        return Err(PerformanceEvidenceError::SampleReconciliation);
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let sample_count =
        u64::try_from(sorted.len()).map_err(|_| PerformanceEvidenceError::SampleReconciliation)?;
    let minimum = *sorted
        .first()
        .ok_or(PerformanceEvidenceError::SampleReconciliation)?;
    let maximum = *sorted
        .last()
        .ok_or(PerformanceEvidenceError::SampleReconciliation)?;
    Ok(ObservedDistribution {
        sample_count,
        minimum,
        p50: nearest_rank(&sorted, 50)?,
        p95: nearest_rank(&sorted, 95)?,
        p99: nearest_rank(&sorted, 99)?,
        maximum,
    })
}

/// Returns the lowercase SHA-256 of canonical performance evidence bytes.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when the package cannot be encoded.
pub fn performance_evidence_sha256(
    package: &PerformanceEvidencePackage,
) -> Result<String, PerformanceEvidenceError> {
    let bytes = encode_performance_evidence(package)?;
    Ok(sha256_hex(&bytes))
}

/// Returns the lowercase SHA-256 of the validated preregistered protocol.
///
/// # Errors
///
/// Returns [`PerformanceEvidenceError`] when the protocol is invalid or cannot
/// be serialized.
pub fn performance_protocol_sha256(
    protocol: &PerformanceProtocol,
) -> Result<String, PerformanceEvidenceError> {
    validate_protocol(protocol)?;
    Ok(sha256_hex(&serde_json::to_vec(protocol)?))
}

fn validate_protocol(protocol: &PerformanceProtocol) -> Result<(), PerformanceEvidenceError> {
    if protocol.schema_version != PERFORMANCE_EVIDENCE_SCHEMA_VERSION
        || !valid_identifier(&protocol.protocol_id)
        || protocol.timeout_ms == 0
        || protocol.concurrency == 0
        || protocol.conditions.is_empty()
    {
        return Err(PerformanceEvidenceError::Condition);
    }
    let mut condition_ids = BTreeSet::new();
    for condition in &protocol.conditions {
        if !valid_identifier(&condition.condition_id)
            || condition.concurrency == 0
            || !condition_ids.insert(condition.condition_id.as_str())
        {
            return Err(PerformanceEvidenceError::Condition);
        }
    }

    let mut tools = BTreeSet::new();
    for plan in &protocol.tools {
        let tool = plan.tool_id();
        if !valid_identifier(tool) || !PUBLIC_MCP_TOOLS.contains(&tool) || !tools.insert(tool) {
            return Err(PerformanceEvidenceError::ToolInventory);
        }
        match plan {
            ToolMeasurementPlan::Required {
                primary_condition_id,
                minimum_success_samples,
                ..
            } => {
                if !condition_ids.contains(primary_condition_id.as_str())
                    || *minimum_success_samples < MIN_PRIMARY_SUCCESS_SAMPLES
                {
                    return Err(PerformanceEvidenceError::InsufficientPrimarySamples);
                }
            }
            ToolMeasurementPlan::NotApplicable {
                reason_code,
                reviewer_role,
                ..
            } => {
                if !valid_identifier(reason_code) || !valid_identifier(reviewer_role) {
                    return Err(PerformanceEvidenceError::InvalidIdentifier);
                }
            }
        }
    }
    if tools != PUBLIC_MCP_TOOLS.into_iter().collect() {
        return Err(PerformanceEvidenceError::ToolInventory);
    }

    let mut cancellation_classes = BTreeSet::new();
    for plan in &protocol.cancellation_classes {
        if !valid_identifier(plan.class_id()) || !cancellation_classes.insert(plan.class_id()) {
            return Err(PerformanceEvidenceError::CancellationEvidence);
        }
        match plan {
            CancellationClassPlan::Required {
                tool_id,
                minimum_success_samples,
                ..
            } => {
                if !PUBLIC_MCP_TOOLS.contains(&tool_id.as_str()) || *minimum_success_samples == 0 {
                    return Err(PerformanceEvidenceError::CancellationEvidence);
                }
            }
            CancellationClassPlan::NotApplicable {
                reason_code,
                reviewer_role,
                ..
            } => {
                if !valid_identifier(reason_code) || !valid_identifier(reviewer_role) {
                    return Err(PerformanceEvidenceError::InvalidIdentifier);
                }
            }
        }
    }
    validate_reason_codes(&protocol.exclusion_reason_codes)?;
    validate_threshold_contract(protocol, &condition_ids, &cancellation_classes)
}

fn validate_threshold_contract(
    protocol: &PerformanceProtocol,
    condition_ids: &BTreeSet<&str>,
    cancellation_classes: &BTreeSet<&str>,
) -> Result<(), PerformanceEvidenceError> {
    let mut threshold_ids = BTreeSet::new();
    for threshold in &protocol.thresholds {
        if !valid_identifier(&threshold.threshold_id)
            || !valid_identifier(&threshold.subject_id)
            || !valid_identifier(&threshold.condition_id)
            || !threshold_ids.insert(threshold.threshold_id.as_str())
            || threshold.upper_bound == 0
        {
            return Err(PerformanceEvidenceError::InvalidThreshold);
        }
        let is_cancellation = threshold.metric == ThresholdMetric::CancellationLatencyP99Ns;
        if is_cancellation {
            if !cancellation_classes.contains(threshold.subject_id.as_str())
                || threshold.condition_id != "all"
            {
                return Err(PerformanceEvidenceError::InvalidThreshold);
            }
        } else if !PUBLIC_MCP_TOOLS.contains(&threshold.subject_id.as_str())
            || !condition_ids.contains(threshold.condition_id.as_str())
        {
            return Err(PerformanceEvidenceError::InvalidThreshold);
        }
    }
    Ok(())
}

fn validate_environment(
    environment: &PerformanceEnvironmentManifest,
) -> Result<(), PerformanceEvidenceError> {
    if environment.schema_version != PERFORMANCE_EVIDENCE_SCHEMA_VERSION
        || environment.cpu_count == 0
        || environment.memory_bytes == 0
    {
        return Err(PerformanceEvidenceError::UnsupportedSchema);
    }
    for value in [
        environment.target_triple.as_str(),
        environment.operating_system.as_str(),
        environment.architecture.as_str(),
        environment.cpu_model.as_str(),
        environment.build_profile.as_str(),
        environment.tokenizer_id.as_str(),
        environment.monotonic_clock.as_str(),
        environment.background_process_policy.as_str(),
        environment.resource_method.method_id.as_str(),
        environment.resource_method.platform.as_str(),
    ] {
        if !valid_identifier(value) {
            return Err(PerformanceEvidenceError::InvalidIdentifier);
        }
    }
    validate_source_revision(&environment.source_revision)?;
    for digest in [
        environment.protocol_sha256.as_str(),
        environment.rustc_verbose_sha256.as_str(),
        environment.dependency_graph_sha256.as_str(),
        environment.tokenizer_sha256.as_str(),
    ]
    .into_iter()
    .chain(environment.binary_sha256.values().map(String::as_str))
    .chain(environment.fixture_sha256.values().map(String::as_str))
    {
        validate_digest(digest)?;
    }
    if environment.binary_sha256.is_empty() || environment.fixture_sha256.is_empty() {
        return Err(PerformanceEvidenceError::InvalidDigest);
    }
    validate_map_keys(&environment.binary_sha256)?;
    validate_map_keys(&environment.fixture_sha256)?;
    validate_reason_codes(&environment.features)?;
    validate_reason_codes(&environment.resource_method.caveat_codes)?;
    reject_target(&environment.resource_method.polling_interval_us)?;
    reject_target(&environment.resource_method.cpu_resolution_ns)?;
    reject_target(&environment.resource_method.rss_resolution_bytes)
}

fn validate_regression(comparison: &RegressionComparison) -> Result<(), PerformanceEvidenceError> {
    validate_digest(&comparison.baseline_sha256)?;
    if !valid_identifier(&comparison.caveat_code)
        || comparison
            .metric_delta_ppm
            .keys()
            .any(|key| !valid_identifier(key))
    {
        return Err(PerformanceEvidenceError::InvalidIdentifier);
    }
    Ok(())
}

fn aggregate_tool_samples(
    protocol: &PerformanceProtocol,
    samples: &[PerformanceRawSample],
) -> Result<Vec<PerformanceAggregate>, PerformanceEvidenceError> {
    let conditions = protocol
        .conditions
        .iter()
        .map(|condition| condition.condition_id.as_str())
        .collect::<BTreeSet<_>>();
    let exclusions = protocol
        .exclusion_reason_codes
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut groups: BTreeMap<(&str, &str), Vec<&PerformanceRawSample>> = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for sample in samples {
        validate_tool_sample(sample, &conditions, &exclusions)?;
        if !ordinals.insert((
            sample.tool_id.as_str(),
            sample.condition_id.as_str(),
            sample.phase,
            sample.ordinal,
        )) {
            return Err(PerformanceEvidenceError::SampleReconciliation);
        }
        if sample.phase == SamplePhase::Measured {
            groups
                .entry((sample.tool_id.as_str(), sample.condition_id.as_str()))
                .or_default()
                .push(sample);
        }
    }

    let mut aggregates = Vec::with_capacity(groups.len());
    for ((tool_id, condition_id), group) in groups {
        aggregates.push(aggregate_tool_group(tool_id, condition_id, &group)?);
    }

    for plan in &protocol.tools {
        if let ToolMeasurementPlan::Required {
            tool_id,
            primary_condition_id,
            minimum_success_samples,
        } = plan
        {
            let aggregate = aggregates
                .iter()
                .find(|aggregate| {
                    aggregate.tool_id == *tool_id && aggregate.condition_id == *primary_condition_id
                })
                .ok_or(PerformanceEvidenceError::InsufficientPrimarySamples)?;
            if aggregate.reconciliation.succeeded < *minimum_success_samples {
                return Err(PerformanceEvidenceError::InsufficientPrimarySamples);
            }
        }
    }
    Ok(aggregates)
}

fn validate_tool_sample(
    sample: &PerformanceRawSample,
    conditions: &BTreeSet<&str>,
    exclusions: &BTreeSet<&str>,
) -> Result<(), PerformanceEvidenceError> {
    if sample.schema_version != PERFORMANCE_EVIDENCE_SCHEMA_VERSION
        || !PUBLIC_MCP_TOOLS.contains(&sample.tool_id.as_str())
        || !conditions.contains(sample.condition_id.as_str())
        || sample.elapsed_ns == 0
        || sample.dimensions.calls == 0
    {
        return Err(PerformanceEvidenceError::SampleReconciliation);
    }
    for value in [
        &sample.process_tree_cpu_ns,
        &sample.process_tree_peak_rss_bytes,
        &sample.dimensions.rows,
        &sample.dimensions.edges,
        &sample.dimensions.traversal_depth,
        &sample.dimensions.result_items,
        &sample.dimensions.source_bytes,
        &sample.dimensions.response_json_bytes,
        &sample.dimensions.estimated_tokens,
        &sample.dimensions.actual_tokens,
    ] {
        reject_target(value)?;
    }
    match &sample.outcome {
        PerformanceSampleOutcome::Failed { error_code } => {
            if !valid_identifier(error_code) {
                return Err(PerformanceEvidenceError::InvalidIdentifier);
            }
        }
        PerformanceSampleOutcome::Excluded { reason_code } => {
            if !exclusions.contains(reason_code.as_str()) {
                return Err(PerformanceEvidenceError::SampleReconciliation);
            }
        }
        PerformanceSampleOutcome::Succeeded
        | PerformanceSampleOutcome::TimedOut
        | PerformanceSampleOutcome::Cancelled => {}
    }
    if matches!(sample.outcome, PerformanceSampleOutcome::Succeeded)
        && (!matches!(
            sample.dimensions.response_json_bytes,
            EvidenceValue::Observed { .. }
        ) || !matches!(
            sample.dimensions.actual_tokens,
            EvidenceValue::Observed { .. }
        ))
    {
        return Err(PerformanceEvidenceError::SampleReconciliation);
    }
    Ok(())
}

fn aggregate_tool_group(
    tool_id: &str,
    condition_id: &str,
    group: &[&PerformanceRawSample],
) -> Result<PerformanceAggregate, PerformanceEvidenceError> {
    let reconciliation = reconcile(group.iter().map(|sample| &sample.outcome))?;
    let successful = group
        .iter()
        .copied()
        .filter(|sample| matches!(sample.outcome, PerformanceSampleOutcome::Succeeded))
        .collect::<Vec<_>>();
    let non_excluded = reconciliation
        .attempted
        .saturating_sub(reconciliation.excluded);
    let unsuccessful = reconciliation
        .failed
        .saturating_add(reconciliation.timed_out);
    let reliability_failure_rate_ppm = if non_excluded == 0 {
        EvidenceValue::unavailable("no_non_excluded_attempts")
    } else {
        EvidenceValue::observed(
            unsuccessful
                .saturating_mul(1_000_000)
                .checked_div(non_excluded)
                .ok_or(PerformanceEvidenceError::SampleReconciliation)?,
        )
    };

    Ok(PerformanceAggregate {
        tool_id: tool_id.to_owned(),
        condition_id: condition_id.to_owned(),
        reconciliation,
        wall_latency_ns: observed_distribution(
            successful.iter().map(|sample| Some(sample.elapsed_ns)),
            "no_successful_samples",
        )?,
        process_tree_cpu_ns: evidence_distribution(
            successful.iter().map(|sample| &sample.process_tree_cpu_ns),
            "process_tree_cpu_unavailable",
        )?,
        process_tree_peak_rss_bytes: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.process_tree_peak_rss_bytes),
            "process_tree_rss_unavailable",
        )?,
        response_json_bytes: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.dimensions.response_json_bytes),
            "response_bytes_unavailable",
        )?,
        actual_tokens: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.dimensions.actual_tokens),
            "actual_tokens_unavailable",
        )?,
        rows: evidence_distribution(
            successful.iter().map(|sample| &sample.dimensions.rows),
            "rows_not_applicable",
        )?,
        edges: evidence_distribution(
            successful.iter().map(|sample| &sample.dimensions.edges),
            "edges_not_applicable",
        )?,
        traversal_depth: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.dimensions.traversal_depth),
            "traversal_depth_not_applicable",
        )?,
        result_items: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.dimensions.result_items),
            "result_items_not_applicable",
        )?,
        source_bytes: evidence_distribution(
            successful
                .iter()
                .map(|sample| &sample.dimensions.source_bytes),
            "source_bytes_not_applicable",
        )?,
        calls: observed_distribution(
            successful
                .iter()
                .map(|sample| Some(sample.dimensions.calls)),
            "no_successful_samples",
        )?,
        reliability_failure_rate_ppm,
    })
}

fn aggregate_cancellation_samples(
    protocol: &PerformanceProtocol,
    samples: &[CancellationRawSample],
) -> Result<Vec<CancellationAggregate>, PerformanceEvidenceError> {
    let mut groups: BTreeMap<&str, Vec<&CancellationRawSample>> = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for sample in samples {
        if sample.schema_version != PERFORMANCE_EVIDENCE_SCHEMA_VERSION
            || !valid_identifier(&sample.class_id)
            || !PUBLIC_MCP_TOOLS.contains(&sample.tool_id.as_str())
            || !ordinals.insert((sample.class_id.as_str(), sample.ordinal))
        {
            return Err(PerformanceEvidenceError::CancellationEvidence);
        }
        if let PerformanceSampleOutcome::Failed { error_code }
        | PerformanceSampleOutcome::Excluded {
            reason_code: error_code,
        } = &sample.outcome
            && !valid_identifier(error_code)
        {
            return Err(PerformanceEvidenceError::InvalidIdentifier);
        }
        groups
            .entry(sample.class_id.as_str())
            .or_default()
            .push(sample);
    }

    let mut aggregates = Vec::new();
    for plan in &protocol.cancellation_classes {
        let CancellationClassPlan::Required {
            class_id,
            tool_id,
            minimum_success_samples,
            maximum_post_cancel_work_ns,
        } = plan
        else {
            continue;
        };
        let group = groups
            .get(class_id.as_str())
            .ok_or(PerformanceEvidenceError::CancellationEvidence)?;
        if group.iter().any(|sample| sample.tool_id != *tool_id) {
            return Err(PerformanceEvidenceError::CancellationEvidence);
        }
        let reconciliation = reconcile(group.iter().map(|sample| &sample.outcome))?;
        if reconciliation.succeeded < *minimum_success_samples {
            return Err(PerformanceEvidenceError::CancellationEvidence);
        }
        let successful = group
            .iter()
            .copied()
            .filter(|sample| matches!(sample.outcome, PerformanceSampleOutcome::Succeeded))
            .collect::<Vec<_>>();
        if successful
            .iter()
            .any(|sample| sample.post_cancel_work_ns > *maximum_post_cancel_work_ns)
        {
            return Err(PerformanceEvidenceError::CancellationEvidence);
        }
        aggregates.push(CancellationAggregate {
            class_id: class_id.clone(),
            tool_id: tool_id.clone(),
            reconciliation,
            cancellation_latency_ns: observed_distribution(
                successful
                    .iter()
                    .map(|sample| Some(sample.cancellation_latency_ns)),
                "no_successful_cancellations",
            )?,
            post_cancel_work_ns: observed_distribution(
                successful
                    .iter()
                    .map(|sample| Some(sample.post_cancel_work_ns)),
                "no_successful_cancellations",
            )?,
        });
    }
    if groups.keys().any(|class_id| {
        !protocol
            .cancellation_classes
            .iter()
            .any(|plan| plan.class_id() == *class_id)
    }) {
        return Err(PerformanceEvidenceError::CancellationEvidence);
    }
    Ok(aggregates)
}

fn reconcile<'a>(
    outcomes: impl Iterator<Item = &'a PerformanceSampleOutcome>,
) -> Result<SampleReconciliation, PerformanceEvidenceError> {
    let mut result = SampleReconciliation {
        attempted: 0,
        succeeded: 0,
        failed: 0,
        timed_out: 0,
        cancelled: 0,
        excluded: 0,
    };
    for outcome in outcomes {
        result.attempted = result.attempted.saturating_add(1);
        match outcome {
            PerformanceSampleOutcome::Succeeded => {
                result.succeeded = result.succeeded.saturating_add(1);
            }
            PerformanceSampleOutcome::Failed { .. } => {
                result.failed = result.failed.saturating_add(1);
            }
            PerformanceSampleOutcome::TimedOut => {
                result.timed_out = result.timed_out.saturating_add(1);
            }
            PerformanceSampleOutcome::Cancelled => {
                result.cancelled = result.cancelled.saturating_add(1);
            }
            PerformanceSampleOutcome::Excluded { .. } => {
                result.excluded = result.excluded.saturating_add(1);
            }
        }
    }
    let terminal_total = result
        .succeeded
        .saturating_add(result.failed)
        .saturating_add(result.timed_out)
        .saturating_add(result.cancelled)
        .saturating_add(result.excluded);
    if terminal_total != result.attempted {
        return Err(PerformanceEvidenceError::SampleReconciliation);
    }
    Ok(result)
}

fn observed_distribution(
    values: impl Iterator<Item = Option<u64>>,
    unavailable_reason: &str,
) -> Result<EvidenceValue<ObservedDistribution>, PerformanceEvidenceError> {
    let values = values.flatten().collect::<Vec<_>>();
    if values.is_empty() {
        Ok(EvidenceValue::unavailable(unavailable_reason))
    } else {
        Ok(EvidenceValue::observed(nearest_rank_distribution(&values)?))
    }
}

fn evidence_distribution<'a>(
    values: impl Iterator<Item = &'a EvidenceValue<u64>>,
    unavailable_reason: &str,
) -> Result<EvidenceValue<ObservedDistribution>, PerformanceEvidenceError> {
    let mut observed = Vec::new();
    for value in values {
        match value {
            EvidenceValue::Observed { value } => observed.push(*value),
            EvidenceValue::Target { .. } => {
                return Err(PerformanceEvidenceError::TargetInObservation);
            }
            EvidenceValue::Unavailable { .. } => {}
        }
    }
    if observed.is_empty() {
        Ok(EvidenceValue::unavailable(unavailable_reason))
    } else {
        Ok(EvidenceValue::observed(nearest_rank_distribution(
            &observed,
        )?))
    }
}

fn nearest_rank(sorted: &[u64], percentile: u64) -> Result<u64, PerformanceEvidenceError> {
    let length =
        u64::try_from(sorted.len()).map_err(|_| PerformanceEvidenceError::SampleReconciliation)?;
    let numerator = percentile
        .checked_mul(length)
        .ok_or(PerformanceEvidenceError::SampleReconciliation)?;
    let rank = numerator
        .checked_add(99)
        .ok_or(PerformanceEvidenceError::SampleReconciliation)?
        .checked_div(100)
        .ok_or(PerformanceEvidenceError::SampleReconciliation)?;
    let index: usize = rank
        .saturating_sub(1)
        .try_into()
        .map_err(|_| PerformanceEvidenceError::SampleReconciliation)?;
    sorted
        .get(index)
        .copied()
        .ok_or(PerformanceEvidenceError::SampleReconciliation)
}

fn evaluate_thresholds(
    protocol: &PerformanceProtocol,
    aggregates: &[PerformanceAggregate],
    cancellation: &[CancellationAggregate],
) -> Result<Vec<ThresholdEvaluation>, PerformanceEvidenceError> {
    let mut evaluations = Vec::with_capacity(protocol.thresholds.len());
    for threshold in &protocol.thresholds {
        let observed = if threshold.metric == ThresholdMetric::CancellationLatencyP99Ns {
            cancellation
                .iter()
                .find(|aggregate| aggregate.class_id == threshold.subject_id)
                .map(|aggregate| p99(&aggregate.cancellation_latency_ns))
                .transpose()?
                .unwrap_or_else(|| EvidenceValue::unavailable("cancellation_class_unmeasured"))
        } else {
            let aggregate = aggregates.iter().find(|aggregate| {
                aggregate.tool_id == threshold.subject_id
                    && aggregate.condition_id == threshold.condition_id
            });
            match aggregate {
                Some(aggregate) => threshold_metric(aggregate, threshold.metric)?,
                None => EvidenceValue::unavailable("tool_condition_unmeasured"),
            }
        };
        let disposition = threshold_disposition(threshold, &observed);
        evaluations.push(ThresholdEvaluation {
            threshold_id: threshold.threshold_id.clone(),
            class: threshold.class,
            target_upper_bound: threshold.upper_bound,
            observed,
            disposition,
        });
    }
    Ok(evaluations)
}

fn threshold_metric(
    aggregate: &PerformanceAggregate,
    metric: ThresholdMetric,
) -> Result<EvidenceValue<u64>, PerformanceEvidenceError> {
    match metric {
        ThresholdMetric::WallLatencyP50Ns => percentile(&aggregate.wall_latency_ns, 50),
        ThresholdMetric::WallLatencyP95Ns => percentile(&aggregate.wall_latency_ns, 95),
        ThresholdMetric::WallLatencyP99Ns => percentile(&aggregate.wall_latency_ns, 99),
        ThresholdMetric::ReliabilityFailureRatePpm => {
            Ok(aggregate.reliability_failure_rate_ppm.clone())
        }
        ThresholdMetric::PeakRssP99Bytes => p99(&aggregate.process_tree_peak_rss_bytes),
        ThresholdMetric::CancellationLatencyP99Ns => {
            Err(PerformanceEvidenceError::InvalidThreshold)
        }
    }
}

fn percentile(
    distribution: &EvidenceValue<ObservedDistribution>,
    percentile: u8,
) -> Result<EvidenceValue<u64>, PerformanceEvidenceError> {
    match distribution {
        EvidenceValue::Observed { value } => Ok(EvidenceValue::observed(match percentile {
            50 => value.p50,
            95 => value.p95,
            99 => value.p99,
            _ => return Err(PerformanceEvidenceError::InvalidThreshold),
        })),
        EvidenceValue::Unavailable { reason_code } => {
            Ok(EvidenceValue::unavailable(reason_code.clone()))
        }
        EvidenceValue::Target { .. } => Err(PerformanceEvidenceError::TargetInObservation),
    }
}

fn p99(
    distribution: &EvidenceValue<ObservedDistribution>,
) -> Result<EvidenceValue<u64>, PerformanceEvidenceError> {
    percentile(distribution, 99)
}

fn threshold_disposition(
    threshold: &PerformanceThreshold,
    observed: &EvidenceValue<u64>,
) -> GateDisposition {
    if threshold.class == ThresholdClass::Aspirational {
        return GateDisposition::Pass;
    }
    match observed {
        EvidenceValue::Observed { value } if *value <= threshold.upper_bound => {
            GateDisposition::Pass
        }
        EvidenceValue::Observed { .. } | EvidenceValue::Target { .. } => GateDisposition::Blocked,
        EvidenceValue::Unavailable { .. } => match threshold.unavailable_policy {
            UnavailablePolicy::Fallback => GateDisposition::Fallback,
            UnavailablePolicy::Block => GateDisposition::Blocked,
        },
    }
}

fn overall_disposition(evaluations: &[ThresholdEvaluation]) -> GateDisposition {
    evaluations
        .iter()
        .filter(|evaluation| evaluation.class != ThresholdClass::Aspirational)
        .map(|evaluation| evaluation.disposition)
        .max()
        .unwrap_or(GateDisposition::Pass)
}

fn reject_target<T>(value: &EvidenceValue<T>) -> Result<(), PerformanceEvidenceError> {
    match value {
        EvidenceValue::Target { .. } => Err(PerformanceEvidenceError::TargetInObservation),
        EvidenceValue::Unavailable { reason_code } if !valid_identifier(reason_code) => {
            Err(PerformanceEvidenceError::InvalidIdentifier)
        }
        EvidenceValue::Observed { .. } | EvidenceValue::Unavailable { .. } => Ok(()),
    }
}

fn validate_map_keys<T>(map: &BTreeMap<String, T>) -> Result<(), PerformanceEvidenceError> {
    if map.keys().all(|key| valid_identifier(key)) {
        Ok(())
    } else {
        Err(PerformanceEvidenceError::InvalidIdentifier)
    }
}

fn validate_reason_codes(values: &[String]) -> Result<(), PerformanceEvidenceError> {
    if values.iter().all(|value| valid_identifier(value)) {
        Ok(())
    } else {
        Err(PerformanceEvidenceError::InvalidIdentifier)
    }
}

fn validate_digest(value: &str) -> Result<(), PerformanceEvidenceError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PerformanceEvidenceError::InvalidDigest)
    }
}

fn validate_source_revision(value: &str) -> Result<(), PerformanceEvidenceError> {
    if matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PerformanceEvidenceError::InvalidDigest)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/')
        })
        && !value.contains("..")
        && !value.starts_with('/')
        && !value.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_do_not_interpolate() {
        let values = (1..=100).collect::<Vec<_>>();
        let distribution = nearest_rank_distribution(&values).expect("distribution builds");

        assert_eq!(distribution.p50, 50);
        assert_eq!(distribution.p95, 95);
        assert_eq!(distribution.p99, 99);
        assert_eq!(distribution.minimum, 1);
        assert_eq!(distribution.maximum, 100);
    }

    #[test]
    fn terminal_failures_and_exclusions_reconcile_without_entering_latency() {
        let protocol = protocol(100);
        let mut samples = successful_samples("repo.index", 100);
        samples.push(sample(
            "repo.index",
            100,
            PerformanceSampleOutcome::Failed {
                error_code: "ADAPTER_FAILED".to_owned(),
            },
        ));
        samples.push(sample(
            "repo.index",
            101,
            PerformanceSampleOutcome::TimedOut,
        ));
        samples.push(sample(
            "repo.index",
            102,
            PerformanceSampleOutcome::Excluded {
                reason_code: "host_interference".to_owned(),
            },
        ));
        add_required_tool_samples(&protocol, &mut samples, "repo.index");

        let package = build_performance_evidence(
            protocol,
            environment(),
            samples,
            cancellation_samples(),
            None,
            vec!["rss_polling_may_miss_short_spikes".to_owned()],
        )
        .expect("package reconciles");
        let aggregate = package
            .aggregates
            .iter()
            .find(|aggregate| aggregate.tool_id == "repo.index")
            .expect("repo index aggregate exists");

        assert_eq!(aggregate.reconciliation.attempted, 103);
        assert_eq!(aggregate.reconciliation.succeeded, 100);
        assert_eq!(aggregate.reconciliation.failed, 1);
        assert_eq!(aggregate.reconciliation.timed_out, 1);
        assert_eq!(aggregate.reconciliation.excluded, 1);
        assert_eq!(
            aggregate.reliability_failure_rate_ppm,
            EvidenceValue::observed(19_607)
        );
    }

    #[test]
    fn insufficient_primary_denominator_blocks_claim_construction() {
        let protocol = protocol(100);
        let mut samples = successful_samples("repo.index", 99);
        add_required_tool_samples(&protocol, &mut samples, "repo.index");

        let error = build_performance_evidence(
            protocol,
            environment(),
            samples,
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect_err("p99 denominator must be enforced");

        assert!(matches!(
            error,
            PerformanceEvidenceError::InsufficientPrimarySamples
        ));
    }

    #[test]
    fn observed_values_cannot_be_relabelled_as_targets() {
        let protocol = protocol(100);
        let mut samples = all_successful_samples(&protocol);
        samples[0].process_tree_cpu_ns = EvidenceValue::target(10);

        let error = build_performance_evidence(
            protocol,
            environment(),
            samples,
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect_err("targets are invalid in raw observations");

        assert!(matches!(
            error,
            PerformanceEvidenceError::TargetInObservation
        ));
    }

    #[test]
    fn unavailable_measurements_require_source_free_reason_codes() {
        let protocol = protocol(100);
        let mut manifest = environment();
        manifest.resource_method.rss_resolution_bytes =
            EvidenceValue::unavailable("C:\\private\\repo\\secret.txt");
        let error = build_performance_evidence(
            protocol.clone(),
            manifest,
            all_successful_samples(&protocol),
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect_err("environment reason paths must be rejected");
        assert!(matches!(error, PerformanceEvidenceError::InvalidIdentifier));

        let mut samples = all_successful_samples(&protocol);
        samples[0].dimensions.source_bytes = EvidenceValue::unavailable("api_key=source-secret");
        let error = build_performance_evidence(
            protocol.clone(),
            environment(),
            samples,
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect_err("raw sample reason text must be rejected");
        assert!(matches!(error, PerformanceEvidenceError::InvalidIdentifier));

        let mut package = build_performance_evidence(
            protocol.clone(),
            environment(),
            all_successful_samples(&protocol),
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect("canonical package builds");
        package.raw_samples[0].process_tree_cpu_ns =
            EvidenceValue::unavailable("/private/repo/source.rs");
        assert!(matches!(
            encode_performance_evidence(&package),
            Err(PerformanceEvidenceError::InvalidIdentifier)
        ));
    }

    #[test]
    fn mutated_percentiles_fail_package_validation() {
        let protocol = protocol(100);
        let mut package = build_performance_evidence(
            protocol.clone(),
            environment(),
            all_successful_samples(&protocol),
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect("package builds");
        let EvidenceValue::Observed { value } = &mut package.aggregates[0].wall_latency_ns else {
            panic!("latency is observed");
        };
        value.p99 = value.p99.saturating_add(1);

        assert!(matches!(
            validate_performance_evidence(&package),
            Err(PerformanceEvidenceError::SampleReconciliation)
        ));
    }

    #[test]
    fn environment_rejects_paths_and_invalid_digests() {
        let mut manifest = environment();
        manifest.cpu_model = "C:\\private\\host".to_owned();
        assert!(matches!(
            validate_environment(&manifest),
            Err(PerformanceEvidenceError::InvalidIdentifier)
        ));

        let mut manifest = environment();
        manifest.tokenizer_sha256 = "not-a-digest".to_owned();
        assert!(matches!(
            validate_environment(&manifest),
            Err(PerformanceEvidenceError::InvalidDigest)
        ));
    }

    #[test]
    fn threshold_algorithm_distinguishes_pass_fallback_and_blocked() {
        let observed = EvidenceValue::observed(10);
        let mut threshold = PerformanceThreshold {
            threshold_id: "latency".to_owned(),
            subject_id: "repo.status".to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP99Ns,
            upper_bound: 10,
            unavailable_policy: UnavailablePolicy::Fallback,
        };
        assert_eq!(
            threshold_disposition(&threshold, &observed),
            GateDisposition::Pass
        );
        threshold.upper_bound = 9;
        assert_eq!(
            threshold_disposition(&threshold, &observed),
            GateDisposition::Blocked
        );
        assert_eq!(
            threshold_disposition(
                &threshold,
                &EvidenceValue::unavailable("sampler_unavailable")
            ),
            GateDisposition::Fallback
        );
        threshold.unavailable_policy = UnavailablePolicy::Block;
        assert_eq!(
            threshold_disposition(
                &threshold,
                &EvidenceValue::unavailable("sampler_unavailable")
            ),
            GateDisposition::Blocked
        );
    }

    #[test]
    fn cancellation_post_work_bound_is_enforced() {
        let protocol = protocol(100);
        let mut cancellations = cancellation_samples();
        cancellations[0].post_cancel_work_ns = 1_000_001;

        assert!(matches!(
            build_performance_evidence(
                protocol.clone(),
                environment(),
                all_successful_samples(&protocol),
                cancellations,
                None,
                Vec::new(),
            ),
            Err(PerformanceEvidenceError::CancellationEvidence)
        ));
    }

    #[test]
    fn canonical_round_trip_preserves_source_free_package() {
        let protocol = protocol(100);
        let package = build_performance_evidence(
            protocol.clone(),
            environment(),
            all_successful_samples(&protocol),
            cancellation_samples(),
            None,
            Vec::new(),
        )
        .expect("package builds");
        let bytes = encode_performance_evidence(&package).expect("package encodes");
        let decoded = decode_performance_evidence(&bytes).expect("package decodes");

        assert_eq!(decoded, package);
        assert_eq!(
            performance_evidence_sha256(&decoded)
                .expect("digest builds")
                .len(),
            64
        );
        let encoded = String::from_utf8(bytes).expect("json is utf8");
        assert!(!encoded.contains("C:\\"));
        assert!(!encoded.contains("/home/"));
        assert!(!encoded.contains("source_text"));
    }

    fn protocol(minimum: u64) -> PerformanceProtocol {
        PerformanceProtocol {
            schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
            protocol_id: "mcp-performance-test-v1".to_owned(),
            warmup_samples: 5,
            timeout_ms: 30_000,
            concurrency: 1,
            conditions: vec![PerformanceCondition {
                condition_id: "warm-small-complete".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Warm,
                concurrency: 1,
            }],
            tools: PUBLIC_MCP_TOOLS
                .iter()
                .map(|tool| ToolMeasurementPlan::Required {
                    tool_id: (*tool).to_owned(),
                    primary_condition_id: "warm-small-complete".to_owned(),
                    minimum_success_samples: minimum,
                })
                .collect(),
            cancellation_classes: vec![
                CancellationClassPlan::Required {
                    class_id: "architecture-analysis".to_owned(),
                    tool_id: "architecture.cycles".to_owned(),
                    minimum_success_samples: 1,
                    maximum_post_cancel_work_ns: 1_000_000,
                },
                CancellationClassPlan::Required {
                    class_id: "advanced-query".to_owned(),
                    tool_id: "query.advanced".to_owned(),
                    minimum_success_samples: 1,
                    maximum_post_cancel_work_ns: 1_000_000,
                },
            ],
            thresholds: vec![PerformanceThreshold {
                threshold_id: "repo-status-p99".to_owned(),
                subject_id: "repo.status".to_owned(),
                condition_id: "warm-small-complete".to_owned(),
                class: ThresholdClass::Gate,
                metric: ThresholdMetric::WallLatencyP99Ns,
                upper_bound: 25_000_000,
                unavailable_policy: UnavailablePolicy::Block,
            }],
            exclusion_reason_codes: vec!["host_interference".to_owned()],
        }
    }

    fn environment() -> PerformanceEnvironmentManifest {
        let digest = "01".repeat(32);
        PerformanceEnvironmentManifest {
            schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
            source_revision: "abcdef0123456789abcdef0123456789abcdef01".to_owned(),
            protocol_sha256: performance_protocol_sha256(&protocol(100))
                .expect("test protocol hashes"),
            rustc_verbose_sha256: digest.clone(),
            dependency_graph_sha256: digest.clone(),
            target_triple: "x86_64-pc-windows-msvc".to_owned(),
            operating_system: "windows-2025".to_owned(),
            architecture: "x86_64".to_owned(),
            cpu_model: "synthetic-calibration-cpu".to_owned(),
            cpu_count: 8,
            memory_bytes: 16 * 1024 * 1024 * 1024,
            build_profile: "release".to_owned(),
            features: vec!["default".to_owned()],
            binary_sha256: BTreeMap::from([
                ("rootlight-daemon".to_owned(), digest.clone()),
                ("rootlight-mcp".to_owned(), digest.clone()),
            ]),
            fixture_sha256: BTreeMap::from([
                ("small".to_owned(), digest.clone()),
                ("large".to_owned(), digest.clone()),
            ]),
            tokenizer_id: "o200k_base".to_owned(),
            tokenizer_sha256: digest,
            monotonic_clock: "std-instant".to_owned(),
            background_process_policy: "isolated-best-effort".to_owned(),
            resource_method: ResourceMeasurementMethod {
                method_id: "unavailable-portable".to_owned(),
                platform: "windows".to_owned(),
                polling_interval_us: EvidenceValue::unavailable("sampler_unavailable"),
                cpu_resolution_ns: EvidenceValue::unavailable("sampler_unavailable"),
                rss_resolution_bytes: EvidenceValue::unavailable("sampler_unavailable"),
                process_tree_included: false,
                caveat_codes: vec!["cpu_rss_unavailable".to_owned()],
            },
        }
    }

    fn sample(tool: &str, ordinal: u64, outcome: PerformanceSampleOutcome) -> PerformanceRawSample {
        PerformanceRawSample {
            schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
            tool_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            ordinal,
            phase: SamplePhase::Measured,
            elapsed_ns: ordinal.saturating_add(1_000),
            process_tree_cpu_ns: EvidenceValue::unavailable("sampler_unavailable"),
            process_tree_peak_rss_bytes: EvidenceValue::unavailable("sampler_unavailable"),
            dimensions: PerformanceDimensions {
                rows: EvidenceValue::observed(1),
                edges: EvidenceValue::observed(0),
                traversal_depth: EvidenceValue::observed(0),
                result_items: EvidenceValue::observed(1),
                source_bytes: EvidenceValue::observed(0),
                response_json_bytes: EvidenceValue::observed(128),
                estimated_tokens: EvidenceValue::observed(32),
                actual_tokens: EvidenceValue::observed(31),
                calls: 1,
            },
            outcome,
        }
    }

    fn successful_samples(tool: &str, count: u64) -> Vec<PerformanceRawSample> {
        (0..count)
            .map(|ordinal| sample(tool, ordinal, PerformanceSampleOutcome::Succeeded))
            .collect()
    }

    fn all_successful_samples(protocol: &PerformanceProtocol) -> Vec<PerformanceRawSample> {
        let mut samples = Vec::new();
        for plan in &protocol.tools {
            if let ToolMeasurementPlan::Required {
                tool_id,
                minimum_success_samples,
                ..
            } = plan
            {
                samples.extend(successful_samples(tool_id, *minimum_success_samples));
            }
        }
        samples
    }

    fn add_required_tool_samples(
        protocol: &PerformanceProtocol,
        samples: &mut Vec<PerformanceRawSample>,
        skip: &str,
    ) {
        for plan in &protocol.tools {
            if let ToolMeasurementPlan::Required {
                tool_id,
                minimum_success_samples,
                ..
            } = plan
                && tool_id != skip
            {
                samples.extend(successful_samples(tool_id, *minimum_success_samples));
            }
        }
    }

    fn cancellation_samples() -> Vec<CancellationRawSample> {
        [
            ("architecture-analysis", "architecture.cycles"),
            ("advanced-query", "query.advanced"),
        ]
        .into_iter()
        .map(|(class_id, tool_id)| CancellationRawSample {
            schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
            class_id: class_id.to_owned(),
            tool_id: tool_id.to_owned(),
            ordinal: 0,
            cancellation_latency_ns: 100_000,
            post_cancel_work_ns: 0,
            outcome: PerformanceSampleOutcome::Succeeded,
        })
        .collect()
    }
}
