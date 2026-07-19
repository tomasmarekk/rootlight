//! Source-free semantic conformance and quality evidence for M09.
//!
//! Reports consume the shared language registry and resolver decisions. The
//! evidence API deliberately exposes no repository path or command surface.

use std::collections::BTreeMap;

use rootlight_adapters::{ADAPTER_VERSION, LanguageAdapterRegistry, LanguageSemantics};
use rootlight_ir::{AnalysisTier, CoverageStatus};
use rootlight_resolve::{
    ExpectedResolution, MAX_CANDIDATE_LIMIT, MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS,
    ResolutionBatch, ResolutionExpectation, ResolutionOutcome, evaluate_resolution_quality,
};
use serde::Serialize;

const CORPUS_ID: &str = "rootlight-m09-semantic-conformance-v1";
const ADAPTER_CRATE: &str = "rootlight-adapters";
const MIN_EXACT_PRECISION_BASIS_POINTS: u16 = 9_500;
const MIN_EXACT_RECALL_BASIS_POINTS: u16 = 9_000;
const MIN_CANDIDATE_RECALL_BASIS_POINTS: u16 = 8_500;
const MAX_CALIBRATION_ERROR_MILLI: u16 = 50;
const P95_PERCENT: u64 = 95;
const PERCENT_SCALE: u64 = 100;
const MILLI_SCALE: u64 = 1_000;

/// Version of the machine-readable M09 evidence schema.
pub const M09_EVIDENCE_SCHEMA_VERSION: &str = "1.0";
/// Maximum canonical byte size emitted for one M09 report.
pub const M09_EVIDENCE_MAX_BYTES: usize = 64 * 1024;
/// Maximum reviewed expectations accepted by one M09 corpus.
pub const M09_EVIDENCE_MAX_EXPECTATIONS: usize = 65_536;

const EXPECTED_PROFILES: [ExpectedProfile; 4] = [
    ExpectedProfile::new(
        "go",
        AnalysisTier::TierA,
        LanguageSemantics::Static,
        &["build_tags", "code_generation", "runtime_registration"],
    ),
    ExpectedProfile::new(
        "python",
        AnalysisTier::TierB,
        LanguageSemantics::Dynamic,
        &[
            "dynamic_attributes",
            "dynamic_imports",
            "monkey_patching",
            "reflection",
        ],
    ),
    ExpectedProfile::new(
        "rust",
        AnalysisTier::TierA,
        LanguageSemantics::Static,
        &["generated_code", "macro_expansion", "procedural_macros"],
    ),
    ExpectedProfile::new(
        "typescript",
        AnalysisTier::TierA,
        LanguageSemantics::Static,
        &["dynamic_imports", "generated_code", "runtime_registration"],
    ),
];

