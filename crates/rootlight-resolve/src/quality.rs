//! Deterministic semantic quality and calibration scorecards.
//!
//! Reports separate unique-edge precision from candidate recall so an
//! ambiguity-preserving result cannot be rewarded as an exact prediction.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{FactId, SymbolId};

use crate::{ResolutionBatch, ResolutionOutcome};

const CALIBRATION_BIN_COUNT: usize = 10;
const BASIS_POINTS_SCALE: u128 = 10_000;
const CONFIDENCE_SCALE: u64 = 1_000;

/// Ground-truth outcome for one occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedResolution {
    /// Exactly one semantic target is correct.
    Exact(SymbolId),
    /// The target must be present, but uniqueness is not established.
    CandidateContains(SymbolId),
    /// No supported target should be claimed.
    Unresolved,
}

/// One occurrence and its reviewed semantic ground truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionExpectation {
    /// Occurrence evaluated by the corpus.
    pub occurrence: FactId,
    /// Reviewed exact, candidate, or unresolved outcome.
    pub expected: ExpectedResolution,
}

/// Integer ratio preserving its numerator and denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityRatio {
    /// Correct or matched predictions.
    pub numerator: u64,
    /// Eligible predictions or expectations.
    pub denominator: u64,
}

impl QualityRatio {
    /// Returns the ratio on a 0-through-10,000 basis-point scale.
    ///
    /// Returns `None` when the denominator is zero.
    #[must_use]
    pub fn basis_points(self) -> Option<u16> {
        if self.denominator == 0 {
            return None;
        }
        let scaled = u128::from(self.numerator) * BASIS_POINTS_SCALE / u128::from(self.denominator);
        u16::try_from(scaled).ok()
    }
}

/// One non-empty fixed-width confidence bin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalibrationBin {
    /// Zero-based bin number; bin 9 includes confidence 1000.
    pub index: u8,
    /// Number of semantic predictions in the bin.
    pub samples: u64,
    /// Mean predicted confidence on the 0-through-1000 scale.
    pub mean_confidence: u16,
    /// Observed correctness on the 0-through-1000 scale.
    pub observed_accuracy: u16,
}

/// Fixed-width expected-calibration-error report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalibrationReport {
    /// Number of exact or candidate predictions evaluated.
    pub samples: u64,
    /// Expected calibration error on the 0-through-1000 scale.
    pub expected_calibration_error: Option<u16>,
    /// Non-empty bins in ascending confidence order.
    pub bins: Vec<CalibrationBin>,
}

/// Resolution scorecard with ambiguity-sensitive metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionQualityReport {
    /// Reviewed expectations in the corpus.
    pub expectations: u64,
    /// Correct unique edges divided by all predicted unique edges.
    pub exact_precision: QualityRatio,
    /// Correct unique edges divided by all expected unique edges.
    pub exact_recall: QualityRatio,
    /// Ground-truth targets present in exact or candidate outputs.
    pub candidate_recall: QualityRatio,
    /// Ambiguous expectations incorrectly collapsed into exact results.
    pub ambiguous_hidden_exact: u64,
    /// Correct unresolved outcomes.
    pub unresolved_correct: u64,
    /// Reviewed unresolved expectations.
    pub unresolved_expected: u64,
    /// Resolver decisions without a corresponding corpus expectation.
    pub unexpected_decisions: u64,
    /// Confidence calibration for semantic target predictions.
    pub calibration: CalibrationReport,
}

/// Semantic quality corpus or arithmetic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QualityError {
    /// More than one expectation named the same occurrence.
    #[error("resolution quality corpus contains a duplicate expectation")]
    DuplicateExpectation,
    /// More than one decision named the same occurrence.
    #[error("resolution batch contains a duplicate decision")]
    DuplicateDecision,
    /// A scorecard counter exceeded its fixed representation.
    #[error("resolution quality counter overflowed")]
    CountOverflow,
}

