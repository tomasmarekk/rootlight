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
    MAX_SEMANTIC_CALIBRATION_ERROR_PPM, MIN_SEMANTIC_PRECISION_PPM, MIN_SEMANTIC_RECALL_PPM,
    MetricDistribution, QualityEvidence, RawSample, ResultSummary, SEMANTIC_QUALITY_RUBRIC_ID,
    SampleOutcome, SemanticQualityMeasurement,
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

/// Result-bundle schema version written and verified by this crate.
///
/// Version 2 makes corpus-backed semantic quality and strict cross-artifact
/// verification normative. Version 1 bundles remain identifiable but are not
/// accepted as evidence under the stronger contract.
pub const RESULT_BUNDLE_SCHEMA_VERSION: &str = "2.0";
