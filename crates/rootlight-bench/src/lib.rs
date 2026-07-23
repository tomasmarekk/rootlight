//! Deterministic benchmark evidence for bounded parser and semantic checks.
//!
//! The crate is development-only: shipping binaries must not depend on it.
//! Directory publication remains fail-closed while the VFS private-tree
//! boundary is disabled; CI uses the canonical single-file fallback envelope.

#![forbid(unsafe_code)]

mod bundle;
mod ci;
mod decode;
mod integrity;
mod model;
mod parser;
mod sampler;
mod semantic_contract;
mod token_accounting;
mod trajectory;

pub use bundle::{
    BundleError, BundleLimits, OperationalEvent, OperationalLog, OperationalLogRecord,
    OperationalStatus, ResultBundle, publish_bundle, publish_bundle_with_limits, verify_bundle,
    verify_bundle_with_limits,
};
pub use ci::{
    PARSER_CI_ENVELOPE_SCHEMA_VERSION, PARSER_CI_MAX_ENVELOPE_BYTES, ParserCiEvidenceEnvelope,
    ParserCiEvidenceError, build_parser_ci_evidence, decode_parser_ci_evidence,
    encode_parser_ci_evidence, verify_parser_ci_evidence,
};
pub use decode::{DecodeError, decode_benchmark_command, decode_dataset_manifest};
pub use model::{
    AgentTrajectory, Availability, BenchmarkCommand, BuildProvenance, CoverageEvidence,
    DatasetEntry, DatasetManifest, EnvironmentEvidence, EvidenceValue,
    MAX_SEMANTIC_CALIBRATION_ERROR_PPM, MAX_TRAJECTORIES_PER_BUNDLE, MAX_TRAJECTORY_COUNTER,
    MAX_TRAJECTORY_ELAPSED_NS, MAX_TRAJECTORY_ENCODED_BYTES, MAX_TRAJECTORY_EVIDENCE_ARTIFACTS,
    MAX_TRAJECTORY_LABEL_BYTES, MAX_TRAJECTORY_REFERENCES, MAX_TRAJECTORY_SOURCE_BYTES,
    MAX_TRAJECTORY_STEPS, MAX_TRAJECTORY_TOKENS, MIN_SEMANTIC_PRECISION_PPM,
    MIN_SEMANTIC_RECALL_PPM, MetricDistribution, QualityEvidence, RawSample, ResultSummary,
    SEMANTIC_QUALITY_RUBRIC_ID, SampleOutcome, SemanticQualityMeasurement, TrajectoryBudget,
    TrajectoryCompleteness, TrajectoryEvidenceKind, TrajectoryEvidenceManifest,
    TrajectoryEvidenceReference, TrajectoryExposureProfile, TrajectoryOperationStatus,
    TrajectoryStep, TrajectoryToolIdentity, TrajectoryUsage,
};
pub use parser::{
    ParserBenchmarkConfig, ParserBenchmarkEvidence, ParserDatasetInput, ParserRunError,
    SemanticFactProbe, UnavailableSemanticFacts, run_parser_benchmark,
};
pub use sampler::{
    ProcessTreeMeasurement, ProcessTreeSample, ProcessTreeSampler, UnavailableProcessTreeSample,
    UnavailableProcessTreeSampler,
};
pub use semantic_contract::{
    SEMANTIC_EVIDENCE_ENVELOPE_MAX_BYTES, SEMANTIC_EVIDENCE_ENVELOPE_SCHEMA,
    SEMANTIC_EVIDENCE_MAX_BYTES, SEMANTIC_EVIDENCE_MAX_EXPECTATIONS,
    SEMANTIC_EVIDENCE_SCHEMA_VERSION, SemanticEvidence, SemanticEvidenceError,
    build_semantic_evidence, encode_semantic_evidence, encode_semantic_evidence_envelope,
};
pub use token_accounting::{
    ActualTokenizerIdentity, TOKEN_ACCOUNTING_SCHEMA_VERSION, TokenAccountingError, TokenInputKind,
    TokenMeasurement, TokenTotals, WorkflowTokenAccounting, sha256_hex,
};
pub use trajectory::{
    AdapterAvailabilityPolicy, BoundedFileExplorationAdapter, MAX_ATTEMPTS_PER_CONDITION,
    MAX_TRAJECTORY_PACKAGE_BYTES, MIN_PREREGISTERED_WORKFLOWS, O200kTrajectoryTokenizer,
    RawTrajectoryAttempt, RawTrajectoryCall, TRAJECTORY_PROTOCOL_SCHEMA_VERSION,
    TRAJECTORY_RUNNER_ID, TrajectoryAdapter, TrajectoryAttemptOutcome, TrajectoryAttemptRecord,
    TrajectoryCallRecord, TrajectoryClaimSignals, TrajectoryCondition, TrajectoryConditionProtocol,
    TrajectoryDenominator, TrajectoryError, TrajectoryEvidencePackage, TrajectoryExecutionBoundary,
    TrajectoryExecutionInput, TrajectoryProtocol, TrajectoryProtocolDigests, TrajectoryRetryPolicy,
    TrajectorySharedBounds, TrajectoryStoppingPolicy, TrajectoryTokenizer,
    TrajectoryWorkflowFamily, TrajectoryWorkflowProtocol, UnavailableTrajectoryAdapter,
    decode_trajectory_evidence, encode_trajectory_evidence, preregistered_trajectory_protocol,
    run_trajectory_suite,
};

/// Result-bundle schema version written and verified by this crate.
///
/// Version 2.1 adds closed, bounded, source-free agent trajectories while
/// retaining read compatibility with empty-trajectory version 2.0 bundles.
pub const RESULT_BUNDLE_SCHEMA_VERSION: &str = "2.1";

pub(crate) const LEGACY_RESULT_BUNDLE_SCHEMA_VERSION: &str = "2.0";

pub(crate) fn is_supported_result_bundle_schema(schema: &str) -> bool {
    matches!(
        schema,
        RESULT_BUNDLE_SCHEMA_VERSION | LEGACY_RESULT_BUNDLE_SCHEMA_VERSION
    )
}
