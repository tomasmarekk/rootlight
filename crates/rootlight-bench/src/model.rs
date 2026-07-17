//! Versioned wire models for immutable benchmark evidence.
//!
//! Every value that could otherwise be mistaken for a measurement carries an
//! explicit observed, target, or unavailable classification.

use std::collections::BTreeMap;

use serde::Serialize;

/// Versioned semantic-quality rubric used by the M05 parser evidence slice.
pub const SEMANTIC_QUALITY_RUBRIC_ID: &str = "m05-parser-semantic-eligibility-2.0";
/// Minimum accepted semantic precision, in millionths.
pub const MIN_SEMANTIC_PRECISION_PPM: u64 = 980_000;
/// Minimum accepted semantic recall, in millionths.
pub const MIN_SEMANTIC_RECALL_PPM: u64 = 920_000;
/// Maximum accepted expected calibration error, in millionths.
pub const MAX_SEMANTIC_CALIBRATION_ERROR_PPM: u64 = 50_000;
pub(crate) const MILLION_PPM: u64 = 1_000_000;

/// A measurement's evidence classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceValue<T> {
    /// A value measured by this recorded run.
    Observed {
        /// Measured value.
        value: T,
    },
    /// A normative or comparison target that was not measured.
    Target {
        /// Target value.
        value: T,
    },
    /// A value the current platform or harness could not measure.
    Unavailable {
        /// Stable source-free reason code.
        reason_code: String,
    },
}

impl<T> EvidenceValue<T> {
    /// Creates an observed value.
    #[must_use]
    pub const fn observed(value: T) -> Self {
        Self::Observed { value }
    }

    /// Creates a target value.
    #[must_use]
    pub const fn target(value: T) -> Self {
        Self::Target { value }
    }

    /// Creates an unavailable value with a stable source-free reason.
    #[must_use]
    pub fn unavailable(reason_code: impl Into<String>) -> Self {
        Self::Unavailable {
            reason_code: reason_code.into(),
        }
    }
}

/// Availability of a correctness or telemetry requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Availability {
    /// The requirement was measured and satisfied.
    Available,
    /// The requirement was measured and failed.
    Failed {
        /// Stable source-free failure code.
        reason_code: String,
    },
    /// The requirement could not be measured.
    Unavailable {
        /// Stable source-free reason code.
        reason_code: String,
    },
}

/// Source-free environment facts required by the benchmark contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentEvidence {
    /// Environment schema version.
    pub schema_version: String,
    /// CPU model.
    pub cpu_model: EvidenceValue<String>,
    /// CPU topology description.
    pub cpu_topology: EvidenceValue<String>,
    /// Installed memory in bytes.
    pub ram_bytes: EvidenceValue<u64>,
    /// Operating-system identity.
    pub operating_system: EvidenceValue<String>,
    /// Kernel identity.
    pub kernel: EvidenceValue<String>,
    /// Filesystem identity.
    pub filesystem: EvidenceValue<String>,
    /// Storage-device identity.
    pub storage_device: EvidenceValue<String>,
    /// Power-mode identity.
    pub power_mode: EvidenceValue<String>,
    /// Container limits.
    pub container_limits: EvidenceValue<String>,
    /// Rust compiler identity.
    pub compiler: EvidenceValue<String>,
    /// Exact benchmark binary SHA-256.
    pub binary_sha256: EvidenceValue<String>,
    /// Cargo feature and build profile.
    pub feature_profile: String,
    /// SQLite version and compile options.
    pub sqlite: EvidenceValue<String>,
    /// Adapter versions by stable adapter ID.
    pub adapter_versions: EvidenceValue<BTreeMap<String, String>>,
    /// Audited grammar versions by stable language ID.
    pub grammar_versions: EvidenceValue<BTreeMap<String, String>>,
    /// crates.io source-package checksums by stable language ID.
    pub grammar_source_package_checksums: EvidenceValue<BTreeMap<String, String>>,
    /// Generated parser and scanner hashes by stable component ID.
    pub grammar_hashes: EvidenceValue<BTreeMap<String, String>>,
    /// Locale identity.
    pub locale: EvidenceValue<String>,
    /// Background-process policy.
    pub background_process_policy: EvidenceValue<String>,
    /// Monotonic clock source.
    pub clock_source: EvidenceValue<String>,
    /// Process-tree accounting capability.
    pub process_tree_accounting: Availability,
}