/// Opaque, source-free M09 conformance and quality report.
///
/// Use [`encode_m09_semantic_evidence`] to obtain the canonical JSON artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct M09SemanticEvidence {
    schema_version: String,
    corpus_id: String,
    adapter_crate: String,
    languages: Vec<LanguageEvidence>,
    resolver_quality: ResolverQualityEvidence,
    repository_execution: RepositoryExecutionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LanguageEvidence {
    language: String,
    maximum_tier: TierEvidence,
    semantics: SemanticsEvidence,
    adapter_version: String,
    context_import: String,
    required_coverage_domains: u8,
    uncertainty_codes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TierEvidence {
    TierA,
    TierB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SemanticsEvidence {
    Static,
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResolverQualityEvidence {
    expectations: u64,
    exact_expected: u64,
    ambiguous_expected: u64,
    unresolved_expected: u64,
    exact_outcomes: u64,
    candidate_outcomes: u64,
    unresolved_outcomes: u64,
    exact_precision_basis_points: u16,
    exact_recall_basis_points: u16,
    candidate_recall_basis_points: u16,
    high_confidence_precision_basis_points: u16,
    ambiguous_hidden_exact: u64,
    unresolved_correct: u64,
    unexpected_decisions: u64,
    unresolved_rate_basis_points: u16,
    mean_candidate_set_size_milli: u64,
    p95_candidate_set_size: u64,
    max_candidate_set_size: u64,
    calibration_samples: u64,
    expected_calibration_error_milli: u16,
    calibration_bins: Vec<CalibrationBinEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CalibrationBinEvidence {
    index: u8,
    samples: u64,
    mean_confidence_milli: u16,
    observed_accuracy_milli: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RepositoryExecutionEvidence {
    command_surface_available: bool,
    observed_command_attempts: u64,
    denied_operations: [RepositoryOperation; 6],
    network_egress_available: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RepositoryOperation {
    PackageManager,
    BuildScript,
    TestCommand,
    Generator,
    ProceduralMacro,
    RepositoryBinary,
}

/// Invalid or oversized M09 semantic evidence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum M09EvidenceError {
    /// The shared registry differs from the reviewed four-language profile.
    #[error("M09 language conformance evidence is invalid")]
    InvalidLanguageEvidence,
    /// Resolver results fail the ambiguity-sensitive quality contract.
    #[error("M09 resolver quality evidence is invalid")]
    InvalidQualityEvidence,
    /// The resolver quality corpus was internally inconsistent.
    #[error("M09 resolver quality evaluation failed")]
    Quality(#[source] rootlight_resolve::QualityError),
    /// A fixed input or output ceiling was exceeded.
    #[error("M09 semantic evidence limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// Canonical JSON serialization failed.
    #[error("M09 semantic evidence encoding failed")]
    Encode,
}

/// Builds evidence from the shared profile registry and reviewed resolver decisions.
///
/// The function accepts no repository path, environment, process, or command
/// input. Its report records that structural no-execution restriction.
///
/// # Errors
///
/// Returns [`M09EvidenceError`] for a changed profile contract, an oversized
/// corpus, inconsistent ground truth, hidden ambiguity, missing explicit
/// outcomes, or quality and calibration below the M09 floors.
pub fn build_m09_semantic_evidence(
    registry: &LanguageAdapterRegistry,
    batch: &ResolutionBatch,
    expectations: &[ResolutionExpectation],
) -> Result<M09SemanticEvidence, M09EvidenceError> {
    if expectations.len() > M09_EVIDENCE_MAX_EXPECTATIONS {
        return Err(M09EvidenceError::LimitExceeded {
            resource: "expectation_count",
        });
    }
    if batch.decisions.len() > M09_EVIDENCE_MAX_EXPECTATIONS {
        return Err(M09EvidenceError::LimitExceeded {
            resource: "decision_count",
        });
    }
    if batch.decisions.iter().any(|decision| {
        matches!(
            &decision.outcome,
            ResolutionOutcome::Candidates { symbols, .. }
                if symbols.len() > MAX_CANDIDATE_LIMIT
        )
    }) {
        return Err(M09EvidenceError::LimitExceeded {
            resource: "candidate_count",
        });
    }
    Ok(M09SemanticEvidence {
        schema_version: M09_EVIDENCE_SCHEMA_VERSION.to_owned(),
        corpus_id: CORPUS_ID.to_owned(),
        adapter_crate: ADAPTER_CRATE.to_owned(),
        languages: language_evidence(registry)?,
        resolver_quality: quality_evidence(batch, expectations)?,
        repository_execution: RepositoryExecutionEvidence {
            command_surface_available: false,
            observed_command_attempts: 0,
            denied_operations: [
                RepositoryOperation::PackageManager,
                RepositoryOperation::BuildScript,
                RepositoryOperation::TestCommand,
                RepositoryOperation::Generator,
                RepositoryOperation::ProceduralMacro,
                RepositoryOperation::RepositoryBinary,
            ],
            network_egress_available: false,
        },
    })
}

/// Encodes a report as deterministic compact JSON under a fixed byte ceiling.
///
/// # Errors
///
/// Returns [`M09EvidenceError`] when serialization fails or the report exceeds
/// [`M09_EVIDENCE_MAX_BYTES`].
pub fn encode_m09_semantic_evidence(
    evidence: &M09SemanticEvidence,
) -> Result<Vec<u8>, M09EvidenceError> {
    let encoded = serde_json::to_vec(evidence).map_err(|_| M09EvidenceError::Encode)?;
    if encoded.len() > M09_EVIDENCE_MAX_BYTES {
        return Err(M09EvidenceError::LimitExceeded {
            resource: "encoded_bytes",
        });
    }
    Ok(encoded)
}

fn language_evidence(
    registry: &LanguageAdapterRegistry,
) -> Result<Vec<LanguageEvidence>, M09EvidenceError> {
    EXPECTED_PROFILES
        .into_iter()
        .map(|expected| {
            let profile = registry
                .iter()
                .find(|profile| profile.language().as_str() == expected.language)
                .ok_or(M09EvidenceError::InvalidLanguageEvidence)?;
            if profile.maximum_tier() != expected.tier || profile.semantics() != expected.semantics
            {
                return Err(M09EvidenceError::InvalidLanguageEvidence);
            }
            Ok(LanguageEvidence {
                language: expected.language.to_owned(),
                maximum_tier: tier_evidence(expected.tier)?,
                semantics: semantics_evidence(expected.semantics)?,
                adapter_version: ADAPTER_VERSION.to_owned(),
                context_import: "validated_declarative_normalized_ir".to_owned(),
                required_coverage_domains: 8,
                uncertainty_codes: expected
                    .uncertainty_codes
                    .iter()
                    .map(|code| (*code).to_owned())
                    .collect(),
            })
        })
        .collect()
}

fn quality_evidence(
    batch: &ResolutionBatch,
    expectations: &[ResolutionExpectation],
) -> Result<ResolverQualityEvidence, M09EvidenceError> {
    let report =
        evaluate_resolution_quality(batch, expectations).map_err(M09EvidenceError::Quality)?;
    let expectations_by_occurrence = expectations
        .iter()
        .map(|expectation| (expectation.occurrence, expectation.expected))
        .collect::<BTreeMap<_, _>>();
    if expectations_by_occurrence.len() != expectations.len() {
        return Err(M09EvidenceError::InvalidQualityEvidence);
    }

    let (mut exact_expected, mut ambiguous_expected, mut unresolved_expected) = (0, 0, 0);
    for expectation in expectations {
        increment(match expectation.expected {
            ExpectedResolution::Exact(_) => &mut exact_expected,
            ExpectedResolution::CandidateContains(_) => &mut ambiguous_expected,
            ExpectedResolution::Unresolved => &mut unresolved_expected,
        })?;
    }

    let (mut exact_outcomes, mut candidate_outcomes, mut unresolved_outcomes) = (0, 0, 0);
    let (mut high_confidence_correct, mut high_confidence_total) = (0, 0);
    let mut candidate_sizes = Vec::new();
    for decision in &batch.decisions {
        match &decision.outcome {
            ResolutionOutcome::Resolved { symbol, confidence } => {
                increment(&mut exact_outcomes)?;
                if confidence.get() >= 900 {
                    increment(&mut high_confidence_total)?;
                    if matches!(
                        expectations_by_occurrence.get(&decision.occurrence),
                        Some(ExpectedResolution::Exact(expected)) if expected == symbol
                    ) {
                        increment(&mut high_confidence_correct)?;
                    }
                }
            }
            ResolutionOutcome::Candidates {
                symbols,
                total_count,
                completeness,
                ..
            } => {
                increment(&mut candidate_outcomes)?;
                let materialized = u64::try_from(symbols.len())
                    .map_err(|_| M09EvidenceError::InvalidQualityEvidence)?;
                if *total_count < materialized
                    || (*completeness == CoverageStatus::Complete && *total_count != materialized)
                {
                    return Err(M09EvidenceError::InvalidQualityEvidence);
                }
                candidate_sizes.push(*total_count);
            }
            ResolutionOutcome::Unresolved { .. } => increment(&mut unresolved_outcomes)?,
        }
    }
    candidate_sizes.sort_unstable();

    let exact_precision = ratio(report.exact_precision.basis_points())?;
    let exact_recall = ratio(report.exact_recall.basis_points())?;
    let candidate_recall = ratio(report.candidate_recall.basis_points())?;
    let high_confidence_precision = basis_points(high_confidence_correct, high_confidence_total)?;
    let unresolved_rate = basis_points(unresolved_outcomes, report.expectations)?;
    let (mean_candidates, p95_candidates, max_candidates) = candidate_metrics(&candidate_sizes)?;
    let calibration_error = report
        .calibration
        .expected_calibration_error
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;

    let expected_total = exact_expected
        .checked_add(ambiguous_expected)
        .and_then(|total| total.checked_add(unresolved_expected))
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    let outcome_total = exact_outcomes
        .checked_add(candidate_outcomes)
        .and_then(|total| total.checked_add(unresolved_outcomes))
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    if exact_expected == 0
        || ambiguous_expected == 0
        || unresolved_expected == 0
        || exact_outcomes == 0
        || candidate_outcomes == 0
        || unresolved_outcomes == 0
        || expected_total != report.expectations
        || outcome_total != report.expectations
        || exact_precision < MIN_EXACT_PRECISION_BASIS_POINTS
        || exact_recall < MIN_EXACT_RECALL_BASIS_POINTS
        || candidate_recall < MIN_CANDIDATE_RECALL_BASIS_POINTS
        || high_confidence_precision < MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS
        || report.ambiguous_hidden_exact != 0
        || report.unresolved_correct != unresolved_expected
        || report.unexpected_decisions != 0
        || calibration_error > MAX_CALIBRATION_ERROR_MILLI
    {
        return Err(M09EvidenceError::InvalidQualityEvidence);
    }

    Ok(ResolverQualityEvidence {
        expectations: report.expectations,
        exact_expected,
        ambiguous_expected,
        unresolved_expected,
        exact_outcomes,
        candidate_outcomes,
        unresolved_outcomes,
        exact_precision_basis_points: exact_precision,
        exact_recall_basis_points: exact_recall,
        candidate_recall_basis_points: candidate_recall,
        high_confidence_precision_basis_points: high_confidence_precision,
        ambiguous_hidden_exact: report.ambiguous_hidden_exact,
        unresolved_correct: report.unresolved_correct,
        unexpected_decisions: report.unexpected_decisions,
        unresolved_rate_basis_points: unresolved_rate,
        mean_candidate_set_size_milli: mean_candidates,
        p95_candidate_set_size: p95_candidates,
        max_candidate_set_size: max_candidates,
        calibration_samples: report.calibration.samples,
        expected_calibration_error_milli: calibration_error,
        calibration_bins: report
            .calibration
            .bins
            .into_iter()
            .map(|bin| CalibrationBinEvidence {
                index: bin.index,
                samples: bin.samples,
                mean_confidence_milli: bin.mean_confidence,
                observed_accuracy_milli: bin.observed_accuracy,
            })
            .collect(),
    })
}

fn candidate_metrics(sizes: &[u64]) -> Result<(u64, u64, u64), M09EvidenceError> {
    let count = u64::try_from(sizes.len()).map_err(|_| M09EvidenceError::InvalidQualityEvidence)?;
    let total = sizes.iter().try_fold(0_u64, |sum, size| {
        sum.checked_add(*size)
            .ok_or(M09EvidenceError::InvalidQualityEvidence)
    })?;
    let mean = total
        .checked_mul(MILLI_SCALE)
        .and_then(|scaled| scaled.checked_div(count))
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    let rank = count
        .checked_mul(P95_PERCENT)
        .and_then(|scaled| scaled.checked_add(PERCENT_SCALE - 1))
        .and_then(|scaled| scaled.checked_div(PERCENT_SCALE))
        .and_then(|rank| rank.checked_sub(1))
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    let index = usize::try_from(rank).map_err(|_| M09EvidenceError::InvalidQualityEvidence)?;
    let p95 = *sizes
        .get(index)
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    let maximum = *sizes
        .last()
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    Ok((mean, p95, maximum))
}

fn basis_points(numerator: u64, denominator: u64) -> Result<u16, M09EvidenceError> {
    let scaled = u128::from(numerator)
        .checked_mul(10_000)
        .and_then(|value| value.checked_div(u128::from(denominator)))
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    u16::try_from(scaled).map_err(|_| M09EvidenceError::InvalidQualityEvidence)
}

fn ratio(value: Option<u16>) -> Result<u16, M09EvidenceError> {
    value.ok_or(M09EvidenceError::InvalidQualityEvidence)
}

fn increment(value: &mut u64) -> Result<(), M09EvidenceError> {
    *value = value
        .checked_add(1)
        .ok_or(M09EvidenceError::InvalidQualityEvidence)?;
    Ok(())
}

fn tier_evidence(tier: AnalysisTier) -> Result<TierEvidence, M09EvidenceError> {
    match tier {
        AnalysisTier::TierA => Ok(TierEvidence::TierA),
        AnalysisTier::TierB => Ok(TierEvidence::TierB),
        AnalysisTier::TierC | AnalysisTier::TierD => Err(M09EvidenceError::InvalidLanguageEvidence),
        _ => Err(M09EvidenceError::InvalidLanguageEvidence),
    }
}

fn semantics_evidence(semantics: LanguageSemantics) -> Result<SemanticsEvidence, M09EvidenceError> {
    match semantics {
        LanguageSemantics::Static => Ok(SemanticsEvidence::Static),
        LanguageSemantics::Dynamic => Ok(SemanticsEvidence::Dynamic),
        _ => Err(M09EvidenceError::InvalidLanguageEvidence),
    }
}

#[derive(Debug, Clone, Copy)]
struct ExpectedProfile {
    language: &'static str,
    tier: AnalysisTier,
    semantics: LanguageSemantics,
    uncertainty_codes: &'static [&'static str],
}

impl ExpectedProfile {
    const fn new(
        language: &'static str,
        tier: AnalysisTier,
        semantics: LanguageSemantics,
        uncertainty_codes: &'static [&'static str],
    ) -> Self {
        Self {
            language,
            tier,
            semantics,
            uncertainty_codes,
        }
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapters::initial_semantic_registry;
    use rootlight_ids::{FactId, GenerationId, RepositoryId, SymbolId};
    use rootlight_ir::{Confidence, CoverageStatus};
    use rootlight_resolve::{
        RESOLVER_PROVIDER_NAME, RESOLVER_PROVIDER_VERSION, ResolutionDecision,
        ResolutionExplanation, ResolutionRule, UnresolvedReason,
    };

    use super::*;

    #[test]
    fn report_is_deterministic_bounded_and_source_free() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (batch, expectations) = corpus();
        let first = build_m09_semantic_evidence(&registry, &batch, &expectations)
            .expect("reference corpus passes");
        let repeated = build_m09_semantic_evidence(&registry, &batch, &expectations)
            .expect("reference corpus repeats");
        let first_bytes = encode_m09_semantic_evidence(&first).expect("evidence encodes");
        let repeated_bytes =
            encode_m09_semantic_evidence(&repeated).expect("repeated evidence encodes");

        assert_eq!(first_bytes, repeated_bytes);
        assert_eq!(first.languages.len(), 4);
        assert_eq!(first.resolver_quality.exact_outcomes, 1);
        assert_eq!(first.resolver_quality.candidate_outcomes, 1);
        assert_eq!(first.resolver_quality.unresolved_outcomes, 1);
        assert_eq!(first.resolver_quality.ambiguous_hidden_exact, 0);
        assert_eq!(first.resolver_quality.expected_calibration_error_milli, 50);
        assert!(!first.repository_execution.command_surface_available);
        assert_eq!(first.repository_execution.observed_command_attempts, 0);
        let text = String::from_utf8(first_bytes).expect("canonical evidence is UTF-8");
        assert!(!text.contains("fn target"));
        assert!(!text.contains("\\Users\\"));
        assert!(!text.contains("/home/"));
    }

    #[test]
    fn report_rejects_hidden_ambiguity_and_missing_profiles() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (mut batch, expectations) = corpus();
        batch.decisions[1].outcome = ResolutionOutcome::Resolved {
            symbol: SymbolId::from_bytes([12; 20]),
            confidence: Confidence::new(900).expect("fixture confidence is valid"),
        };
        assert!(matches!(
            build_m09_semantic_evidence(&registry, &batch, &expectations),
            Err(M09EvidenceError::InvalidQualityEvidence)
        ));

        let incomplete = LanguageAdapterRegistry::new(
            registry
                .iter()
                .filter(|profile| profile.language().as_str() != "go")
                .cloned()
                .collect(),
        )
        .expect("remaining profiles form a valid registry");
        let (batch, expectations) = corpus();
        assert!(matches!(
            build_m09_semantic_evidence(&incomplete, &batch, &expectations),
            Err(M09EvidenceError::InvalidLanguageEvidence)
        ));
    }

    #[test]
    fn expectation_count_is_bounded_before_quality_evaluation() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (mut batch, expectations) = corpus();
        let oversized = vec![expectations[0]; M09_EVIDENCE_MAX_EXPECTATIONS + 1];
        assert!(matches!(
            build_m09_semantic_evidence(&registry, &batch, &oversized),
            Err(M09EvidenceError::LimitExceeded {
                resource: "expectation_count"
            })
        ));

        let ResolutionOutcome::Candidates { symbols, .. } = &mut batch.decisions[1].outcome else {
            panic!("fixture contains a candidate decision");
        };
        *symbols = vec![SymbolId::from_bytes([14; 20]); MAX_CANDIDATE_LIMIT + 1];
        assert!(matches!(
            build_m09_semantic_evidence(&registry, &batch, &expectations),
            Err(M09EvidenceError::LimitExceeded {
                resource: "candidate_count"
            })
        ));
    }

    fn corpus() -> (ResolutionBatch, Vec<ResolutionExpectation>) {
        let exact_occurrence = FactId::from_bytes([1; 20]);
        let ambiguous_occurrence = FactId::from_bytes([2; 20]);
        let unresolved_occurrence = FactId::from_bytes([3; 20]);
        let exact_symbol = SymbolId::from_bytes([11; 20]);
        let candidate_symbol = SymbolId::from_bytes([12; 20]);
        let decisions = vec![
            decision(
                exact_occurrence,
                ResolutionOutcome::Resolved {
                    symbol: exact_symbol,
                    confidence: Confidence::new(1_000).expect("fixture confidence is valid"),
                },
            ),
            decision(
                ambiguous_occurrence,
                ResolutionOutcome::Candidates {
                    symbols: vec![candidate_symbol, SymbolId::from_bytes([13; 20])],
                    total_count: 2,
                    completeness: CoverageStatus::Complete,
                    confidence: Confidence::new(900).expect("fixture confidence is valid"),
                },
            ),
            decision(
                unresolved_occurrence,
                ResolutionOutcome::Unresolved {
                    reason: UnresolvedReason::NoCandidate,
                    confidence: Confidence::new(0).expect("fixture confidence is valid"),
                },
            ),
        ];
        let expectations = vec![
            ResolutionExpectation {
                occurrence: exact_occurrence,
                expected: ExpectedResolution::Exact(exact_symbol),
            },
            ResolutionExpectation {
                occurrence: ambiguous_occurrence,
                expected: ExpectedResolution::CandidateContains(candidate_symbol),
            },
            ResolutionExpectation {
                occurrence: unresolved_occurrence,
                expected: ExpectedResolution::Unresolved,
            },
        ];
        (
            ResolutionBatch {
                repository: RepositoryId::from_bytes([21; 16]),
                generation: GenerationId::from_bytes([22; 20]),
                decisions,
            },
            expectations,
        )
    }

    fn decision(occurrence: FactId, outcome: ResolutionOutcome) -> ResolutionDecision {
        ResolutionDecision {
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
        }
    }
}
