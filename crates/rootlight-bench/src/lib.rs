//! Deterministic benchmark evidence contracts and bounded M05 parser harnesses.
//!
//! The crate is development-only: shipping binaries must not depend on it.
//! Result publication is immutable and keeps targets separate from observations.

#![forbid(unsafe_code)]

mod bundle;
mod model;

pub use bundle::{BundleError, ResultBundle, publish_bundle, verify_bundle};
pub use model::{
    AgentTrajectory, Availability, BenchmarkCommand, BuildProvenance, CoverageEvidence,
    DatasetEntry, DatasetManifest, EnvironmentEvidence, EvidenceValue, MetricDistribution,
    QualityEvidence, RawSample, ResultSummary, SampleOutcome,
};

/// Result-bundle schema version written by this crate.
pub const RESULT_BUNDLE_SCHEMA_VERSION: &str = "1.0";
