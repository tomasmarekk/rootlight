//! Source-free semantic contract conformance evidence.
//!
//! Reports consume the shared language registry and caller-supplied resolver
//! decisions. Contract fixtures cannot authorize tier promotion or completion.

use std::collections::BTreeMap;

use rootlight_adapters::{ADAPTER_VERSION, LanguageAdapterRegistry, LanguageSemantics};
use rootlight_ir::{AnalysisTier, CoverageStatus};
use rootlight_resolve::{
    ExpectedResolution, MAX_CANDIDATE_LIMIT, MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS,
    ResolutionBatch, ResolutionExpectation, ResolutionOutcome, evaluate_resolution_quality,
};
use serde::Serialize;

const CORPUS_ID: &str = "rootlight-semantic-contract-fixture-v1";
const ADAPTER_CRATE: &str = "rootlight-adapters";
const MIN_EXACT_PRECISION_BASIS_POINTS: u16 = 9_500;
const MIN_EXACT_RECALL_BASIS_POINTS: u16 = 9_000;
const MIN_CANDIDATE_RECALL_BASIS_POINTS: u16 = 8_500;
const MAX_CALIBRATION_ERROR_MILLI: u16 = 50;
const P95_PERCENT: u64 = 95;
const PERCENT_SCALE: u64 = 100;
const MILLI_SCALE: u64 = 1_000;

/// Version of the machine-readable semantic contract evidence schema.
pub const SEMANTIC_EVIDENCE_SCHEMA_VERSION: &str = "1.1";
/// Version of the source-revision-bound semantic evidence envelope.
pub const SEMANTIC_EVIDENCE_ENVELOPE_SCHEMA: &str = "rootlight.semantic-evidence-envelope/1";
/// Maximum canonical byte size emitted for one semantic contract report.
pub const SEMANTIC_EVIDENCE_MAX_BYTES: usize = 64 * 1024;
/// Maximum canonical byte size emitted for one source-bound envelope.
pub const SEMANTIC_EVIDENCE_ENVELOPE_MAX_BYTES: usize = 128 * 1024;
/// Maximum reviewed expectations accepted by one semantic contract corpus.
pub const SEMANTIC_EVIDENCE_MAX_EXPECTATIONS: usize = 65_536;

const EXPECTED_PROFILES: [ExpectedProfile; 4] = [
    ExpectedProfile::new("go", AnalysisTier::TierA, LanguageSemantics::Static),
    ExpectedProfile::new("python", AnalysisTier::TierB, LanguageSemantics::Dynamic),
    ExpectedProfile::new("rust", AnalysisTier::TierA, LanguageSemantics::Static),
    ExpectedProfile::new("typescript", AnalysisTier::TierA, LanguageSemantics::Static),
];

