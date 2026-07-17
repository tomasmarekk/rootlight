//! Deterministic benchmark evidence contracts and bounded M05 parser harnesses.
//!
//! The crate is development-only: shipping binaries must not depend on it.
//! Result publication is immutable and keeps targets separate from observations.

#![forbid(unsafe_code)]

mod bundle;
mod decode;
mod integrity;
mod model;
mod parser;
mod sampler;

pub use bundle::{
    BundleError, BundleLimits, OperationalEvent, OperationalLog, OperationalLogRecord,
    OperationalStatus, ResultBundle, publish_bundle, publish_bundle_with_limits, verify_bundle,
    verify_bundle_with_limits,
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

/// Result-bundle schema version written and verified by this crate.
///
/// Version 2 makes corpus-backed semantic quality and strict cross-artifact
/// verification normative. Version 1 bundles remain identifiable but are not
/// accepted as evidence under the stronger contract.
pub const RESULT_BUNDLE_SCHEMA_VERSION: &str = "2.0";
