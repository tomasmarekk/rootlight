//! Pluggable process-tree resource accounting for benchmark samples.
//!
//! The begin/end boundary encloses the complete parse call so a future
//! platform implementation can compute CPU deltas and interval peak RSS. The
//! portable default reports unavailable instead of parent-only telemetry.

use crate::EvidenceValue;

/// Process-tree CPU and peak-memory evidence for one complete sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeMeasurement {
    /// Process-tree CPU nanoseconds accumulated during the sample.
    pub cpu_ns: EvidenceValue<u64>,
    /// Process-tree peak resident bytes observed during the sample.
    pub peak_rss_bytes: EvidenceValue<u64>,
}

/// An active process-tree measurement interval.
pub trait ProcessTreeSample {
    /// Ends the interval and returns its process-tree delta and peak evidence.
    fn finish(self) -> ProcessTreeMeasurement;
}

/// Begins scoped process-tree accounting without shell commands.
pub trait ProcessTreeSampler: Send + Sync {
    /// Concrete active interval returned by this sampler.
    type Sample: ProcessTreeSample;

    /// Begins accounting immediately before the parser call.
    fn begin(&self) -> Self::Sample;
}

/// Honest fallback used until an audited platform sampler is available.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableProcessTreeSampler;

/// Active unavailable interval preserving the same begin/end lifecycle.
#[derive(Debug, Clone, Copy)]
pub struct UnavailableProcessTreeSample;

impl ProcessTreeSampler for UnavailableProcessTreeSampler {
    type Sample = UnavailableProcessTreeSample;

    fn begin(&self) -> Self::Sample {
        UnavailableProcessTreeSample
    }
}

impl ProcessTreeSample for UnavailableProcessTreeSample {
    fn finish(self) -> ProcessTreeMeasurement {
        ProcessTreeMeasurement {
            cpu_ns: EvidenceValue::unavailable("process_tree_cpu_sampler_unavailable"),
            peak_rss_bytes: EvidenceValue::unavailable("process_tree_rss_sampler_unavailable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_sampler_never_mislabels_parent_metrics_as_process_tree_data() {
        let measurement = UnavailableProcessTreeSampler.begin().finish();

        assert!(matches!(
            measurement.cpu_ns,
            EvidenceValue::Unavailable { .. }
        ));
        assert!(matches!(
            measurement.peak_rss_bytes,
            EvidenceValue::Unavailable { .. }
        ));
    }
}
