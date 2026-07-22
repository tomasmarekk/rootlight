//! Fail-closed policy for claims derived from analytical query results.
//!
//! Execution completeness and semantic coverage describe different failure
//! modes. This module evaluates both before an agent presents a result as a
//! claim, so missing or future states cannot silently become proof of complete
//! analysis.

use rootlight_ir::CoverageStatus;
use rootlight_query::{ExecutionCompleteness, ExecutionCompletenessState};

/// Semantic shape of a claim derived from an analytical result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimKind {
    /// At least one concrete matching fact exists.
    PositiveExistence,
    /// No matching fact exists in the relevant domain.
    NegativeExistence,
    /// The returned values enumerate the entire relevant domain.
    ExhaustiveEnumeration,
    /// A numeric value aggregates the entire relevant domain.
    QuantitativeAggregate,
    /// A returned item is recommended by its rank within the relevant domain.
    RankedRecommendation,
}

/// Required treatment of a claim at the presentation boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimDisposition {
    /// The claim may be stated without an execution or coverage caveat.
    Supported,
    /// The claim requires an explicit caveat limiting it to observed evidence.
    Qualified,
    /// Available evidence cannot support the claim.
    Inconclusive,
}

/// Evidence limitation that prevents an unqualified claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimLimitation {
    /// A known execution limit truncated the supported query domain.
    ExecutionTruncated,
    /// A known portion of the requested query semantics was unsupported.
    ExecutionUnsupportedPartial,
    /// Execution completeness was absent or not recognized.
    ExecutionIndeterminate,
    /// Coverage stopped at a declared semantic bound.
    CoverageBounded,
    /// Coverage represents a sample rather than the complete domain.
    CoverageSampled,
    /// Coverage was absent, unknown, or not recognized.
    CoverageIndeterminate,
}

/// Fail-closed assessment of one analytical claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAssessment {
    disposition: ClaimDisposition,
    limitations: Vec<ClaimLimitation>,
}

impl ClaimAssessment {
    /// Returns the required presentation treatment.
    #[must_use]
    pub const fn disposition(&self) -> ClaimDisposition {
        self.disposition
    }

    /// Returns deterministic execution-first limitations.
    #[must_use]
    pub fn limitations(&self) -> &[ClaimLimitation] {
        &self.limitations
    }

    /// Reports whether the evidence supports an unqualified claim.
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        matches!(self.disposition, ClaimDisposition::Supported)
    }
}

/// Assesses whether an analytical claim is safe to present.
///
/// `None` represents missing or legacy metadata and fails closed as
/// indeterminate. Positive witnesses and rankings over returned evidence remain
/// usable only with an explicit qualification. Negative, exhaustive, and
/// aggregate claims become inconclusive whenever either signal is incomplete.
#[must_use]
pub fn assess_claim(
    kind: ClaimKind,
    execution: Option<&ExecutionCompleteness>,
    coverage: Option<CoverageStatus>,
) -> ClaimAssessment {
    let mut limitations = Vec::with_capacity(2);
    if let Some(limitation) = execution_limitation(execution) {
        limitations.push(limitation);
    }
    if let Some(limitation) = coverage_limitation(coverage) {
        limitations.push(limitation);
    }

    let disposition = if limitations.is_empty() {
        ClaimDisposition::Supported
    } else {
        match kind {
            ClaimKind::PositiveExistence | ClaimKind::RankedRecommendation => {
                ClaimDisposition::Qualified
            }
            ClaimKind::NegativeExistence
            | ClaimKind::ExhaustiveEnumeration
            | ClaimKind::QuantitativeAggregate => ClaimDisposition::Inconclusive,
        }
    };

    ClaimAssessment {
        disposition,
        limitations,
    }
}

fn execution_limitation(execution: Option<&ExecutionCompleteness>) -> Option<ClaimLimitation> {
    match execution.map(ExecutionCompleteness::state) {
        Some(ExecutionCompletenessState::Complete) => None,
        Some(ExecutionCompletenessState::Truncated) => Some(ClaimLimitation::ExecutionTruncated),
        Some(ExecutionCompletenessState::UnsupportedPartial) => {
            Some(ClaimLimitation::ExecutionUnsupportedPartial)
        }
        Some(_) | None => Some(ClaimLimitation::ExecutionIndeterminate),
    }
}