/// One immutable dataset input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetEntry {
    /// Stable dataset-local ID.
    pub id: String,
    /// Audited grammar family.
    pub grammar_family: String,
    /// Normalized language ID.
    pub language: String,
    /// Repository-relative source path.
    pub relative_path: String,
    /// Exact source SHA-256.
    pub source_sha256: String,
    /// Exact source byte count.
    pub source_bytes: u64,
    /// Physical source line count under the manifest's counting rule.
    pub physical_lines: u64,
    /// Whether the source is generated.
    pub generated: bool,
}

/// Versioned immutable dataset manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifest {
    /// Manifest schema version.
    pub schema_version: String,
    /// Stable dataset identity.
    pub dataset_id: String,
    /// Immutable dataset revision.
    pub revision: String,
    /// Human-independent source-scope rule.
    pub scope_rule: String,
    /// Physical-LOC counting rule.
    pub loc_counting_rule: String,
    /// Dataset entries in canonical ID order.
    pub entries: Vec<DatasetEntry>,
}

/// Exact source and build identity for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildProvenance {
    /// Provenance schema version.
    pub schema_version: String,
    /// Rootlight source revision.
    pub source_revision: String,
    /// Benchmark binary revision.
    pub binary_revision: String,
    /// Cargo profile.
    pub build_profile: String,
    /// Enabled features in sorted order.
    pub features: Vec<String>,
    /// Rust target triple.
    pub target: String,
}

/// Exact benchmark command and deterministic trial policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCommand {
    /// Command schema version.
    pub schema_version: String,
    /// Logical subcommand.
    pub subcommand: String,
    /// Source-free normalized arguments.
    pub arguments: Vec<String>,
    /// Randomization seed.
    pub seed: u64,
    /// Warm-up rounds.
    pub warmup_rounds: u32,
    /// Measured trial rounds.
    pub trial_rounds: u32,
    /// Per-sample monotonic deadline.
    pub timeout_ms: u64,
}

/// Terminal result for one retained raw sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SampleOutcome {
    /// The parser transaction committed.
    Succeeded,
    /// The transaction failed with a stable source-free code.
    Failed {
        /// Stable failure code.
        error_code: String,
    },
    /// The monotonic deadline elapsed.
    TimedOut,
    /// The sample was cancelled.
    Cancelled,
}

/// One retained parser benchmark sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawSample {
    /// Sample schema version.
    pub schema_version: String,
    /// Zero-based deterministic execution ordinal.
    pub ordinal: u64,
    /// Whether this sample is warm-up or measured.
    pub phase: String,
    /// Dataset entry ID.
    pub dataset_entry_id: String,
    /// Grammar family.
    pub grammar_family: String,
    /// Monotonic elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Source bytes processed.
    pub source_bytes: u64,
    /// Physical lines processed.
    pub physical_lines: u64,
    /// Concrete syntax nodes observed.
    pub syntax_nodes: u64,
    /// Syntax facts emitted.
    pub syntax_facts: u64,
    /// Later semantic extraction fact count, when available.
    pub semantic_facts: EvidenceValue<u64>,
    /// Process-tree CPU nanoseconds.
    pub process_tree_cpu_ns: EvidenceValue<u64>,
    /// Process-tree peak RSS bytes.
    pub process_tree_peak_rss_bytes: EvidenceValue<u64>,
    /// Terminal sample outcome.
    pub outcome: SampleOutcome,
    /// Whether the retained sample lies outside the recorded outlier fence.
    pub is_outlier: bool,
}