/// Opaque, source-free semantic conformance and quality report.
///
/// Use [`encode_semantic_evidence`] to obtain the canonical JSON artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticEvidence {
    schema_version: String,
    corpus_id: String,
    adapter_crate: String,
    disposition: EvidenceDisposition,
    production_acceptance_eligible: bool,
    languages: Vec<LanguageEvidence>,
    resolver_quality: ResolverQualityEvidence,
    repository_execution: RepositoryExecutionEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceDisposition {
    ContractFixtureOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LanguageEvidence {
    language: String,
    maximum_tier: TierEvidence,
    semantics: SemanticsEvidence,
    adapter_version: String,
    context_import_contract: String,
    observed_context_imports: u64,
    tier_promotion_eligible: bool,
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
    corpus_scope: ResolverCorpusScope,
    language_breakdown_available: bool,
    holdout_available: bool,
    promotion_eligible: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResolverCorpusScope {
    CallerSuppliedContractFixture,
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

/// Invalid or oversized semantic contract evidence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SemanticEvidenceError {
    /// The shared registry differs from the reviewed four-language profile.
    #[error("Semantic language conformance evidence is invalid")]
    InvalidLanguageEvidence,
    /// Resolver results fail the ambiguity-sensitive quality contract.
    #[error("Semantic resolver quality evidence is invalid")]
    InvalidQualityEvidence,
    /// The resolver quality corpus was internally inconsistent.
    #[error("Semantic resolver quality evaluation failed")]
    Quality(#[source] rootlight_resolve::QualityError),
    /// A fixed input or output ceiling was exceeded.
    #[error("Semantic contract evidence limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// Canonical JSON serialization failed.
    #[error("Semantic contract evidence encoding failed")]
    Encode,
    /// Source revision was not a canonical full Git object name.
    #[error("Semantic contract evidence source revision is invalid")]
    InvalidSourceRevision,
}

/// Builds contract evidence from the shared registry and caller-supplied decisions.
///
/// The function accepts no repository path, environment, process, or command
/// input. The report records that structural no-execution restriction and is
/// explicitly ineligible for tier promotion or production acceptance.
///
/// # Errors
///
/// Returns [`SemanticEvidenceError`] for a changed profile contract, an oversized
/// corpus, inconsistent ground truth, hidden ambiguity, missing explicit
/// outcomes, or quality and calibration below the semantic quality floors.
pub fn build_semantic_evidence(
    registry: &LanguageAdapterRegistry,
    batch: &ResolutionBatch,
    expectations: &[ResolutionExpectation],
) -> Result<SemanticEvidence, SemanticEvidenceError> {
    if expectations.len() > SEMANTIC_EVIDENCE_MAX_EXPECTATIONS {
        return Err(SemanticEvidenceError::LimitExceeded {
            resource: "expectation_count",
        });
    }
    if batch.decisions.len() > SEMANTIC_EVIDENCE_MAX_EXPECTATIONS {
        return Err(SemanticEvidenceError::LimitExceeded {
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
        return Err(SemanticEvidenceError::LimitExceeded {
            resource: "candidate_count",
        });
    }
    Ok(SemanticEvidence {
        schema_version: SEMANTIC_EVIDENCE_SCHEMA_VERSION.to_owned(),
        corpus_id: CORPUS_ID.to_owned(),
        adapter_crate: ADAPTER_CRATE.to_owned(),
        disposition: EvidenceDisposition::ContractFixtureOnly,
        production_acceptance_eligible: false,
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
/// Returns [`SemanticEvidenceError`] when serialization fails or the report exceeds
/// [`SEMANTIC_EVIDENCE_MAX_BYTES`].
pub fn encode_semantic_evidence(
    evidence: &SemanticEvidence,
) -> Result<Vec<u8>, SemanticEvidenceError> {
    let encoded = serde_json::to_vec(evidence).map_err(|_| SemanticEvidenceError::Encode)?;
    if encoded.len() > SEMANTIC_EVIDENCE_MAX_BYTES {
        return Err(SemanticEvidenceError::LimitExceeded {
            resource: "encoded_bytes",
        });
    }
    Ok(encoded)
}

/// Encodes one report with the exact source revision that produced it.
///
/// # Errors
///
/// Returns [`SemanticEvidenceError`] for a non-canonical lowercase 40-digit Git
/// object name, serialization failure, or an envelope above its hard ceiling.
pub fn encode_semantic_evidence_envelope(
    evidence: &SemanticEvidence,
    source_revision: &str,
) -> Result<Vec<u8>, SemanticEvidenceError> {
    if source_revision.len() != 40
        || source_revision
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(SemanticEvidenceError::InvalidSourceRevision);
    }
    let envelope = SemanticEvidenceEnvelope {
        schema: SEMANTIC_EVIDENCE_ENVELOPE_SCHEMA,
        source_revision,
        evidence,
    };
    let encoded = serde_json::to_vec(&envelope).map_err(|_| SemanticEvidenceError::Encode)?;
    if encoded.len() > SEMANTIC_EVIDENCE_ENVELOPE_MAX_BYTES {
        return Err(SemanticEvidenceError::LimitExceeded {
            resource: "envelope_bytes",
        });
    }
    Ok(encoded)
}

#[derive(Debug, Serialize)]
struct SemanticEvidenceEnvelope<'a> {
    schema: &'static str,
    source_revision: &'a str,
    evidence: &'a SemanticEvidence,
}

fn language_evidence(
    registry: &LanguageAdapterRegistry,
) -> Result<Vec<LanguageEvidence>, SemanticEvidenceError> {
    EXPECTED_PROFILES
        .into_iter()
        .map(|expected| {
            let profile = registry
                .iter()
                .find(|profile| profile.language().as_str() == expected.language)
                .ok_or(SemanticEvidenceError::InvalidLanguageEvidence)?;
            if profile.maximum_tier() != expected.tier || profile.semantics() != expected.semantics
            {
                return Err(SemanticEvidenceError::InvalidLanguageEvidence);
            }
            let uncertainty_codes = profile
                .uncertainties()
                .map(|code| code.as_str().to_owned())
                .collect::<Vec<_>>();
            if uncertainty_codes.is_empty() {
                return Err(SemanticEvidenceError::InvalidLanguageEvidence);
            }
            Ok(LanguageEvidence {
                language: expected.language.to_owned(),
                maximum_tier: tier_evidence(expected.tier)?,
                semantics: semantics_evidence(expected.semantics)?,
                adapter_version: ADAPTER_VERSION.to_owned(),
                context_import_contract: "validated_declarative_normalized_ir".to_owned(),
                observed_context_imports: 0,
                tier_promotion_eligible: false,
                required_coverage_domains: 8,
                uncertainty_codes,
            })
        })
        .collect()
}

fn quality_evidence(
    batch: &ResolutionBatch,
    expectations: &[ResolutionExpectation],
) -> Result<ResolverQualityEvidence, SemanticEvidenceError> {
    let report =
        evaluate_resolution_quality(batch, expectations).map_err(SemanticEvidenceError::Quality)?;
    let expectations_by_occurrence = expectations
        .iter()
        .map(|expectation| (expectation.occurrence, expectation.expected))
        .collect::<BTreeMap<_, _>>();
    if expectations_by_occurrence.len() != expectations.len() {
        return Err(SemanticEvidenceError::InvalidQualityEvidence);
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
                    .map_err(|_| SemanticEvidenceError::InvalidQualityEvidence)?;
                if *total_count < materialized
                    || (*completeness == CoverageStatus::Complete && *total_count != materialized)
                {
                    return Err(SemanticEvidenceError::InvalidQualityEvidence);
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
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;

    let expected_total = exact_expected
        .checked_add(ambiguous_expected)
        .and_then(|total| total.checked_add(unresolved_expected))
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    let outcome_total = exact_outcomes
        .checked_add(candidate_outcomes)
        .and_then(|total| total.checked_add(unresolved_outcomes))
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
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
        return Err(SemanticEvidenceError::InvalidQualityEvidence);
    }

    Ok(ResolverQualityEvidence {
        corpus_scope: ResolverCorpusScope::CallerSuppliedContractFixture,
        language_breakdown_available: false,
        holdout_available: false,
        promotion_eligible: false,
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

fn candidate_metrics(sizes: &[u64]) -> Result<(u64, u64, u64), SemanticEvidenceError> {
    let count =
        u64::try_from(sizes.len()).map_err(|_| SemanticEvidenceError::InvalidQualityEvidence)?;
    let total = sizes.iter().try_fold(0_u64, |sum, size| {
        sum.checked_add(*size)
            .ok_or(SemanticEvidenceError::InvalidQualityEvidence)
    })?;
    let mean = total
        .checked_mul(MILLI_SCALE)
        .and_then(|scaled| scaled.checked_div(count))
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    let rank = count
        .checked_mul(P95_PERCENT)
        .and_then(|scaled| scaled.checked_add(PERCENT_SCALE - 1))
        .and_then(|scaled| scaled.checked_div(PERCENT_SCALE))
        .and_then(|rank| rank.checked_sub(1))
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    let index = usize::try_from(rank).map_err(|_| SemanticEvidenceError::InvalidQualityEvidence)?;
    let p95 = *sizes
        .get(index)
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    let maximum = *sizes
        .last()
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    Ok((mean, p95, maximum))
}

fn basis_points(numerator: u64, denominator: u64) -> Result<u16, SemanticEvidenceError> {
    let scaled = u128::from(numerator)
        .checked_mul(10_000)
        .and_then(|value| value.checked_div(u128::from(denominator)))
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    u16::try_from(scaled).map_err(|_| SemanticEvidenceError::InvalidQualityEvidence)
}

fn ratio(value: Option<u16>) -> Result<u16, SemanticEvidenceError> {
    value.ok_or(SemanticEvidenceError::InvalidQualityEvidence)
}

fn increment(value: &mut u64) -> Result<(), SemanticEvidenceError> {
    *value = value
        .checked_add(1)
        .ok_or(SemanticEvidenceError::InvalidQualityEvidence)?;
    Ok(())
}

fn tier_evidence(tier: AnalysisTier) -> Result<TierEvidence, SemanticEvidenceError> {
    match tier {
        AnalysisTier::TierA => Ok(TierEvidence::TierA),
        AnalysisTier::TierB => Ok(TierEvidence::TierB),
        AnalysisTier::TierC | AnalysisTier::TierD => {
            Err(SemanticEvidenceError::InvalidLanguageEvidence)
        }
        _ => Err(SemanticEvidenceError::InvalidLanguageEvidence),
    }
}

fn semantics_evidence(
    semantics: LanguageSemantics,
) -> Result<SemanticsEvidence, SemanticEvidenceError> {
    match semantics {
        LanguageSemantics::Static => Ok(SemanticsEvidence::Static),
        LanguageSemantics::Dynamic => Ok(SemanticsEvidence::Dynamic),
        _ => Err(SemanticEvidenceError::InvalidLanguageEvidence),
    }
}

#[derive(Debug, Clone, Copy)]
struct ExpectedProfile {
    language: &'static str,
    tier: AnalysisTier,
    semantics: LanguageSemantics,
}

impl ExpectedProfile {
    const fn new(language: &'static str, tier: AnalysisTier, semantics: LanguageSemantics) -> Self {
        Self {
            language,
            tier,
            semantics,
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
        let first = build_semantic_evidence(&registry, &batch, &expectations)
            .expect("reference corpus passes");
        let repeated = build_semantic_evidence(&registry, &batch, &expectations)
            .expect("reference corpus repeats");
        let first_bytes = encode_semantic_evidence(&first).expect("evidence encodes");
        let repeated_bytes =
            encode_semantic_evidence(&repeated).expect("repeated evidence encodes");

        assert_eq!(first_bytes, repeated_bytes);
        assert_eq!(first.disposition, EvidenceDisposition::ContractFixtureOnly);
        assert!(!first.production_acceptance_eligible);
        assert_eq!(first.languages.len(), 4);
        assert!(
            first
                .languages
                .iter()
                .all(|language| language.observed_context_imports == 0
                    && !language.tier_promotion_eligible)
        );
        assert_eq!(
            first.resolver_quality.corpus_scope,
            ResolverCorpusScope::CallerSuppliedContractFixture
        );
        assert!(!first.resolver_quality.language_breakdown_available);
        assert!(!first.resolver_quality.holdout_available);
        assert!(!first.resolver_quality.promotion_eligible);
        assert_eq!(first.resolver_quality.exact_outcomes, 1);
        assert_eq!(first.resolver_quality.candidate_outcomes, 1);
        assert_eq!(first.resolver_quality.unresolved_outcomes, 1);
        assert_eq!(first.resolver_quality.ambiguous_hidden_exact, 0);
        assert_eq!(first.resolver_quality.expected_calibration_error_milli, 50);
        assert!(!first.repository_execution.command_surface_available);
        assert_eq!(first.repository_execution.observed_command_attempts, 0);
        let text = String::from_utf8(first_bytes).expect("canonical evidence is UTF-8");
        assert!(text.contains("\"disposition\":\"contract_fixture_only\""));
        assert!(text.contains("\"production_acceptance_eligible\":false"));
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
            build_semantic_evidence(&registry, &batch, &expectations),
            Err(SemanticEvidenceError::InvalidQualityEvidence)
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
            build_semantic_evidence(&incomplete, &batch, &expectations),
            Err(SemanticEvidenceError::InvalidLanguageEvidence)
        ));
    }

    #[test]
    fn expectation_count_is_bounded_before_quality_evaluation() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (mut batch, expectations) = corpus();
        let oversized = vec![expectations[0]; SEMANTIC_EVIDENCE_MAX_EXPECTATIONS + 1];
        assert!(matches!(
            build_semantic_evidence(&registry, &batch, &oversized),
            Err(SemanticEvidenceError::LimitExceeded {
                resource: "expectation_count"
            })
        ));

        let ResolutionOutcome::Candidates { symbols, .. } = &mut batch.decisions[1].outcome else {
            panic!("fixture contains a candidate decision");
        };
        *symbols = vec![SymbolId::from_bytes([14; 20]); MAX_CANDIDATE_LIMIT + 1];
        assert!(matches!(
            build_semantic_evidence(&registry, &batch, &expectations),
            Err(SemanticEvidenceError::LimitExceeded {
                resource: "candidate_count"
            })
        ));
    }

    #[test]
    fn source_bound_envelope_is_deterministic_and_rejects_invalid_revisions() {
        let registry = initial_semantic_registry().expect("shared registry is valid");
        let (batch, expectations) = corpus();
        let evidence = build_semantic_evidence(&registry, &batch, &expectations)
            .expect("fixture evidence builds");
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let first = encode_semantic_evidence_envelope(&evidence, revision)
            .expect("source-bound evidence encodes");
        let second = encode_semantic_evidence_envelope(&evidence, revision)
            .expect("source-bound evidence re-encodes");
        assert_eq!(first, second);
        assert!(first.len() <= SEMANTIC_EVIDENCE_ENVELOPE_MAX_BYTES);
        let value: serde_json::Value =
            serde_json::from_slice(&first).expect("envelope JSON decodes");
        assert_eq!(value["schema"], SEMANTIC_EVIDENCE_ENVELOPE_SCHEMA);
        assert_eq!(value["source_revision"], revision);
        assert_eq!(value["evidence"]["production_acceptance_eligible"], false);
        assert!(matches!(
            encode_semantic_evidence_envelope(&evidence, "not-a-revision"),
            Err(SemanticEvidenceError::InvalidSourceRevision)
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