/// Evaluates one resolution batch against reviewed ground truth.
///
/// Unique-edge precision, exact recall, candidate recall, unresolved accuracy,
/// ambiguity collapse, and fixed-width calibration remain separate outputs.
///
/// # Errors
///
/// Returns [`QualityError`] for duplicate occurrence IDs or counter overflow.
pub fn evaluate_resolution_quality(
    batch: &ResolutionBatch,
    expectations: &[ResolutionExpectation],
) -> Result<ResolutionQualityReport, QualityError> {
    let mut decisions = BTreeMap::new();
    for decision in &batch.decisions {
        if decisions.insert(decision.occurrence, decision).is_some() {
            return Err(QualityError::DuplicateDecision);
        }
    }
    let mut expected_occurrences = BTreeSet::new();
    for expectation in expectations {
        if !expected_occurrences.insert(expectation.occurrence) {
            return Err(QualityError::DuplicateExpectation);
        }
    }

    let mut exact_predicted = 0_u64;
    let mut exact_expected = 0_u64;
    let mut exact_correct = 0_u64;
    let mut target_expected = 0_u64;
    let mut target_present = 0_u64;
    let mut ambiguous_hidden_exact = 0_u64;
    let mut unresolved_expected = 0_u64;
    let mut unresolved_correct = 0_u64;
    let mut calibration = CalibrationAccumulator::default();

    for expectation in expectations {
        let outcome = decisions
            .get(&expectation.occurrence)
            .map(|decision| &decision.outcome);
        if matches!(outcome, Some(ResolutionOutcome::Resolved { .. })) {
            checked_increment(&mut exact_predicted)?;
        }

        match expectation.expected {
            ExpectedResolution::Exact(target) => {
                checked_increment(&mut exact_expected)?;
                checked_increment(&mut target_expected)?;
                let exact_match = matches!(
                    outcome,
                    Some(ResolutionOutcome::Resolved { symbol, .. }) if *symbol == target
                );
                if exact_match {
                    checked_increment(&mut exact_correct)?;
                }
                let present = target_is_present(outcome, target);
                if present {
                    checked_increment(&mut target_present)?;
                }
                calibration.observe(outcome, present)?;
            }
            ExpectedResolution::CandidateContains(target) => {
                checked_increment(&mut target_expected)?;
                if matches!(outcome, Some(ResolutionOutcome::Resolved { .. })) {
                    checked_increment(&mut ambiguous_hidden_exact)?;
                }
                let present = target_is_present(outcome, target);
                if present {
                    checked_increment(&mut target_present)?;
                }
                let ambiguity_preserved = matches!(
                    outcome,
                    Some(ResolutionOutcome::Candidates { symbols, .. })
                        if symbols.contains(&target)
                );
                calibration.observe(outcome, ambiguity_preserved)?;
            }
            ExpectedResolution::Unresolved => {
                checked_increment(&mut unresolved_expected)?;
                if outcome.is_none()
                    || matches!(outcome, Some(ResolutionOutcome::Unresolved { .. }))
                {
                    checked_increment(&mut unresolved_correct)?;
                }
                calibration.observe(outcome, false)?;
            }
        }
    }

    let unexpected_decisions = u64::try_from(
        decisions
            .keys()
            .filter(|occurrence| !expected_occurrences.contains(occurrence))
            .count(),
    )
    .map_err(|_| QualityError::CountOverflow)?;
    let expectations =
        u64::try_from(expectations.len()).map_err(|_| QualityError::CountOverflow)?;

    Ok(ResolutionQualityReport {
        expectations,
        exact_precision: QualityRatio {
            numerator: exact_correct,
            denominator: exact_predicted,
        },
        exact_recall: QualityRatio {
            numerator: exact_correct,
            denominator: exact_expected,
        },
        candidate_recall: QualityRatio {
            numerator: target_present,
            denominator: target_expected,
        },
        ambiguous_hidden_exact,
        unresolved_correct,
        unresolved_expected,
        unexpected_decisions,
        calibration: calibration.finish()?,
    })
}

fn target_is_present(outcome: Option<&ResolutionOutcome>, target: SymbolId) -> bool {
    match outcome {
        Some(ResolutionOutcome::Resolved { symbol, .. }) => *symbol == target,
        Some(ResolutionOutcome::Candidates { symbols, .. }) => symbols.contains(&target),
        Some(ResolutionOutcome::Unresolved { .. }) | None => false,
    }
}

fn checked_increment(counter: &mut u64) -> Result<(), QualityError> {
    *counter = counter.checked_add(1).ok_or(QualityError::CountOverflow)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default)]
struct CalibrationAccumulator {
    bins: [CalibrationBinAccumulator; CALIBRATION_BIN_COUNT],
}