/// Deterministic percentile and throughput summary for one metric family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDistribution {
    /// Number of successful measured samples.
    pub sample_count: u64,
    /// Median elapsed nanoseconds.
    pub p50_ns: EvidenceValue<u64>,
    /// 95th-percentile elapsed nanoseconds.
    pub p95_ns: EvidenceValue<u64>,
    /// 99th-percentile elapsed nanoseconds.
    pub p99_ns: EvidenceValue<u64>,
    /// Aggregate physical lines per second.
    pub physical_lines_per_second: EvidenceValue<u64>,
    /// Aggregate files per second.
    pub files_per_second: EvidenceValue<u64>,
    /// Aggregate concrete syntax nodes per second.
    pub syntax_nodes_per_second: EvidenceValue<u64>,
    /// Aggregate syntax facts per source byte, scaled by one million.
    pub syntax_facts_per_source_byte_ppm: EvidenceValue<u64>,
    /// Retained outlier count.
    pub outlier_count: u64,
}

/// Aggregate result summary, including claim eligibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSummary {
    /// Summary schema version.
    pub schema_version: String,
    /// Benchmark identity.
    pub benchmark_id: String,
    /// Overall semantic correctness eligibility.
    pub semantic_eligibility: Availability,
    /// Per-family distributions in stable key order.
    pub families: BTreeMap<String, MetricDistribution>,
    /// Failed measured samples.
    pub failed_samples: u64,
    /// Timed-out measured samples.
    pub timed_out_samples: u64,
    /// Cancelled measured samples.
    pub cancelled_samples: u64,
    /// Confidence interval status for this first slice.
    pub confidence_intervals: Availability,
}

/// Coverage evidence retained separately from performance summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageEvidence {
    /// Coverage schema version.
    pub schema_version: String,
    /// Dataset entries attempted.
    pub attempted_entries: u64,
    /// Dataset entries with a committed parse.
    pub committed_entries: u64,
    /// Explicitly skipped entries and stable reason codes.
    pub skipped: BTreeMap<String, String>,
    /// Parser coverage status by dataset entry.
    pub parser_status: BTreeMap<String, String>,
}

/// Quality evidence and semantic eligibility rubric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QualityEvidence {
    /// Quality schema version.
    pub schema_version: String,
    /// Versioned rubric identity.
    pub rubric_id: String,
    /// Semantic eligibility status.
    pub semantic_eligibility: Availability,
    /// Precision in millionths, when measured.
    pub precision_ppm: EvidenceValue<u64>,
    /// Recall in millionths, when measured.
    pub recall_ppm: EvidenceValue<u64>,
    /// Expected calibration error in millionths, when measured.
    pub expected_calibration_error_ppm: EvidenceValue<u64>,
    /// Unsupported cases by stable category.
    pub unsupported_cases: BTreeMap<String, u64>,
}

/// Corpus-backed semantic quality measurements supplied by an extraction probe.
///
/// Counts alone do not establish correctness. A run can become semantically
/// eligible only when all three quality metrics are observed and satisfy the
/// versioned M05 rubric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticQualityMeasurement {
    /// Measured semantic precision in millionths.
    pub precision_ppm: EvidenceValue<u64>,
    /// Measured semantic recall in millionths.
    pub recall_ppm: EvidenceValue<u64>,
    /// Measured expected calibration error in millionths.
    pub expected_calibration_error_ppm: EvidenceValue<u64>,
    /// Unsupported cases by stable category.
    pub unsupported_cases: BTreeMap<String, u64>,
}

impl SemanticQualityMeasurement {
    /// Creates a measurement whose corpus-backed metrics are unavailable.
    #[must_use]
    pub fn unavailable(reason_code: impl Into<String>) -> Self {
        let reason_code = reason_code.into();
        Self {
            precision_ppm: EvidenceValue::unavailable(reason_code.clone()),
            recall_ppm: EvidenceValue::unavailable(reason_code.clone()),
            expected_calibration_error_ppm: EvidenceValue::unavailable(reason_code),
            unsupported_cases: BTreeMap::new(),
        }
    }
}

/// One retained agent trajectory; parser-only runs normally publish none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTrajectory {
    /// Trajectory schema version.
    pub schema_version: String,
    /// Stable task ID.
    pub task_id: String,
    /// Terminal eligibility.
    pub eligibility: Availability,
    /// Tool calls retained as source-free structured records.
    pub tool_calls: Vec<String>,
    /// Total model tokens, when measured.
    pub total_tokens: EvidenceValue<u64>,
}
