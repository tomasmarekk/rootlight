//! Deterministic benchmark evidence for bounded parser and semantic checks.
//!
//! The crate is development-only: shipping binaries must not depend on it.
//! Directory publication remains fail-closed while the VFS private-tree
//! boundary is disabled; CI uses the canonical single-file fallback envelope.

#![forbid(unsafe_code)]

mod ablation;
mod bundle;
mod ci;
mod decode;
mod integrity;
mod language_workspace;
mod model;
mod parser;
mod performance;
mod project_semantic_holdout;
mod sampler;
mod semantic_contract;
mod token_accounting;
mod trajectory;
mod trajectory_quality;
mod workspace_scale;

pub use ablation::{
    ABLATION_SCHEMA_VERSION, AblationAggregateReport, AblationBlindingKey, AblationDecision,
    AblationError, AblationProtocol, AblationRubricProtocol, AblationVariant,
    AblationVariantProtocol, AutomatedAdjudicationRecord, AutomatedAgreement,
    AutomatedGraderIdentity, AutomatedRawGrade, BlindedAblationCandidate, BlindedCandidateMetrics,
    BlindedRunOutcome, CandidateGrade, CandidateRubricEvidence, ContextPackAblationEvidence,
    DimensionGrade, EfficiencyAlongsideQuality, FinalAutomatedGrade, GraderKind,
    MAX_ABLATION_EVIDENCE_BYTES, MAX_CHECKS_PER_DIMENSION, MAX_QUALITY_LOSS_CENTIPOINTS,
    MAX_QUALITY_SCORE_CENTIPOINTS, PAIRED_BOOTSTRAP_REPLICATES, PairedUncertaintyInterval,
    PreparedBlindedAblation, QualitySensitivity, RestrictedPair, RestrictedPairingEntry,
    RestrictedPairingMap, RubricDimension, RubricDimensionWeight, RubricObservation,
    UncertaintyMethod, UnsupportedClaimAssessment, UnsupportedClaimCategory, VariantAggregate,
    decode_context_pack_ablation, encode_context_pack_ablation, evaluate_context_pack_ablation,
    prepare_blinded_ablation, preregister_context_pack_ablation, produce_context_pack_ablation,
};
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
pub use language_workspace::{
    LANGUAGE_WORKSPACE_EVIDENCE_MAX_BYTES, LANGUAGE_WORKSPACE_EVIDENCE_SCHEMA,
    LanguageWorkspaceEvidence, LanguageWorkspaceEvidenceError, build_language_workspace_evidence,
    encode_language_workspace_evidence, verify_language_workspace_evidence,
};
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
pub use performance::{
    CacheState, CancellationAggregate, CancellationClassPlan, CancellationRawSample, FixtureScale,
    GateDisposition, MAX_PERFORMANCE_EVIDENCE_BYTES, MAX_PERFORMANCE_SAMPLES,
    MIN_PRIMARY_SUCCESS_SAMPLES, ObservedDistribution, PERFORMANCE_EVIDENCE_SCHEMA_VERSION,
    PUBLIC_MCP_TOOLS, PerformanceAggregate, PerformanceCondition, PerformanceDimensions,
    PerformanceEnvironmentManifest, PerformanceEvidenceError, PerformanceEvidencePackage,
    PerformanceProtocol, PerformanceRawSample, PerformanceSampleOutcome, PerformanceThreshold,
    ProcessState, RegressionComparison, ResourceMeasurementMethod, ResultCompleteness, SamplePhase,
    SampleReconciliation, ThresholdClass, ThresholdEvaluation, ThresholdMetric,
    ToolMeasurementPlan, UnavailablePolicy, build_performance_evidence,
    decode_performance_evidence, encode_performance_evidence, nearest_rank_distribution,
    performance_evidence_sha256, performance_protocol_sha256, validate_performance_evidence,
};
pub use project_semantic_holdout::{
    PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_MAX_BYTES, PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_SCHEMA,
    PROJECT_SEMANTIC_HOLDOUT_MAX_BYTES, PROJECT_SEMANTIC_HOLDOUT_SCHEMA,
    ProjectSemanticHoldoutError, ProjectSemanticHoldoutEvidence, build_project_semantic_holdout,
    encode_project_semantic_holdout, encode_project_semantic_holdout_envelope,
    verify_project_semantic_holdout_document,
};
#[cfg(target_os = "linux")]
pub use sampler::{LinuxProcTreeSample, LinuxProcTreeSampler, LinuxProcTreeSamplerError};
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
    AdapterAvailabilityPolicy, BoundedFileExplorationAdapter, BoundedFileObservation,
    MAX_ATTEMPTS_PER_CONDITION, MAX_TRAJECTORY_PACKAGE_BYTES, MIN_PREREGISTERED_WORKFLOWS,
    O200kTrajectoryTokenizer, RawTrajectoryAttempt, RawTrajectoryCall,
    TRAJECTORY_PROTOCOL_SCHEMA_VERSION, TRAJECTORY_RUNNER_ID, TrajectoryAdapter,
    TrajectoryAttemptOutcome, TrajectoryAttemptRecord, TrajectoryCallRecord,
    TrajectoryClaimSignals, TrajectoryCondition, TrajectoryConditionProtocol,
    TrajectoryDenominator, TrajectoryError, TrajectoryEvidencePackage, TrajectoryExecutionBoundary,
    TrajectoryExecutionInput, TrajectoryProtocol, TrajectoryProtocolDigests, TrajectoryRetryPolicy,
    TrajectorySharedBounds, TrajectoryStoppingPolicy, TrajectoryTokenizer,
    TrajectoryWorkflowFamily, TrajectoryWorkflowProtocol, UnavailableTrajectoryAdapter,
    decode_trajectory_evidence, encode_trajectory_evidence, preregistered_trajectory_protocol,
    run_trajectory_suite, trajectory_task_prompt,
};
pub use trajectory_quality::{
    MAX_WORKFLOW_QUALITY_EVIDENCE_BYTES, MAX_WORKFLOW_QUALITY_SCORE_CENTIPOINTS,
    WORKFLOW_QUALITY_RUNNER_ID, WORKFLOW_QUALITY_SCHEMA_VERSION, WorkflowQualityCandidate,
    WorkflowQualityCandidateMeasurement, WorkflowQualityDenominator, WorkflowQualityDimension,
    WorkflowQualityDimensionScore, WorkflowQualityEfficiency, WorkflowQualityError,
    WorkflowQualityEvidence, WorkflowQualityPair, WorkflowQualityPairMeasurement,
    WorkflowQualityProtocol, WorkflowQualitySummary, WorkflowQualityTaskProtocol,
    WorkflowQualityTaskRegistration, build_workflow_quality_evidence,
    decode_workflow_quality_evidence, encode_workflow_quality_evidence,
    preregister_workflow_quality_protocol,
};
pub use workspace_scale::{
    WORKSPACE_SCALE_EVIDENCE_MAX_BYTES, WORKSPACE_SCALE_EVIDENCE_SCHEMA, WorkspaceScaleEvidence,
    WorkspaceScaleEvidenceError, build_workspace_scale_evidence, encode_workspace_scale_evidence,
    verify_workspace_scale_evidence,
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