impl CalibrationAccumulator {
    fn observe(
        &mut self,
        outcome: Option<&ResolutionOutcome>,
        correct: bool,
    ) -> Result<(), QualityError> {
        let confidence = match outcome {
            Some(ResolutionOutcome::Resolved { confidence, .. })
            | Some(ResolutionOutcome::Candidates { confidence, .. }) => confidence.get(),
            Some(ResolutionOutcome::Unresolved { .. }) | None => return Ok(()),
        };
        let index = usize::from((confidence / 100).min(9));
        let bin = &mut self.bins[index];
        checked_increment(&mut bin.samples)?;
        bin.confidence_sum = bin
            .confidence_sum
            .checked_add(u64::from(confidence))
            .ok_or(QualityError::CountOverflow)?;
        if correct {
            checked_increment(&mut bin.correct)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<CalibrationReport, QualityError> {
        let mut samples = 0_u64;
        let mut weighted_gap = 0_u64;
        let mut bins = Vec::new();
        for (index, accumulator) in self.bins.into_iter().enumerate() {
            if accumulator.samples == 0 {
                continue;
            }
            samples = samples
                .checked_add(accumulator.samples)
                .ok_or(QualityError::CountOverflow)?;
            let mean_confidence = accumulator.confidence_sum / accumulator.samples;
            let observed_accuracy = accumulator
                .correct
                .checked_mul(CONFIDENCE_SCALE)
                .ok_or(QualityError::CountOverflow)?
                / accumulator.samples;
            let gap = mean_confidence.abs_diff(observed_accuracy);
            weighted_gap = weighted_gap
                .checked_add(
                    gap.checked_mul(accumulator.samples)
                        .ok_or(QualityError::CountOverflow)?,
                )
                .ok_or(QualityError::CountOverflow)?;
            bins.push(CalibrationBin {
                index: u8::try_from(index).map_err(|_| QualityError::CountOverflow)?,
                samples: accumulator.samples,
                mean_confidence: u16::try_from(mean_confidence)
                    .map_err(|_| QualityError::CountOverflow)?,
                observed_accuracy: u16::try_from(observed_accuracy)
                    .map_err(|_| QualityError::CountOverflow)?,
            });
        }
        let expected_calibration_error = weighted_gap
            .checked_div(samples)
            .map(u16::try_from)
            .transpose()
            .map_err(|_| QualityError::CountOverflow)?;
        Ok(CalibrationReport {
            samples,
            expected_calibration_error,
            bins,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CalibrationBinAccumulator {
    samples: u64,
    confidence_sum: u64,
    correct: u64,
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::{Confidence, CoverageStatus};

    use super::*;
    use crate::{
        RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
        ResolutionExplanation, ResolutionRule,
    };

    #[test]
    fn ambiguous_ground_truth_rejects_a_hidden_exact_edge() {
        let occurrence = FactId::from_bytes([1; 20]);
        let symbol = SymbolId::from_bytes([2; 20]);
        let batch = batch_with(
            occurrence,
            ResolutionOutcome::Resolved {
                symbol,
                confidence: Confidence::new(900).expect("fixture confidence is valid"),
            },
        );

        let report = evaluate_resolution_quality(
            &batch,
            &[ResolutionExpectation {
                occurrence,
                expected: ExpectedResolution::CandidateContains(symbol),
            }],
        )
        .expect("fixture scorecard evaluates");

        assert_eq!(report.ambiguous_hidden_exact, 1);
        assert_eq!(report.exact_precision.basis_points(), Some(0));
        assert_eq!(report.candidate_recall.basis_points(), Some(10_000));
        assert_eq!(report.calibration.expected_calibration_error, Some(900));
    }

    #[test]
    fn candidate_ground_truth_scores_candidate_presence() {
        let occurrence = FactId::from_bytes([3; 20]);
        let symbol = SymbolId::from_bytes([4; 20]);
        let batch = batch_with(
            occurrence,
            ResolutionOutcome::Candidates {
                symbols: vec![symbol],
                total_count: 1,
                completeness: CoverageStatus::Complete,
                confidence: Confidence::new(700).expect("fixture confidence is valid"),
            },
        );

        let report = evaluate_resolution_quality(
            &batch,
            &[ResolutionExpectation {
                occurrence,
                expected: ExpectedResolution::CandidateContains(symbol),
            }],
        )
        .expect("fixture scorecard evaluates");

        assert_eq!(report.candidate_recall.basis_points(), Some(10_000));
        assert_eq!(report.calibration.expected_calibration_error, Some(300));
    }

    fn batch_with(occurrence: FactId, outcome: ResolutionOutcome) -> ResolutionBatch {
        ResolutionBatch {
            repository: RepositoryId::from_bytes([5; 16]),
            generation: GenerationId::from_bytes([6; 20]),
            decisions: vec![ResolutionDecision {
                occurrence,
                outcome,
                explanation: ResolutionExplanation {
                    rule: ResolutionRule::LexicalScope,
                    provider_name: RESOLVER_PROVIDER_NAME,
                    provider_version: RESOLVER_PROVIDER_VERSION,
                    candidates: Vec::new(),
                    rejected_candidates: Vec::new(),
                    rejected_total: 0,
                    completeness_assumptions: Vec::new(),
                },
            }],
        }
    }
}
