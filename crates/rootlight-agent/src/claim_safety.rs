//! Fail-closed policy for claims derived from analytical query results.
//!
//! Execution completeness and semantic coverage describe different failure
//! modes. This module evaluates both before an agent presents a result as a
//! claim, so missing or future states cannot silently become proof of complete
//! analysis.

use rootlight_ir::CoverageStatus;
use rootlight_mcp_contract::completeness::{
    CompletenessState as PublicCompletenessState, ResultCompleteness,
};

/// Transport-neutral execution state considered by claim-safety policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClaimExecutionState {
    /// Execution observed the complete admitted domain.
    Complete,
    /// A hard resource limit stopped execution.
    Truncated,
    /// Part of the requested semantics was explicitly unsupported.
    UnsupportedPartial,
    /// Execution completeness was absent or could not be interpreted.
    Indeterminate,
}

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
    execution: Option<ClaimExecutionState>,
    coverage: Option<CoverageStatus>,
) -> ClaimAssessment {
    assess(
        kind,
        execution_limitation(execution),
        coverage_limitation(coverage),
    )
}

/// Assesses a claim at the public response boundary.
///
/// This variant consumes the transport-neutral completeness contract after
/// daemon and client validation, so final response shaping applies the same
/// fail-closed policy as query-owned agent workflows.
#[must_use]
pub fn assess_public_claim(
    kind: ClaimKind,
    execution: Option<&ResultCompleteness>,
    coverage: Option<CoverageStatus>,
) -> ClaimAssessment {
    let execution_limitation = match execution.map(|value| value.state) {
        Some(PublicCompletenessState::Complete) => None,
        Some(PublicCompletenessState::Truncated) => Some(ClaimLimitation::ExecutionTruncated),
        Some(PublicCompletenessState::UnsupportedPartial) => {
            Some(ClaimLimitation::ExecutionUnsupportedPartial)
        }
        Some(PublicCompletenessState::Indeterminate) | None => {
            Some(ClaimLimitation::ExecutionIndeterminate)
        }
    };
    assess(kind, execution_limitation, coverage_limitation(coverage))
}

fn assess(
    kind: ClaimKind,
    execution_limitation: Option<ClaimLimitation>,
    coverage_limitation: Option<ClaimLimitation>,
) -> ClaimAssessment {
    let mut limitations = Vec::with_capacity(2);
    if let Some(limitation) = execution_limitation {
        limitations.push(limitation);
    }
    if let Some(limitation) = coverage_limitation {
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

fn execution_limitation(execution: Option<ClaimExecutionState>) -> Option<ClaimLimitation> {
    match execution {
        Some(ClaimExecutionState::Complete) => None,
        Some(ClaimExecutionState::Truncated) => Some(ClaimLimitation::ExecutionTruncated),
        Some(ClaimExecutionState::UnsupportedPartial) => {
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
    use super::*;
    use proptest::prelude::*;
    use rootlight_mcp_contract::completeness::{
        CompletenessState as PublicCompletenessState, ContinuationAvailability,
        ContinuationGuidance, LimitingResource, LimitingResourceKind, ResultCompleteness,
    };

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
        const fn value(self) -> Option<ClaimExecutionState> {
            match self {
                Self::Complete => Some(ClaimExecutionState::Complete),
                Self::Truncated => Some(ClaimExecutionState::Truncated),
                Self::UnsupportedPartial => Some(ClaimExecutionState::UnsupportedPartial),
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
                    let assessment = assess_claim(kind, execution, coverage);
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
        let execution = ClaimExecutionState::Truncated;

        for kind in CLAIMS {
            let assessment = assess_claim(kind, Some(execution), Some(CoverageStatus::Bounded));
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
        let execution = ClaimExecutionState::UnsupportedPartial;

        let assessment = assess_claim(
            ClaimKind::PositiveExistence,
            Some(execution),
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

    #[test]
    fn public_incomplete_execution_makes_negative_claims_inconclusive() {
        let execution = ResultCompleteness::new(
            PublicCompletenessState::Truncated,
            vec![LimitingResource::kind(LimitingResourceKind::Results)],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::NarrowScope],
        )
        .expect("public completeness is valid");

        let assessment = assess_public_claim(
            ClaimKind::NegativeExistence,
            Some(&execution),
            Some(CoverageStatus::Complete),
        );

        assert_eq!(assessment.disposition(), ClaimDisposition::Inconclusive);
        assert_eq!(
            assessment.limitations(),
            &[ClaimLimitation::ExecutionTruncated]
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
                execution,
                COVERAGES[coverage_index],
            );

            prop_assert_ne!(assessment.disposition(), ClaimDisposition::Supported);
            prop_assert!(!assessment.limitations().is_empty());
        }
    }
}
