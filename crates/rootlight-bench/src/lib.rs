//! Deterministic benchmark evidence for bounded parser and semantic checks.
//!
//! The crate is development-only: shipping binaries must not depend on it.
//! Directory publication remains fail-closed until the VFS private-tree
//! boundary is accepted; CI uses the canonical single-file fallback envelope.

#![forbid(unsafe_code)]

mod bundle;
mod ci;
mod decode;
mod integrity;
mod m09;
mod model;
mod parser;
mod sampler;

pub use bundle::{
    BundleError, BundleLimits, OperationalEvent, OperationalLog, OperationalLogRecord,
    OperationalStatus, ResultBundle, publish_bundle, publish_bundle_with_limits, verify_bundle,
    verify_bundle_with_limits,
};
pub use ci::{
    M05_CI_ENVELOPE_SCHEMA_VERSION, M05_CI_MAX_ENVELOPE_BYTES, M05CiEvidenceEnvelope,
    M05CiEvidenceError, build_m05_ci_evidence, decode_m05_ci_evidence, encode_m05_ci_evidence,
    verify_m05_ci_evidence,
};
pub use decode::{DecodeError, decode_benchmark_command, decode_dataset_manifest};
pub use m09::{
    M09_EVIDENCE_MAX_BYTES, M09_EVIDENCE_MAX_EXPECTATIONS, M09_EVIDENCE_SCHEMA_VERSION,
    M09EvidenceError, M09SemanticEvidence, build_m09_semantic_evidence,
    encode_m09_semantic_evidence,
};
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

/// Result-bundle schema version written and verified by this crate.
///
/// Version 2 makes corpus-backed semantic quality and strict cross-artifact
/// verification normative. Version 1 bundles remain identifiable but are not
/// accepted as evidence under the stronger contract.
pub const RESULT_BUNDLE_SCHEMA_VERSION: &str = "2.0";
