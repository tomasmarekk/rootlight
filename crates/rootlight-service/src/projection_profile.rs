//! Retain the last computed fresh projection model under opt-in profiling.
//! Numeric admission terms are observations, not RSS or a new capacity policy.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Observation {
    pub(super) scope: &'static str,
    pub(super) model: &'static str,
    pub(super) stage: &'static str,
    pub(super) document_bytes: u64,
    pub(super) lexical_coverage_bytes: u64,
    pub(super) workspace_bytes: u64,
    pub(super) snapshot_metadata_bytes: u64,
    pub(super) structural_cache_bytes: u64,
    pub(super) explain_index_bytes: u64,
    pub(super) total_bytes: u64,
    pub(super) ceiling_bytes: u64,
}

pub(super) fn observe(observation: Observation) {
    // A fixed scalar slot cannot turn telemetry allocation loss into rejection.
    // The field says last: a capture enclosing several indexes replaces it.
    super::with_analysis_profile(|profile| {
        profile.last_fresh_projection_admission = Some(observation);
    });
}

/// Per-record logical measurements for the same opt-in final projection pass.
pub(super) mod records;