fn coverage_limitation(coverage: Option<CoverageStatus>) -> Option<ClaimLimitation> {
    match coverage {
        Some(CoverageStatus::Complete) => None,
        Some(CoverageStatus::Bounded) => Some(ClaimLimitation::CoverageBounded),
        Some(CoverageStatus::Sampled) => Some(ClaimLimitation::CoverageSampled),
        Some(CoverageStatus::Unknown) | Some(_) | None => {
            Some(ClaimLimitation::CoverageIndeterminate)
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rootlight_query::QueryResource;

    use super::*;

    const CLAIMS: [ClaimKind; 5] = [
        ClaimKind::PositiveExistence,
        ClaimKind::NegativeExistence,
        ClaimKind::ExhaustiveEnumeration,
        ClaimKind::QuantitativeAggregate,
        ClaimKind::RankedRecommendation,
    ];

    #[derive(Debug, Clone, Copy)]
    enum ExecutionFixture {
        Complete,
        Truncated,
        UnsupportedPartial,
        Indeterminate,
    }

    impl ExecutionFixture {
        fn value(self) -> Option<ExecutionCompleteness> {
            match self {
                Self::Complete => Some(ExecutionCompleteness::complete()),
                Self::Truncated => Some(ExecutionCompleteness::truncated(
                    QueryResource::Results,
                    std::iter::empty(),
                )),
                Self::UnsupportedPartial => Some(ExecutionCompleteness::unsupported_partial(
                    QueryResource::Capability,
                    std::iter::empty(),
                )),
                Self::Indeterminate => None,
            }
        }
    }

    const EXECUTIONS: [ExecutionFixture; 4] = [
        ExecutionFixture::Complete,
        ExecutionFixture::Truncated,
        ExecutionFixture::UnsupportedPartial,
        ExecutionFixture::Indeterminate,
    ];

    const COVERAGES: [Option<CoverageStatus>; 5] = [
        Some(CoverageStatus::Complete),
        Some(CoverageStatus::Bounded),
        Some(CoverageStatus::Sampled),
        Some(CoverageStatus::Unknown),
        None,
    ];

    #[test]
    fn exhaustive_matrix_never_supports_incomplete_evidence() {
        for kind in CLAIMS {
            for execution_fixture in EXECUTIONS {
                let execution = execution_fixture.value();
                for coverage in COVERAGES {
                    let assessment = assess_claim(kind, execution.as_ref(), coverage);
                    let evidence_complete = matches!(execution_fixture, ExecutionFixture::Complete)
                        && coverage == Some(CoverageStatus::Complete);

                    if evidence_complete {
                        assert_eq!(assessment.disposition(), ClaimDisposition::Supported);
                        assert!(assessment.limitations().is_empty());
                    } else {
                        assert_ne!(assessment.disposition(), ClaimDisposition::Supported);
                        assert!(!assessment.limitations().is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn incomplete_evidence_only_qualifies_witness_and_ranked_claims() {
        let execution = ExecutionCompleteness::truncated(QueryResource::Depth, std::iter::empty());

        for kind in CLAIMS {
            let assessment = assess_claim(kind, Some(&execution), Some(CoverageStatus::Bounded));
            let expected = match kind {
                ClaimKind::PositiveExistence | ClaimKind::RankedRecommendation => {
                    ClaimDisposition::Qualified
                }
                ClaimKind::NegativeExistence
                | ClaimKind::ExhaustiveEnumeration
                | ClaimKind::QuantitativeAggregate => ClaimDisposition::Inconclusive,
            };
            assert_eq!(assessment.disposition(), expected);
        }
    }

    #[test]
    fn limitations_preserve_execution_before_coverage() {
        let execution = ExecutionCompleteness::unsupported_partial(
            QueryResource::Capability,
            std::iter::empty(),
        );

        let assessment = assess_claim(
            ClaimKind::PositiveExistence,
            Some(&execution),
            Some(CoverageStatus::Sampled),
        );

        assert_eq!(
            assessment.limitations(),
            &[
                ClaimLimitation::ExecutionUnsupportedPartial,
                ClaimLimitation::CoverageSampled,
            ]
        );
    }

    proptest! {
        #[test]
        fn arbitrary_incomplete_signal_never_yields_supported(
            claim_index in 0usize..CLAIMS.len(),
            execution_index in 0usize..EXECUTIONS.len(),
            coverage_index in 0usize..COVERAGES.len(),
        ) {
            prop_assume!(
                execution_index != 0 || coverage_index != 0,
                "at least one signal is incomplete"
            );
            let execution = EXECUTIONS[execution_index].value();
            let assessment = assess_claim(
                CLAIMS[claim_index],
                execution.as_ref(),
                COVERAGES[coverage_index],
            );

            prop_assert_ne!(assessment.disposition(), ClaimDisposition::Supported);
            prop_assert!(!assessment.limitations().is_empty());
        }
    }
}
