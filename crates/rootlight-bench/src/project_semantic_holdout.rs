//! Independent quality evidence for production whole-project semantics.
//!
//! The production analyzer receives only the versioned source manifest. A
//! separate reviewed answer key is withheld from analyzer inputs and applied
//! only during post-analysis scoring against authoritative source locations.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisUnitId, BatchThresholds, BuildTargetId, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseProvider,
    ProjectAnalysisLimits, ProjectAnalysisOutput, ProjectAnalysisRequest, ProjectSourceInput,
    StreamLimits, execute_project_analysis,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_adapters::{SemanticProjectAnalyzer, SemanticProjectLanguage};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId, content_hash};
#[cfg(test)]
use rootlight_ir::Confidence;
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, EntityVisibility,
    ExtensionSupport, FactRef, IrLimits, LEXICAL_EXTENSION_NAMESPACE, LexicalEvidenceKind,
    NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, ProducerIdentity,
    ProducerKind, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    decode_lexical_evidence_envelope,
};
use rootlight_resolve::{
    CompletenessAssumption, ExpectedResolution, ResolutionBatch, ResolutionDecision,
    ResolutionExpectation, ResolutionExplanation, ResolutionOutcome, ResolutionQualityReport,
    ResolutionRule, UnresolvedReason, evaluate_resolution_quality,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const MANIFEST_SCHEMA: &str = "rootlight.project-semantic-holdout-manifest/1";
const ANSWER_KEY_SCHEMA: &str = "rootlight.project-semantic-holdout-answer-key/1";
const CORPUS_ID: &str = "rootlight-project-semantic-holdout-v1";
const ANALYZER_ID: &str = "rootlight-project-semantics";
const PARSER_ID: &str = "rootlight-audited-tree-sitter";
const EXPECTED_LANGUAGES: [&str; 5] = ["rust", "typescript", "javascript", "python", "go"];
const EXPECTED_SOURCE_FILES: usize = 3;
const EXPECTED_EXACT_CALLS: u64 = 8;
const EXPECTED_AMBIGUOUS_CALLS: u64 = 2;
const EXPECTED_UNRESOLVED_CALLS: u64 = 2;
const EXPECTED_CALLS: u64 =
    EXPECTED_EXACT_CALLS + EXPECTED_AMBIGUOUS_CALLS + EXPECTED_UNRESOLVED_CALLS;
const MIN_PRECISION_BASIS_POINTS: u16 = 9_500;
const MIN_RECALL_BASIS_POINTS: u16 = 9_000;
const MAX_CALIBRATION_ERROR_MILLI: u16 = 400;
const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_SYNTAX_NODES: usize = 64 * 1024;
const MAX_SYNTAX_DEPTH: usize = 256;
const MAX_FIXTURE_BYTES: usize = 256 * 1024;

const MANIFEST_JSON: &str = include_str!("../tests/fixtures/semantic-holdout/v1/manifest.json");
const ANSWER_KEY_JSON: &str = include_str!("../tests/fixtures/semantic-holdout/v1/answer-key.json");

/// Version of the independent project-semantic holdout artifact.
pub const PROJECT_SEMANTIC_HOLDOUT_SCHEMA: &str = "rootlight.project-semantic-holdout/2";
/// Maximum canonical size of one project-semantic holdout artifact.
pub const PROJECT_SEMANTIC_HOLDOUT_MAX_BYTES: usize = 32 * 1024;
/// Version of the exact-source semantic holdout envelope.
pub const PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_SCHEMA: &str =
    "rootlight.project-semantic-holdout-envelope/1";
/// Maximum canonical size of one exact-source semantic holdout envelope.
pub const PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_MAX_BYTES: usize = 64 * 1024;

/// Source-free quality evidence from the production Tier B project path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectSemanticHoldoutEvidence {
    schema: &'static str,
    corpus_id: &'static str,
    corpus_sha256: String,
    analyzer: &'static str,
    parser: &'static str,
    execution_path: &'static str,
    observed_tier: &'static str,
    compiler_assisted_observed: bool,
    compiler_build_context_observed: bool,
    holdout_available: bool,
    language_breakdown_available: bool,
    languages: Vec<ProjectSemanticLanguageEvidence>,
    aggregate: ProjectSemanticAggregateEvidence,
}

#[derive(Debug, Serialize)]
struct ProjectSemanticHoldoutEnvelope<'a> {
    schema: &'static str,
    source_revision: &'a str,
    evidence: &'a ProjectSemanticHoldoutEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ProjectSemanticLanguageEvidence {
    language: String,
    observed_tier: &'static str,
    compiler_assisted_observed: bool,
    source_files: u8,
    annotated_call_sites: u64,
    observed_annotated_call_sites: u64,
    exact_expected: u64,
    ambiguous_expected: u64,
    unresolved_expected: u64,
    exact_precision_basis_points: u16,
    exact_recall_basis_points: u16,
    candidate_recall_basis_points: u16,
    unresolved_correct: u64,
    ambiguous_hidden_exact: u64,
    unexpected_decisions: u64,
    calibration_samples: u64,
    expected_calibration_error_milli: u16,
    capabilities: ProjectSemanticCapabilityEvidence,
    analysis_coverage_complete: bool,
    build_context_identity_bound: bool,
    provenance_complete: bool,
    repeated_output_identical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ProjectSemanticCapabilityEvidence {
    import_edges: ProjectSemanticCapabilityScore,
    signatures: ProjectSemanticCapabilityScore,
    type_entities: ProjectSemanticCapabilityScore,
    hierarchy_relations: ProjectSemanticCapabilityScore,
    visibility: ProjectSemanticCapabilityScore,
    inferred_calls: ProjectSemanticCapabilityScore,
    dispatch_candidates: ProjectSemanticCapabilityScore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ProjectSemanticCapabilityScore {
    expected: u64,
    observed: u64,
    correct: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ProjectSemanticAggregateEvidence {
    annotated_call_sites: u64,
    observed_annotated_call_sites: u64,
    exact_expected: u64,
    ambiguous_expected: u64,
    unresolved_expected: u64,
    exact_precision_basis_points: u16,
    exact_recall_basis_points: u16,
    candidate_recall_basis_points: u16,
    unresolved_correct: u64,
    ambiguous_hidden_exact: u64,
    unexpected_decisions: u64,
    calibration_samples: u64,
    maximum_expected_calibration_error_milli: u16,
}

/// Failure to construct or validate the independent semantic holdout.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectSemanticHoldoutError {
    /// A named built-in configuration boundary rejected the reviewed fixture.
    #[error("Project semantic holdout configuration is invalid at {0}")]
    ConfigurationAt(&'static str),
    /// The versioned manifest or answer key is malformed or inconsistent.
    #[error("Project semantic holdout fixture is invalid")]
    Fixture,
    /// The temporary annotated corpus could not be materialized.
    #[error("Project semantic holdout corpus is unavailable")]
    Corpus,
    /// The production analysis path rejected a reviewed holdout case.
    #[error("Project semantic holdout analysis failed")]
    Analysis,
    /// Measured output no longer meets the reviewed Tier B quality contract.
    #[error("Project semantic holdout quality is invalid: {0}")]
    Quality(String),
    /// Canonical evidence encoding failed or exceeded its byte ceiling.
    #[error("Project semantic holdout encoding failed")]
    Encode,
}

/// Runs the independent annotated corpus through the production project path.
///
/// This evidence intentionally claims Tier B only. Compiler-backed Tier A
/// remains a separate capability that must provide its own frontend evidence.
///
/// # Errors
///
/// Returns [`ProjectSemanticHoldoutError`] when fixture integrity, production
/// analysis, source-bound ground truth, ambiguity handling, provenance,
/// quality thresholds, or bounded encoding no longer satisfies the contract.
pub fn build_project_semantic_holdout()
-> Result<ProjectSemanticHoldoutEvidence, ProjectSemanticHoldoutError> {
    let fixture = load_fixture()?;
    let parser: Arc<dyn ParseProvider> = Arc::new(
        TreeSitterProvider::new(parser_config()?)
            .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("parser provider"))?,
    );
    let limits = analysis_limits()?;
    let mut languages = Vec::with_capacity(EXPECTED_LANGUAGES.len());

    for (manifest, answer) in fixture
        .manifest
        .languages
        .iter()
        .zip(&fixture.answer_key.languages)
    {
        languages.push(measure_language(manifest, answer, parser.clone(), &limits)?);
    }

    let aggregate = aggregate_evidence(&languages)?;
    let evidence = ProjectSemanticHoldoutEvidence {
        schema: PROJECT_SEMANTIC_HOLDOUT_SCHEMA,
        corpus_id: CORPUS_ID,
        corpus_sha256: fixture.digest,
        analyzer: ANALYZER_ID,
        parser: PARSER_ID,
        execution_path: "in_process_analyzer",
        observed_tier: "tier_b",
        compiler_assisted_observed: languages
            .iter()
            .any(|language| language.compiler_assisted_observed),
        compiler_build_context_observed: false,
        holdout_available: true,
        language_breakdown_available: true,
        languages,
        aggregate,
    };
    validate_evidence(&evidence)?;
    encode_project_semantic_holdout(&evidence)?;
    Ok(evidence)
}

/// Encodes one holdout report as bounded canonical JSON.
///
/// # Errors
///
/// Returns [`ProjectSemanticHoldoutError::Encode`] when serialization fails or
/// the encoded artifact exceeds [`PROJECT_SEMANTIC_HOLDOUT_MAX_BYTES`].
pub fn encode_project_semantic_holdout(
    evidence: &ProjectSemanticHoldoutEvidence,
) -> Result<Vec<u8>, ProjectSemanticHoldoutError> {
    let encoded = serde_json::to_vec(evidence).map_err(|_| ProjectSemanticHoldoutError::Encode)?;
    if encoded.len() > PROJECT_SEMANTIC_HOLDOUT_MAX_BYTES {
        return Err(ProjectSemanticHoldoutError::Encode);
    }
    Ok(encoded)
}

/// Binds one production holdout report to the exact Git source revision.
///
/// # Errors
///
/// Returns [`ProjectSemanticHoldoutError::Encode`] for a noncanonical revision,
/// serialization failure, or an envelope above its fixed byte ceiling.
pub fn encode_project_semantic_holdout_envelope(
    evidence: &ProjectSemanticHoldoutEvidence,
    source_revision: &str,
) -> Result<Vec<u8>, ProjectSemanticHoldoutError> {
    if source_revision.len() != 40
        || source_revision
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(ProjectSemanticHoldoutError::Encode);
    }
    let encoded = serde_json::to_vec(&ProjectSemanticHoldoutEnvelope {
        schema: PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_SCHEMA,
        source_revision,
        evidence,
    })
    .map_err(|_| ProjectSemanticHoldoutError::Encode)?;
    if encoded.len() > PROJECT_SEMANTIC_HOLDOUT_ENVELOPE_MAX_BYTES {
        return Err(ProjectSemanticHoldoutError::Encode);
    }
    Ok(encoded)
}

/// Verifies one normalized document against the exact reviewed holdout answer key.
///
/// This entry point is intended for composed process-boundary tests. It checks
/// the source-bound exact targets, complete candidate identity sets,
/// unresolved sites, capability endpoints, and provenance without rerunning
/// the analyzer in process.
///
/// # Errors
///
/// Returns [`ProjectSemanticHoldoutError`] when `language` is not part of the
/// reviewed corpus or any emitted identity differs from the independent answer
/// key.
pub fn verify_project_semantic_holdout_document(
    language: &str,
    document: &NormalizedIrDocument,
    expected_build_context: BuildContextIdentity,
) -> Result<(), ProjectSemanticHoldoutError> {
    let fixture = load_fixture()?;
    let manifest = fixture
        .manifest
        .languages
        .iter()
        .find(|entry| entry.language == language)
        .ok_or(ProjectSemanticHoldoutError::Fixture)?;
    let answer = fixture
        .answer_key
        .languages
        .iter()
        .find(|entry| entry.language == language)
        .ok_or(ProjectSemanticHoldoutError::Fixture)?;
    let scored = score_document(document, manifest, answer)?;
    let report = evaluate_resolution_quality(&scored.batch, &scored.expectations)
        .map_err(|_| quality(format!("{language} quality evaluation failed")))?;
    if scored.observed_calls != EXPECTED_CALLS
        || report.expectations != EXPECTED_CALLS
        || report.exact_recall.denominator != EXPECTED_EXACT_CALLS
        || report
            .candidate_recall
            .denominator
            .saturating_sub(report.exact_recall.denominator)
            != EXPECTED_AMBIGUOUS_CALLS
        || report.unresolved_expected != EXPECTED_UNRESOLVED_CALLS
        || report.unresolved_correct != EXPECTED_UNRESOLVED_CALLS
        || report.ambiguous_hidden_exact != 0
        || report.unexpected_decisions != 0
        || required_basis_points(report.exact_precision, "exact precision")? != 10_000
        || required_basis_points(report.exact_recall, "exact recall")? != 10_000
        || required_basis_points(report.candidate_recall, "candidate recall")? != 10_000
    {
        return Err(quality(format!(
            "{language} composed resolution identities differ"
        )));
    }
    let capabilities = capability_evidence(manifest, answer, document, &scored)?;
    if !capability_complete(capabilities.import_edges)
        || !capability_complete(capabilities.signatures)
        || !capability_complete(capabilities.type_entities)
        || !capability_complete(capabilities.hierarchy_relations)
        || !capability_complete(capabilities.visibility)
        || !capability_complete(capabilities.inferred_calls)
        || !capability_complete(capabilities.dispatch_candidates)
    {
        return Err(quality(format!(
            "{language} composed capability identities differ"
        )));
    }
    let provenance = validate_provenance(document, expected_build_context)?;
    if provenance.compiler_assisted_observed
        || !provenance.build_context_identity_bound
        || !provenance.complete
    {
        return Err(quality(format!("{language} composed provenance differs")));
    }
    Ok(())
}

fn measure_language(
    manifest: &ManifestLanguage,
    answer: &AnswerLanguage,
    parser: Arc<dyn ParseProvider>,
    limits: &AnalysisLimits,
) -> Result<ProjectSemanticLanguageEvidence, ProjectSemanticHoldoutError> {
    let first = analyze_case(manifest, parser.clone(), limits)?;
    let repeated = analyze_case(manifest, parser, limits)?;
    let repeated_output_identical = first.document() == repeated.document();
    let scored = score_document(first.document(), manifest, answer)?;
    let report = evaluate_resolution_quality(&scored.batch, &scored.expectations)
        .map_err(|_| quality(format!("{} quality evaluation failed", manifest.language)))?;
    let coverage = first.report().work().coverage();
    let execution = LanguageExecutionEvidence {
        provenance: validate_provenance(first.document(), scored.build_context)?,
        analysis_coverage_complete: coverage.tier() == AnalysisTier::TierB
            && coverage.status() == CoverageStatus::Complete,
        repeated_output_identical,
    };
    let evidence = language_evidence(
        manifest,
        answer,
        first.document(),
        &scored,
        &report,
        execution,
    )?;
    validate_language_evidence(&evidence)?;
    Ok(evidence)
}

fn score_document(
    document: &NormalizedIrDocument,
    manifest: &ManifestLanguage,
    answer: &AnswerLanguage,
) -> Result<ScoredLanguage, ProjectSemanticHoldoutError> {
    let sources = manifest_source_map(manifest)?;
    let files = document
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.id))
        .collect::<BTreeMap<_, _>>();
    let call_file = files
        .get(answer.call_path.as_str())
        .copied()
        .ok_or_else(|| quality(format!("{} call file is absent", manifest.language)))?;
    let call_source = sources
        .get(answer.call_path.as_str())
        .copied()
        .ok_or(ProjectSemanticHoldoutError::Fixture)?;
    let mut decisions = Vec::with_capacity(usize::try_from(EXPECTED_CALLS).unwrap_or(0));
    let mut expectations = Vec::with_capacity(usize::try_from(EXPECTED_CALLS).unwrap_or(0));
    let mut annotated_occurrences = BTreeSet::new();

    for expected in &answer.exact {
        let occurrence = annotated_call(
            document,
            call_file,
            call_source,
            expected.call_line,
            &expected.call,
        )?;
        let target = target_symbol(
            document,
            &files,
            &sources,
            &expected.target_path,
            expected.target_line,
            &expected.target,
        )?;
        expectations.push(ResolutionExpectation {
            occurrence: occurrence.id,
            expected: ExpectedResolution::Exact(target),
        });
        decisions.push(resolution_decision(occurrence)?);
        if !annotated_occurrences.insert(occurrence.id) {
            return Err(quality(format!(
                "{} answer key contains a duplicate call site",
                manifest.language
            )));
        }
    }

    for expected in &answer.candidates {
        let occurrence = annotated_call(
            document,
            call_file,
            call_source,
            expected.call_line,
            &expected.call,
        )?;
        let expected_symbols = expected
            .targets
            .iter()
            .map(|target| {
                target_symbol(
                    document,
                    &files,
                    &sources,
                    &target.path,
                    target.line,
                    &target.name,
                )
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let observed_symbols = match &occurrence.target {
            OccurrenceTarget::Candidates {
                symbols,
                total_count,
                completeness,
            } if *completeness == CoverageStatus::Unknown
                && *total_count == u64::try_from(symbols.len()).unwrap_or(u64::MAX) =>
            {
                symbols.iter().copied().collect::<BTreeSet<_>>()
            }
            _ => {
                return Err(quality(format!(
                    "{} ambiguity was not preserved: {:?}",
                    manifest.language, occurrence.target
                )));
            }
        };
        if observed_symbols != expected_symbols {
            return Err(quality(format!(
                "{} ambiguity target identities differ",
                manifest.language
            )));
        }
        let reviewed_target = expected_symbols
            .first()
            .copied()
            .ok_or(ProjectSemanticHoldoutError::Fixture)?;
        expectations.push(ResolutionExpectation {
            occurrence: occurrence.id,
            expected: ExpectedResolution::CandidateContains(reviewed_target),
        });
        decisions.push(resolution_decision(occurrence)?);
        if !annotated_occurrences.insert(occurrence.id) {
            return Err(quality(format!(
                "{} answer key contains a duplicate call site",
                manifest.language
            )));
        }
    }

    for expected in &answer.unresolved {
        let occurrence = annotated_call(
            document,
            call_file,
            call_source,
            expected.call_line,
            &expected.call,
        )?;
        expectations.push(ResolutionExpectation {
            occurrence: occurrence.id,
            expected: ExpectedResolution::Unresolved,
        });
        decisions.push(resolution_decision(occurrence)?);
        if !annotated_occurrences.insert(occurrence.id) {
            return Err(quality(format!(
                "{} answer key contains a duplicate call site",
                manifest.language
            )));
        }
    }

    let observed_calls = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .map(|occurrence| occurrence.id)
        .collect::<BTreeSet<_>>();
    if observed_calls != annotated_occurrences {
        return Err(quality(format!(
            "{} observed call sites differ from the complete answer key",
            manifest.language
        )));
    }
    decisions.sort_by_key(|decision| decision.occurrence);
    expectations.sort_by_key(|expectation| expectation.occurrence);
    validate_decision_coverage(&decisions, &expectations)?;
    Ok(ScoredLanguage {
        batch: ResolutionBatch {
            repository: document.repository,
            generation: document.generation,
            decisions,
        },
        expectations,
        build_context: build_context(&manifest.language),
        observed_calls: u64::try_from(observed_calls.len())
            .map_err(|_| quality("observed call count is not representable"))?,
    })
}

fn validate_decision_coverage(
    decisions: &[ResolutionDecision],
    expectations: &[ResolutionExpectation],
) -> Result<(), ProjectSemanticHoldoutError> {
    let observed = decisions
        .iter()
        .map(|decision| decision.occurrence)
        .collect::<BTreeSet<_>>();
    let expected = expectations
        .iter()
        .map(|expectation| expectation.occurrence)
        .collect::<BTreeSet<_>>();
    if decisions.len() != observed.len()
        || expectations.len() != expected.len()
        || observed != expected
    {
        return Err(quality(
            "semantic decisions do not cover every annotated call site exactly once",
        ));
    }
    Ok(())
}

fn annotated_call<'document>(
    document: &'document NormalizedIrDocument,
    file: FileId,
    source: &[u8],
    line: u32,
    name: &str,
) -> Result<&'document OccurrenceRecord, ProjectSemanticHoldoutError> {
    let (line_start, line_end) = line_bounds(source, line)?;
    let expected_hash = content_hash(name.as_bytes());
    let mut matches = document.occurrences.iter().filter(|occurrence| {
        let span = occurrence.source.span();
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.file == file
            && occurrence.syntactic_text_hash == expected_hash
            && span.start_byte() >= line_start
            && span.end_byte() <= line_end
    });
    let occurrence = matches
        .next()
        .ok_or_else(|| quality(format!("annotated call {name} was not emitted")))?;
    if matches.next().is_some() {
        return Err(quality(format!("annotated call {name} is not unique")));
    }
    let span = occurrence.source.span();
    let start = usize::try_from(span.start_byte())
        .map_err(|_| quality("call span start is not representable"))?;
    let end = usize::try_from(span.end_byte())
        .map_err(|_| quality("call span end is not representable"))?;
    let observed = std::str::from_utf8(
        source
            .get(start..end)
            .ok_or_else(|| quality("call span is outside its source"))?,
    )
    .map_err(|_| quality("call span is not UTF-8"))?;
    if !observed.ends_with(name) {
        return Err(quality(format!(
            "annotated call {name} has a different authoritative span"
        )));
    }
    Ok(occurrence)
}

fn target_symbol(
    document: &NormalizedIrDocument,
    files: &BTreeMap<&str, FileId>,
    sources: &BTreeMap<&str, &[u8]>,
    path: &str,
    line: u32,
    name: &str,
) -> Result<SymbolId, ProjectSemanticHoldoutError> {
    let file = files
        .get(path)
        .copied()
        .ok_or_else(|| quality(format!("target file for {name} is absent")))?;
    let source = sources
        .get(path)
        .copied()
        .ok_or(ProjectSemanticHoldoutError::Fixture)?;
    let (line_start, line_end) = line_bounds(source, line)?;
    let mut matches = document.entities.iter().filter(|entity| {
        let Some(source) = entity.evidence.source.as_ref() else {
            return false;
        };
        let span = source.span();
        entity.display_name == name
            && span.file() == file
            && span.start_byte() >= line_start
            && span.start_byte() < line_end
    });
    let symbol = matches.next().map(|entity| entity.id).ok_or_else(|| {
        let observed = document
            .entities
            .iter()
            .filter(|entity| entity.display_name == name)
            .map(|entity| {
                let observed_path = document
                    .files
                    .iter()
                    .find(|record| {
                        record.id
                            == entity
                                .evidence
                                .source
                                .as_ref()
                                .map(|value| value.span().file())
                                .unwrap_or(file)
                    })
                    .map(|record| record.path.as_str())
                    .unwrap_or("<unknown>");
                let span = entity.evidence.source.as_ref().map(|value| value.span());
                format!("{observed_path}:{span:?}")
            })
            .collect::<Vec<_>>();
        quality(format!(
            "reviewed target {name} at {path}:{line} was not emitted; observed {observed:?}"
        ))
    })?;
    if matches.next().is_some() {
        return Err(quality(format!("reviewed target {name} is not unique")));
    }
    Ok(symbol)
}

fn resolution_decision(
    occurrence: &OccurrenceRecord,
) -> Result<ResolutionDecision, ProjectSemanticHoldoutError> {
    let outcome = match &occurrence.target {
        OccurrenceTarget::Resolved { symbol } => ResolutionOutcome::Resolved {
            symbol: *symbol,
            confidence: occurrence.confidence,
        },
        OccurrenceTarget::Candidates {
            symbols,
            total_count,
            completeness,
        } => ResolutionOutcome::Candidates {
            symbols: symbols.clone(),
            total_count: *total_count,
            completeness: *completeness,
            confidence: occurrence.confidence,
        },
        OccurrenceTarget::Unresolved { .. } => ResolutionOutcome::Unresolved {
            reason: UnresolvedReason::NoCandidate,
            confidence: occurrence.confidence,
        },
    };
    Ok(ResolutionDecision {
        occurrence: occurrence.id,
        outcome,
        explanation: ResolutionExplanation {
            rule: ResolutionRule::Import,
            provider_name: ANALYZER_ID,
            provider_version: "1.0.0",
            candidates: Vec::new(),
            rejected_candidates: Vec::new(),
            rejected_total: 0,
            completeness_assumptions: vec![
                CompletenessAssumption::ValidatedNormalizedDocument,
                CompletenessAssumption::SingleGeneration,
                CompletenessAssumption::NoRepositoryExecution,
            ],
        },
    })
}

fn language_evidence(
    manifest: &ManifestLanguage,
    answer: &AnswerLanguage,
    document: &NormalizedIrDocument,
    scored: &ScoredLanguage,
    report: &ResolutionQualityReport,
    execution: LanguageExecutionEvidence,
) -> Result<ProjectSemanticLanguageEvidence, ProjectSemanticHoldoutError> {
    Ok(ProjectSemanticLanguageEvidence {
        language: manifest.language.clone(),
        observed_tier: "tier_b",
        compiler_assisted_observed: execution.provenance.compiler_assisted_observed,
        source_files: u8::try_from(manifest.sources.len())
            .map_err(|_| quality("source file count is not representable"))?,
        annotated_call_sites: report.expectations,
        observed_annotated_call_sites: scored.observed_calls,
        exact_expected: report.exact_recall.denominator,
        ambiguous_expected: report
            .candidate_recall
            .denominator
            .saturating_sub(report.exact_recall.denominator),
        unresolved_expected: report.unresolved_expected,
        exact_precision_basis_points: required_basis_points(
            report.exact_precision,
            "exact precision",
        )?,
        exact_recall_basis_points: required_basis_points(report.exact_recall, "exact recall")?,
        candidate_recall_basis_points: required_basis_points(
            report.candidate_recall,
            "candidate recall",
        )?,
        unresolved_correct: report.unresolved_correct,
        ambiguous_hidden_exact: report.ambiguous_hidden_exact,
        unexpected_decisions: report.unexpected_decisions,
        calibration_samples: report.calibration.samples,
        expected_calibration_error_milli: report
            .calibration
            .expected_calibration_error
            .ok_or_else(|| quality("calibration error is unmeasured"))?,
        capabilities: capability_evidence(manifest, answer, document, scored)?,
        analysis_coverage_complete: execution.analysis_coverage_complete,
        build_context_identity_bound: execution.provenance.build_context_identity_bound,
        provenance_complete: execution.provenance.complete,
        repeated_output_identical: execution.repeated_output_identical,
    })
}

fn capability_evidence(
    manifest: &ManifestLanguage,
    answer: &AnswerLanguage,
    document: &NormalizedIrDocument,
    scored: &ScoredLanguage,
) -> Result<ProjectSemanticCapabilityEvidence, ProjectSemanticHoldoutError> {
    let sources = manifest_source_map(manifest)?;
    let files = document
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.id))
        .collect::<BTreeMap<_, _>>();

    let import_edges = capability_score(answer.capabilities.import_edges.iter().map(|expected| {
        let Some(from_file) = files.get(expected.from_path.as_str()).copied() else {
            return (false, false);
        };
        let Some(to_file) = files.get(expected.to_path.as_str()).copied() else {
            return (false, false);
        };
        let observed = document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Imports
                && relation.subject == RelationEndpoint::File(from_file)
        });
        let correct = document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Imports
                && relation.subject == RelationEndpoint::File(from_file)
                && relation_object_file(document, relation.object) == Some(to_file)
        });
        (observed, correct)
    }))?;

    let signatures = capability_score(answer.capabilities.signatures.iter().map(|expected| {
        let entity = reviewed_entity(document, &files, &sources, expected);
        let observed = entity.is_some_and(|entity| {
            document.extensions.iter().any(|extension| {
                extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                    && decode_lexical_evidence_envelope(extension)
                        .is_ok_and(|evidence| evidence.subject() == FactRef::Entity(entity.id))
            })
        });
        let correct = entity.is_some_and(|entity| {
            document.extensions.iter().any(|extension| {
                extension.namespace == LEXICAL_EXTENSION_NAMESPACE
                    && decode_lexical_evidence_envelope(extension).is_ok_and(|evidence| {
                        evidence.subject() == FactRef::Entity(entity.id)
                            && evidence.kind() == LexicalEvidenceKind::Signature
                    })
            })
        });
        (observed, correct)
    }))?;

    let type_entities =
        capability_score(answer.capabilities.type_entities.iter().map(|expected| {
            let entity = reviewed_entity(document, &files, &sources, &expected.target);
            (
                entity.is_some(),
                entity.is_some_and(|entity| entity.kind == expected.kind),
            )
        }))?;

    let hierarchy_relations = capability_score(
        answer
            .capabilities
            .hierarchy_relations
            .iter()
            .map(|expected| {
                let subject = reviewed_entity(document, &files, &sources, &expected.subject);
                let object = reviewed_entity(document, &files, &sources, &expected.object);
                let observed = subject.is_some_and(|subject| {
                    document.relations.iter().any(|relation| {
                        relation.subject == RelationEndpoint::Entity(subject.id)
                            && matches!(
                                relation.predicate,
                                RelationPredicate::Embeds
                                    | RelationPredicate::Extends
                                    | RelationPredicate::Implements
                            )
                    })
                });
                let correct = subject.zip(object).is_some_and(|(subject, object)| {
                    document.relations.iter().any(|relation| {
                        relation.subject == RelationEndpoint::Entity(subject.id)
                            && relation.predicate == expected.predicate
                            && relation.object == RelationEndpoint::Entity(object.id)
                    })
                });
                (observed, correct)
            }),
    )?;

    let visibility = capability_score(answer.capabilities.visibility.iter().map(|expected| {
        let entity = reviewed_entity(document, &files, &sources, &expected.target);
        (
            entity.is_some(),
            entity.is_some_and(|entity| entity.visibility == expected.visibility),
        )
    }))?;

    let inferred_call_occurrences = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    let inferred_calls = ProjectSemanticCapabilityScore {
        expected: EXPECTED_CALLS,
        observed: scored.observed_calls,
        correct: u64::try_from(
            inferred_call_occurrences
                .iter()
                .filter(|occurrence| occurrence.confidence.get() < 1_000)
                .count(),
        )
        .map_err(|_| quality("inferred call count is not representable"))?,
    };
    let call_file = files
        .get(answer.call_path.as_str())
        .copied()
        .ok_or_else(|| quality(format!("{} call file is absent", manifest.language)))?;
    let call_source = sources
        .get(answer.call_path.as_str())
        .copied()
        .ok_or(ProjectSemanticHoldoutError::Fixture)?;
    let dispatch_cases = answer
        .candidates
        .iter()
        .map(|expected| {
            let occurrence = annotated_call(
                document,
                call_file,
                call_source,
                expected.call_line,
                &expected.call,
            )?;
            let observed = matches!(occurrence.target, OccurrenceTarget::Candidates { .. });
            let correct = match &occurrence.target {
                OccurrenceTarget::Candidates {
                    symbols,
                    total_count,
                    completeness,
                } => {
                    let relation_symbols = document
                        .relations
                        .iter()
                        .filter_map(|relation| {
                            (relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                                && relation.predicate == RelationPredicate::DispatchCandidate)
                                .then_some(relation.object)
                        })
                        .filter_map(|endpoint| match endpoint {
                            RelationEndpoint::Entity(symbol) => Some(symbol),
                            _ => None,
                        })
                        .collect::<BTreeSet<_>>();
                    *completeness == CoverageStatus::Unknown
                        && *total_count == u64::try_from(symbols.len()).unwrap_or(u64::MAX)
                        && relation_symbols == symbols.iter().copied().collect()
                }
                _ => false,
            };
            Ok((observed, correct))
        })
        .collect::<Result<Vec<_>, ProjectSemanticHoldoutError>>()?;
    let dispatch_candidates = capability_score(dispatch_cases.into_iter())?;

    Ok(ProjectSemanticCapabilityEvidence {
        import_edges,
        signatures,
        type_entities,
        hierarchy_relations,
        visibility,
        inferred_calls,
        dispatch_candidates,
    })
}

fn capability_score(
    cases: impl Iterator<Item = (bool, bool)>,
) -> Result<ProjectSemanticCapabilityScore, ProjectSemanticHoldoutError> {
    let mut expected = 0_u64;
    let mut observed = 0_u64;
    let mut correct = 0_u64;
    for (case_observed, case_correct) in cases {
        expected = expected
            .checked_add(1)
            .ok_or_else(|| quality("capability expected count overflowed"))?;
        observed = observed
            .checked_add(u64::from(case_observed))
            .ok_or_else(|| quality("capability observed count overflowed"))?;
        correct = correct
            .checked_add(u64::from(case_correct))
            .ok_or_else(|| quality("capability correct count overflowed"))?;
    }
    Ok(ProjectSemanticCapabilityScore {
        expected,
        observed,
        correct,
    })
}

fn reviewed_entity<'document>(
    document: &'document NormalizedIrDocument,
    files: &BTreeMap<&str, FileId>,
    sources: &BTreeMap<&str, &[u8]>,
    target: &TargetAnswer,
) -> Option<&'document rootlight_ir::EntityRecord> {
    let file = files.get(target.path.as_str()).copied()?;
    let source = sources.get(target.path.as_str()).copied()?;
    let (line_start, line_end) = line_bounds(source, target.line).ok()?;
    let mut matches = document.entities.iter().filter(|entity| {
        entity.evidence.source.as_ref().is_some_and(|source| {
            let span = source.span();
            entity.display_name == target.name
                && span.file() == file
                && span.start_byte() >= line_start
                && span.start_byte() < line_end
        })
    });
    let entity = matches.next()?;
    matches.next().is_none().then_some(entity)
}

fn relation_object_file(
    document: &NormalizedIrDocument,
    endpoint: RelationEndpoint,
) -> Option<FileId> {
    let RelationEndpoint::Entity(symbol) = endpoint else {
        return None;
    };
    document.entities.iter().find_map(|entity| {
        (entity.id == symbol && entity.kind == EntityKind::Module)
            .then(|| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .map(|source| source.span().file())
            })
            .flatten()
    })
}

fn validate_provenance(
    document: &NormalizedIrDocument,
    build_context: BuildContextIdentity,
) -> Result<ProvenanceEvidence, ProjectSemanticHoldoutError> {
    let provenance_ids = document
        .provenance
        .iter()
        .map(|provenance| provenance.id)
        .collect::<BTreeSet<_>>();
    let referenced = document
        .files
        .iter()
        .map(|record| record.provenance)
        .chain(document.entities.iter().map(|record| record.provenance))
        .chain(document.occurrences.iter().map(|record| record.provenance))
        .chain(document.relations.iter().map(|record| record.provenance))
        .all(|provenance| provenance_ids.contains(&provenance));
    let compiler_assisted_observed = document.provenance.iter().any(|provenance| {
        matches!(
            provenance.producer_kind,
            ProducerKind::Compiler | ProducerKind::Scip
        )
    });
    let build_context_identity_bound = !document.provenance.is_empty()
        && document
            .provenance
            .iter()
            .all(|provenance| provenance.build_context == build_context);
    if document.provenance.is_empty() || !referenced {
        return Err(quality("normalized records lack complete provenance"));
    }
    if compiler_assisted_observed {
        return Err(quality(
            "structural holdout unexpectedly claims compiler assistance",
        ));
    }
    if !build_context_identity_bound {
        return Err(quality(
            "provenance is not bound to the requested build context",
        ));
    }
    if !document
        .provenance
        .iter()
        .all(|provenance| provenance.tier == AnalysisTier::TierB)
    {
        return Err(quality(
            "provenance overstates the structural analysis tier",
        ));
    }
    if !document
        .provenance
        .iter()
        .all(|provenance| provenance.producer_kind == ProducerKind::Derivation)
    {
        return Err(quality("provenance is not attributed to the derivation"));
    }
    if !document
        .provenance
        .iter()
        .all(|provenance| provenance.producer.name() == ANALYZER_ID)
    {
        return Err(quality(
            "provenance is not attributed to the reviewed analyzer",
        ));
    }
    Ok(ProvenanceEvidence {
        compiler_assisted_observed,
        build_context_identity_bound,
        complete: true,
    })
}

fn validate_language_evidence(
    evidence: &ProjectSemanticLanguageEvidence,
) -> Result<(), ProjectSemanticHoldoutError> {
    if usize::from(evidence.source_files) != EXPECTED_SOURCE_FILES
        || evidence.annotated_call_sites != EXPECTED_CALLS
        || evidence.observed_annotated_call_sites != EXPECTED_CALLS
        || evidence.exact_expected != EXPECTED_EXACT_CALLS
        || evidence.ambiguous_expected != EXPECTED_AMBIGUOUS_CALLS
        || evidence.unresolved_expected != EXPECTED_UNRESOLVED_CALLS
    {
        return Err(quality(format!(
            "{} category coverage differs",
            evidence.language
        )));
    }
    if evidence.exact_precision_basis_points < MIN_PRECISION_BASIS_POINTS
        || evidence.exact_recall_basis_points < MIN_RECALL_BASIS_POINTS
        || evidence.candidate_recall_basis_points < MIN_RECALL_BASIS_POINTS
    {
        return Err(quality(format!(
            "{} precision or recall is below threshold: exact precision {}, exact recall {}, candidate recall {}",
            evidence.language,
            evidence.exact_precision_basis_points,
            evidence.exact_recall_basis_points,
            evidence.candidate_recall_basis_points,
        )));
    }
    if evidence.unresolved_correct != evidence.unresolved_expected
        || evidence.ambiguous_hidden_exact != 0
        || evidence.unexpected_decisions != 0
        || evidence.calibration_samples
            != evidence
                .exact_expected
                .saturating_add(evidence.ambiguous_expected)
        || evidence.expected_calibration_error_milli > MAX_CALIBRATION_ERROR_MILLI
    {
        return Err(quality(format!(
            "{} ambiguity, unresolved, or calibration evidence differs",
            evidence.language
        )));
    }
    let capabilities = &evidence.capabilities;
    if evidence.compiler_assisted_observed
        || !capability_complete(capabilities.import_edges)
        || !capability_complete(capabilities.signatures)
        || !capability_complete(capabilities.type_entities)
        || !capability_complete(capabilities.hierarchy_relations)
        || !capability_complete(capabilities.visibility)
        || !capability_complete(capabilities.inferred_calls)
        || !capability_complete(capabilities.dispatch_candidates)
        || !evidence.analysis_coverage_complete
        || !evidence.build_context_identity_bound
        || !evidence.provenance_complete
        || !evidence.repeated_output_identical
    {
        return Err(quality(format!(
            "{} Tier B behavioral evidence is incomplete: {capabilities:?}",
            evidence.language,
        )));
    }
    Ok(())
}

fn capability_complete(score: ProjectSemanticCapabilityScore) -> bool {
    score.expected > 0 && score.observed == score.expected && score.correct == score.expected
}

fn aggregate_evidence(
    languages: &[ProjectSemanticLanguageEvidence],
) -> Result<ProjectSemanticAggregateEvidence, ProjectSemanticHoldoutError> {
    let annotated_call_sites =
        checked_sum(languages.iter().map(|value| value.annotated_call_sites))?;
    let observed_annotated_call_sites = checked_sum(
        languages
            .iter()
            .map(|value| value.observed_annotated_call_sites),
    )?;
    let exact_expected = checked_sum(languages.iter().map(|value| value.exact_expected))?;
    let ambiguous_expected = checked_sum(languages.iter().map(|value| value.ambiguous_expected))?;
    let unresolved_expected = checked_sum(languages.iter().map(|value| value.unresolved_expected))?;
    let unresolved_correct = checked_sum(languages.iter().map(|value| value.unresolved_correct))?;
    let ambiguous_hidden_exact =
        checked_sum(languages.iter().map(|value| value.ambiguous_hidden_exact))?;
    let unexpected_decisions =
        checked_sum(languages.iter().map(|value| value.unexpected_decisions))?;
    let calibration_samples = checked_sum(languages.iter().map(|value| value.calibration_samples))?;
    Ok(ProjectSemanticAggregateEvidence {
        annotated_call_sites,
        observed_annotated_call_sites,
        exact_expected,
        ambiguous_expected,
        unresolved_expected,
        exact_precision_basis_points: weighted_basis_points(
            languages,
            |value| value.exact_precision_basis_points,
            |value| value.exact_expected,
        )?,
        exact_recall_basis_points: weighted_basis_points(
            languages,
            |value| value.exact_recall_basis_points,
            |value| value.exact_expected,
        )?,
        candidate_recall_basis_points: weighted_basis_points(
            languages,
            |value| value.candidate_recall_basis_points,
            |value| {
                value
                    .exact_expected
                    .saturating_add(value.ambiguous_expected)
            },
        )?,
        unresolved_correct,
        ambiguous_hidden_exact,
        unexpected_decisions,
        calibration_samples,
        maximum_expected_calibration_error_milli: languages
            .iter()
            .map(|value| value.expected_calibration_error_milli)
            .max()
            .unwrap_or(0),
    })
}

fn validate_evidence(
    evidence: &ProjectSemanticHoldoutEvidence,
) -> Result<(), ProjectSemanticHoldoutError> {
    if evidence
        .languages
        .iter()
        .map(|language| language.language.as_str())
        .ne(EXPECTED_LANGUAGES)
        || evidence.compiler_assisted_observed
        || evidence.compiler_build_context_observed
        || evidence.aggregate.annotated_call_sites != EXPECTED_CALLS * 5
        || evidence.aggregate.observed_annotated_call_sites != EXPECTED_CALLS * 5
        || evidence.aggregate.exact_precision_basis_points < MIN_PRECISION_BASIS_POINTS
        || evidence.aggregate.exact_recall_basis_points < MIN_RECALL_BASIS_POINTS
        || evidence.aggregate.candidate_recall_basis_points < MIN_RECALL_BASIS_POINTS
        || evidence.aggregate.unresolved_correct != evidence.aggregate.unresolved_expected
        || evidence.aggregate.ambiguous_hidden_exact != 0
        || evidence.aggregate.unexpected_decisions != 0
    {
        return Err(quality("aggregate semantic holdout evidence differs"));
    }
    Ok(())
}

fn analyze_case(
    manifest: &ManifestLanguage,
    parser: Arc<dyn ParseProvider>,
    limits: &AnalysisLimits,
) -> Result<ProjectAnalysisOutput, ProjectSemanticHoldoutError> {
    let validated_paths = manifest
        .sources
        .iter()
        .map(|source| {
            RelativePath::parse(Path::new(&source.project_path))
                .map_err(|_| ProjectSemanticHoldoutError::Corpus)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let temporary = tempdir().map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
    for (source, path) in manifest.sources.iter().zip(&validated_paths) {
        let bytes = embedded_source(&source.fixture).ok_or(ProjectSemanticHoldoutError::Fixture)?;
        let full = temporary.path().join(path.as_str());
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
        }
        fs::write(full, bytes).map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
    }

    let language = semantic_language(&manifest.language)?;
    let repository_id = RepositoryId::from_bytes([manifest.ordinal; 16]);
    let generation_id = GenerationId::from_bytes([manifest.ordinal.saturating_add(32); 20]);
    let repository = RepositoryRoot::open(repository_id, temporary.path())
        .map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
    let source_limit = u64::try_from(MAX_SOURCE_BYTES)
        .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("source limit"))?;
    let mut paths = validated_paths;
    paths.sort_by(|left, right| left.identity_bytes().cmp(right.identity_bytes()));
    let snapshots = paths
        .iter()
        .map(|path| {
            repository
                .snapshot(path, source_limit)
                .map_err(|_| ProjectSemanticHoldoutError::Corpus)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sources = snapshots
        .iter()
        .map(|snapshot| {
            let end = u64::try_from(snapshot.content().len())
                .map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
            let span = SourceSpan::new(snapshot.file(), 0, end)
                .map_err(|_| ProjectSemanticHoldoutError::Corpus)?;
            Ok(SourceRef::new(
                repository_id,
                generation_id,
                span,
                snapshot.content_hash(),
                None,
            ))
        })
        .collect::<Result<Vec<_>, ProjectSemanticHoldoutError>>()?;
    let inputs = snapshots
        .iter()
        .zip(&sources)
        .map(|(snapshot, source)| {
            Ok(ProjectSourceInput::new(
                GenerationBoundSnapshot::new(snapshot, source).map_err(|_| {
                    ProjectSemanticHoldoutError::ConfigurationAt("generation snapshot")
                })?,
                LanguageId::new(language.as_str()).map_err(|_| {
                    ProjectSemanticHoldoutError::ConfigurationAt("language identity")
                })?,
                EncodingId::utf8(),
                false,
                Vec::new(),
            ))
        })
        .collect::<Result<Vec<_>, ProjectSemanticHoldoutError>>()?;
    let manifest_bytes = format!(
        "{{\"language\":\"{}\",\"profile\":\"structural_holdout\"}}",
        language.as_str()
    );
    let build_context = build_context(&manifest.language);
    let request = ProjectAnalysisRequest::new(
        AnalysisUnitId::new(&format!("holdout.{}", language.as_str()))
            .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("analysis unit identity"))?,
        BuildTargetId::new(&format!("//holdout:{}", language.as_str()))
            .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("build target identity"))?,
        build_context,
        content_hash(manifest_bytes.as_bytes()),
        manifest_bytes.as_bytes(),
        inputs,
        AnalysisTier::TierB,
        limits,
    )
    .map_err(|error| quality(format!("{} project request: {error}", manifest.language)))?;
    let analyzer = SemanticProjectAnalyzer::new(
        language,
        parser,
        ProducerIdentity::new(
            ANALYZER_ID,
            "1.0.0",
            content_hash(language.as_str().as_bytes()),
        )
        .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("producer identity"))?,
        content_hash(PARSER_ID.as_bytes()),
        build_context,
    )
    .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("project analyzer"))?;

    execute_project_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline()?,
    )
    .map_err(|_| ProjectSemanticHoldoutError::Analysis)
}

fn load_fixture() -> Result<LoadedFixture, ProjectSemanticHoldoutError> {
    if MANIFEST_JSON.len().saturating_add(ANSWER_KEY_JSON.len()) > MAX_FIXTURE_BYTES {
        return Err(ProjectSemanticHoldoutError::Fixture);
    }
    let manifest: HoldoutManifest =
        serde_json::from_str(MANIFEST_JSON).map_err(|_| ProjectSemanticHoldoutError::Fixture)?;
    let answer_key: HoldoutAnswerKey =
        serde_json::from_str(ANSWER_KEY_JSON).map_err(|_| ProjectSemanticHoldoutError::Fixture)?;
    if manifest.schema != MANIFEST_SCHEMA
        || answer_key.schema != ANSWER_KEY_SCHEMA
        || manifest.corpus_id != CORPUS_ID
        || answer_key.corpus_id != CORPUS_ID
        || manifest.languages.len() != EXPECTED_LANGUAGES.len()
        || answer_key.languages.len() != EXPECTED_LANGUAGES.len()
    {
        return Err(ProjectSemanticHoldoutError::Fixture);
    }
    let mut digest = Sha256::new();
    digest.update(MANIFEST_JSON.as_bytes());
    digest.update(ANSWER_KEY_JSON.as_bytes());
    let mut seen = BTreeSet::new();
    for (manifest_language, answer_language) in manifest.languages.iter().zip(&answer_key.languages)
    {
        if manifest_language.language != answer_language.language
            || manifest_language.language
                != EXPECTED_LANGUAGES
                    .get(seen.len())
                    .copied()
                    .ok_or(ProjectSemanticHoldoutError::Fixture)?
            || !seen.insert(manifest_language.language.as_str())
            || manifest_language.sources.len() != EXPECTED_SOURCE_FILES
            || answer_language.exact.len()
                != usize::try_from(EXPECTED_EXACT_CALLS).unwrap_or(usize::MAX)
            || answer_language.candidates.len()
                != usize::try_from(EXPECTED_AMBIGUOUS_CALLS).unwrap_or(usize::MAX)
            || answer_language.unresolved.len()
                != usize::try_from(EXPECTED_UNRESOLVED_CALLS).unwrap_or(usize::MAX)
        {
            return Err(ProjectSemanticHoldoutError::Fixture);
        }
        let mut source_paths = BTreeSet::new();
        for source in &manifest_language.sources {
            let bytes =
                embedded_source(&source.fixture).ok_or(ProjectSemanticHoldoutError::Fixture)?;
            if bytes.is_empty()
                || bytes.len() > MAX_SOURCE_BYTES
                || !source_paths.insert(source.project_path.as_str())
                || sha256_hex(bytes) != source.sha256
            {
                return Err(ProjectSemanticHoldoutError::Fixture);
            }
            digest.update(source.fixture.as_bytes());
            digest.update(bytes);
        }
        validate_answer_key(answer_language, &source_paths)?;
    }
    Ok(LoadedFixture {
        manifest,
        answer_key,
        digest: hex_lower(&digest.finalize()),
    })
}

fn validate_answer_key(
    answer: &AnswerLanguage,
    source_paths: &BTreeSet<&str>,
) -> Result<(), ProjectSemanticHoldoutError> {
    if !source_paths.contains(answer.call_path.as_str()) {
        return Err(ProjectSemanticHoldoutError::Fixture);
    }
    let mut calls = BTreeSet::new();
    for exact in &answer.exact {
        if !calls.insert((exact.call_line, exact.call.as_str()))
            || !source_paths.contains(exact.target_path.as_str())
        {
            return Err(ProjectSemanticHoldoutError::Fixture);
        }
    }
    for candidate in &answer.candidates {
        if !calls.insert((candidate.call_line, candidate.call.as_str()))
            || candidate.targets.len() < 2
            || candidate
                .targets
                .iter()
                .any(|target| !source_paths.contains(target.path.as_str()))
        {
            return Err(ProjectSemanticHoldoutError::Fixture);
        }
    }
    for unresolved in &answer.unresolved {
        if !calls.insert((unresolved.call_line, unresolved.call.as_str())) {
            return Err(ProjectSemanticHoldoutError::Fixture);
        }
    }
    let capabilities = &answer.capabilities;
    if capabilities.import_edges.is_empty()
        || capabilities.signatures.is_empty()
        || capabilities.type_entities.is_empty()
        || capabilities.hierarchy_relations.is_empty()
        || capabilities.visibility.is_empty()
        || capabilities.import_edges.iter().any(|expected| {
            !source_paths.contains(expected.from_path.as_str())
                || !source_paths.contains(expected.to_path.as_str())
        })
        || capabilities
            .signatures
            .iter()
            .any(|target| !source_paths.contains(target.path.as_str()))
        || capabilities
            .type_entities
            .iter()
            .any(|expected| !source_paths.contains(expected.target.path.as_str()))
        || capabilities.hierarchy_relations.iter().any(|expected| {
            !source_paths.contains(expected.subject.path.as_str())
                || !source_paths.contains(expected.object.path.as_str())
        })
        || capabilities
            .visibility
            .iter()
            .any(|expected| !source_paths.contains(expected.target.path.as_str()))
    {
        return Err(ProjectSemanticHoldoutError::Fixture);
    }
    Ok(())
}

fn manifest_source_map(
    manifest: &ManifestLanguage,
) -> Result<BTreeMap<&str, &'static [u8]>, ProjectSemanticHoldoutError> {
    manifest
        .sources
        .iter()
        .map(|source| {
            embedded_source(&source.fixture)
                .map(|bytes| (source.project_path.as_str(), bytes))
                .ok_or(ProjectSemanticHoldoutError::Fixture)
        })
        .collect()
}

fn embedded_source(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "rust/dep_a.rs" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/rust/dep_a.rs")
        }
        "rust/dep_b.rs" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/rust/dep_b.rs")
        }
        "rust/main.rs" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/rust/main.rs")
        }
        "typescript/dep-a.ts" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/typescript/dep-a.ts")
        }
        "typescript/dep-b.ts" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/typescript/dep-b.ts")
        }
        "typescript/main.ts" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/typescript/main.ts")
        }
        "javascript/dep-a.js" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/javascript/dep-a.js")
        }
        "javascript/dep-b.js" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/javascript/dep-b.js")
        }
        "javascript/main.js" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/javascript/main.js")
        }
        "python/holdout_dep_a.py" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/python/holdout_dep_a.py")
        }
        "python/holdout_dep_b.py" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/python/holdout_dep_b.py")
        }
        "python/holdout_main.py" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/python/holdout_main.py")
        }
        "go/dep_a/dep.go" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/go/dep_a/dep.go")
        }
        "go/dep_b/dep.go" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/go/dep_b/dep.go")
        }
        "go/main/main.go" => {
            include_bytes!("../tests/fixtures/semantic-holdout/v1/sources/go/main/main.go")
        }
        "semantic-dispatch/typescript/dep-a.ts" => {
            include_bytes!("../tests/fixtures/semantic-dispatch/typescript/dep-a.ts")
        }
        "semantic-dispatch/typescript/dep-b.ts" => {
            include_bytes!("../tests/fixtures/semantic-dispatch/typescript/dep-b.ts")
        }
        "semantic-dispatch/typescript/main.ts" => {
            include_bytes!("../tests/fixtures/semantic-dispatch/typescript/main.ts")
        }
        _ => return None,
    })
}

fn semantic_language(
    language: &str,
) -> Result<SemanticProjectLanguage, ProjectSemanticHoldoutError> {
    match language {
        "rust" => Ok(SemanticProjectLanguage::Rust),
        "typescript" => Ok(SemanticProjectLanguage::TypeScript),
        "javascript" => Ok(SemanticProjectLanguage::JavaScript),
        "python" => Ok(SemanticProjectLanguage::Python),
        "go" => Ok(SemanticProjectLanguage::Go),
        _ => Err(ProjectSemanticHoldoutError::Fixture),
    }
}

fn line_bounds(source: &[u8], line: u32) -> Result<(u64, u64), ProjectSemanticHoldoutError> {
    if line == 0 {
        return Err(ProjectSemanticHoldoutError::Fixture);
    }
    let mut current = 1_u32;
    let mut start = 0_usize;
    for (index, byte) in source.iter().copied().enumerate() {
        if current == line && byte == b'\n' {
            return Ok((
                u64::try_from(start).map_err(|_| ProjectSemanticHoldoutError::Fixture)?,
                u64::try_from(index).map_err(|_| ProjectSemanticHoldoutError::Fixture)?,
            ));
        }
        if byte == b'\n' {
            current = current
                .checked_add(1)
                .ok_or(ProjectSemanticHoldoutError::Fixture)?;
            start = index
                .checked_add(1)
                .ok_or(ProjectSemanticHoldoutError::Fixture)?;
        }
    }
    if current == line {
        return Ok((
            u64::try_from(start).map_err(|_| ProjectSemanticHoldoutError::Fixture)?,
            u64::try_from(source.len()).map_err(|_| ProjectSemanticHoldoutError::Fixture)?,
        ));
    }
    Err(ProjectSemanticHoldoutError::Fixture)
}

fn required_basis_points(
    ratio: rootlight_resolve::QualityRatio,
    label: &str,
) -> Result<u16, ProjectSemanticHoldoutError> {
    ratio
        .basis_points()
        .ok_or_else(|| quality(format!("{label} is unmeasured")))
}

fn weighted_basis_points(
    values: &[ProjectSemanticLanguageEvidence],
    ratio: impl Fn(&ProjectSemanticLanguageEvidence) -> u16,
    weight: impl Fn(&ProjectSemanticLanguageEvidence) -> u64,
) -> Result<u16, ProjectSemanticHoldoutError> {
    let numerator = values.iter().try_fold(0_u128, |sum, value| {
        sum.checked_add(u128::from(ratio(value)) * u128::from(weight(value)))
            .ok_or_else(|| quality("weighted ratio overflowed"))
    })?;
    let denominator = values.iter().try_fold(0_u128, |sum, value| {
        sum.checked_add(u128::from(weight(value)))
            .ok_or_else(|| quality("weighted denominator overflowed"))
    })?;
    let result = numerator
        .checked_div(denominator)
        .ok_or_else(|| quality("weighted ratio is undefined"))?;
    u16::try_from(result).map_err(|_| quality("weighted ratio is not representable"))
}

fn checked_sum(mut values: impl Iterator<Item = u64>) -> Result<u64, ProjectSemanticHoldoutError> {
    values.try_fold(0_u64, |sum, value| {
        sum.checked_add(value)
            .ok_or_else(|| quality("aggregate counter overflowed"))
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn build_context(language: &str) -> BuildContextIdentity {
    BuildContextIdentity::new(content_hash(
        format!("holdout-build-context-{language}").as_bytes(),
    ))
}

fn parser_config() -> Result<RuntimeConfig, ProjectSemanticHoldoutError> {
    let settings = ParserSettings::new(4 * 1024)
        .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("parser settings"))?;
    RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        16,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("parser runtime"))
}

fn analysis_limits() -> Result<AnalysisLimits, ProjectSemanticHoldoutError> {
    let batch = BatchThresholds::new(256, 4 * 1024 * 1024, 128, 128 * 1024)
        .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("batch limits"))?;
    let stream = StreamLimits::new(
        256,
        16 * 1024,
        4 * 1024 * 1024,
        1024,
        1024 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("stream limits"))?;
    let limits = AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        256,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("analysis limits"))?;
    Ok(limits.with_project_limits(
        ProjectAnalysisLimits::new(16, 512 * 1024, 64 * 1024, 128, 128 * 1024, 256, 256)
            .map_err(|_| ProjectSemanticHoldoutError::ConfigurationAt("project limits"))?,
    ))
}

fn deadline() -> Result<Cancellation, ProjectSemanticHoldoutError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or(ProjectSemanticHoldoutError::ConfigurationAt("deadline"))?;
    Ok(Cancellation::with_deadline(deadline))
}

fn quality(detail: impl Into<String>) -> ProjectSemanticHoldoutError {
    ProjectSemanticHoldoutError::Quality(detail.into())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldoutManifest {
    schema: String,
    corpus_id: String,
    languages: Vec<ManifestLanguage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestLanguage {
    language: String,
    ordinal: u8,
    sources: Vec<ManifestSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSource {
    fixture: String,
    project_path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldoutAnswerKey {
    schema: String,
    corpus_id: String,
    languages: Vec<AnswerLanguage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerLanguage {
    language: String,
    call_path: String,
    exact: Vec<ExactAnswer>,
    candidates: Vec<CandidateAnswer>,
    unresolved: Vec<UnresolvedAnswer>,
    capabilities: CapabilityAnswer,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactAnswer {
    call: String,
    call_line: u32,
    target: String,
    target_path: String,
    target_line: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateAnswer {
    call: String,
    call_line: u32,
    targets: Vec<TargetAnswer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetAnswer {
    name: String,
    path: String,
    line: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityAnswer {
    import_edges: Vec<ImportAnswer>,
    signatures: Vec<TargetAnswer>,
    type_entities: Vec<TypeAnswer>,
    hierarchy_relations: Vec<HierarchyAnswer>,
    visibility: Vec<VisibilityAnswer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportAnswer {
    from_path: String,
    to_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeAnswer {
    #[serde(flatten)]
    target: TargetAnswer,
    kind: EntityKind,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HierarchyAnswer {
    subject: TargetAnswer,
    predicate: RelationPredicate,
    object: TargetAnswer,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VisibilityAnswer {
    #[serde(flatten)]
    target: TargetAnswer,
    visibility: EntityVisibility,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnresolvedAnswer {
    call: String,
    call_line: u32,
}

struct LoadedFixture {
    manifest: HoldoutManifest,
    answer_key: HoldoutAnswerKey,
    digest: String,
}

struct ScoredLanguage {
    batch: ResolutionBatch,
    expectations: Vec<ResolutionExpectation>,
    build_context: BuildContextIdentity,
    observed_calls: u64,
}

#[derive(Debug, Clone, Copy)]
struct ProvenanceEvidence {
    compiler_assisted_observed: bool,
    build_context_identity_bound: bool,
    complete: bool,
}

#[derive(Debug, Clone, Copy)]
struct LanguageExecutionEvidence {
    provenance: ProvenanceEvidence,
    analysis_coverage_complete: bool,
    repeated_output_identical: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_holdout_is_deterministic_bounded_and_complete() {
        let first = build_project_semantic_holdout().expect("project semantic holdout passes");
        let repeated = build_project_semantic_holdout().expect("project semantic holdout repeats");
        let first_bytes =
            encode_project_semantic_holdout(&first).expect("project semantic holdout encodes");
        let repeated_bytes =
            encode_project_semantic_holdout(&repeated).expect("repeated holdout encodes");

        assert_eq!(first, repeated);
        assert_eq!(first_bytes, repeated_bytes);
        assert!(first_bytes.len() <= PROJECT_SEMANTIC_HOLDOUT_MAX_BYTES);
        assert_eq!(first.languages.len(), EXPECTED_LANGUAGES.len());
        assert!(first.languages.iter().all(|language| {
            language.exact_precision_basis_points == 10_000
                && language.exact_recall_basis_points == 10_000
                && language.candidate_recall_basis_points == 10_000
                && language.unresolved_correct == EXPECTED_UNRESOLVED_CALLS
                && language.ambiguous_hidden_exact == 0
                && language.unexpected_decisions == 0
        }));
        let text = String::from_utf8(first_bytes).expect("holdout evidence is UTF-8");
        assert!(!text.contains("\\Users\\"));
        assert!(!text.contains("/home/"));
        assert!(!text.contains("holdout_exact"));
        assert!(!text.contains("holdout/main"));
    }

    #[test]
    fn fixture_hashes_and_answer_key_are_independently_validated() {
        let fixture = load_fixture().expect("reviewed fixture is valid");
        assert_eq!(fixture.manifest.languages.len(), EXPECTED_LANGUAGES.len());
        assert_eq!(fixture.answer_key.languages.len(), EXPECTED_LANGUAGES.len());
        assert_eq!(fixture.digest.len(), 64);
    }

    #[test]
    fn corpus_paths_are_validated_before_materialization() {
        let mut manifest: HoldoutManifest =
            serde_json::from_str(MANIFEST_JSON).expect("manifest fixture parses");
        manifest.languages[0].sources[0].project_path = "../outside.rs".to_owned();
        let parser: Arc<dyn ParseProvider> = Arc::new(
            TreeSitterProvider::new(parser_config().expect("parser config is valid"))
                .expect("parser is valid"),
        );

        let result = analyze_case(
            &manifest.languages[0],
            parser,
            &analysis_limits().expect("analysis limits are valid"),
        );

        assert!(matches!(result, Err(ProjectSemanticHoldoutError::Corpus)));
    }

    #[test]
    fn production_path_preserves_zero_one_and_many_dispatch_candidates() {
        let manifest = ManifestLanguage {
            language: "typescript".to_owned(),
            ordinal: 16,
            sources: vec![
                ManifestSource {
                    fixture: "semantic-dispatch/typescript/dep-a.ts".to_owned(),
                    project_path: "src/dep-a.ts".to_owned(),
                    sha256: String::new(),
                },
                ManifestSource {
                    fixture: "semantic-dispatch/typescript/dep-b.ts".to_owned(),
                    project_path: "src/dep-b.ts".to_owned(),
                    sha256: String::new(),
                },
                ManifestSource {
                    fixture: "semantic-dispatch/typescript/main.ts".to_owned(),
                    project_path: "src/main.ts".to_owned(),
                    sha256: String::new(),
                },
            ],
        };
        let parser: Arc<dyn ParseProvider> = Arc::new(
            TreeSitterProvider::new(parser_config().expect("parser config is valid"))
                .expect("parser is valid"),
        );
        let output = analyze_case(
            &manifest,
            parser,
            &analysis_limits().expect("analysis limits are valid"),
        )
        .expect("dispatch fixture analyzes");
        let document = output.document();
        let call = |name: &str| {
            document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::CallSite
                        && occurrence.syntactic_text_hash == content_hash(name.as_bytes())
                })
                .unwrap_or_else(|| panic!("{name} call is present"))
        };

        let direct = call("directCall");
        let OccurrenceTarget::Resolved { symbol } = direct.target else {
            panic!("namespace-qualified direct call is exact");
        };
        assert!(
            document
                .entities
                .iter()
                .any(|entity| { entity.id == symbol && entity.display_name == "directCall" })
        );
        assert!(document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(direct.id)
                && relation.predicate == RelationPredicate::Calls
        }));

        assert!(matches!(
            call("absentMethod").target,
            OccurrenceTarget::Unresolved { .. }
        ));

        let sole = call("soleMethod");
        let OccurrenceTarget::Candidates {
            symbols,
            total_count,
            completeness,
        } = &sole.target
        else {
            panic!("single dynamic target remains a candidate set");
        };
        assert_eq!(symbols.len(), 1);
        assert_eq!(*total_count, 1);
        assert_eq!(*completeness, CoverageStatus::Unknown);
        assert!(document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(sole.id)
                && relation.predicate == RelationPredicate::DispatchCandidate
        }));

        let shared = call("sharedMethod");
        let OccurrenceTarget::Candidates {
            symbols,
            total_count,
            completeness,
        } = &shared.target
        else {
            panic!("multiple dynamic targets remain a candidate set");
        };
        assert_eq!(symbols.len(), 2);
        assert_eq!(*total_count, 2);
        assert_eq!(*completeness, CoverageStatus::Unknown);
    }

    #[test]
    fn scoring_rejects_swapped_targets_hidden_ambiguity_and_missing_decisions() {
        let mut swapped = scored_rust();
        let exact = swapped
            .expectations
            .iter()
            .filter_map(|expectation| match expectation.expected {
                ExpectedResolution::Exact(symbol) => Some((expectation.occurrence, symbol)),
                _ => None,
            })
            .take(2)
            .collect::<Vec<_>>();
        let [(first_occurrence, _), (_, second_symbol)] = exact.as_slice() else {
            panic!("Rust holdout contains two exact expectations");
        };
        let decision = swapped
            .batch
            .decisions
            .iter_mut()
            .find(|decision| decision.occurrence == *first_occurrence)
            .expect("first exact decision is present");
        decision.outcome = ResolutionOutcome::Resolved {
            symbol: *second_symbol,
            confidence: Confidence::new(1_000).expect("confidence is valid"),
        };
        let swapped_report = evaluate_resolution_quality(&swapped.batch, &swapped.expectations)
            .expect("swapped score is measurable");
        assert!(
            swapped_report
                .exact_precision
                .basis_points()
                .is_some_and(|value| value < MIN_PRECISION_BASIS_POINTS)
        );
        assert!(
            swapped_report
                .exact_recall
                .basis_points()
                .is_some_and(|value| value < MIN_RECALL_BASIS_POINTS)
        );

        let mut hidden = scored_rust();
        let (candidate_occurrence, candidate_symbol) = hidden
            .expectations
            .iter()
            .find_map(|expectation| match expectation.expected {
                ExpectedResolution::CandidateContains(symbol) => {
                    Some((expectation.occurrence, symbol))
                }
                _ => None,
            })
            .expect("Rust holdout contains ambiguity");
        hidden
            .batch
            .decisions
            .iter_mut()
            .find(|decision| decision.occurrence == candidate_occurrence)
            .expect("candidate decision is present")
            .outcome = ResolutionOutcome::Resolved {
            symbol: candidate_symbol,
            confidence: Confidence::new(1_000).expect("confidence is valid"),
        };
        let hidden_report = evaluate_resolution_quality(&hidden.batch, &hidden.expectations)
            .expect("hidden ambiguity score is measurable");
        assert_eq!(hidden_report.ambiguous_hidden_exact, 1);

        let mut missing = scored_rust();
        missing.batch.decisions.pop();
        assert!(
            validate_decision_coverage(&missing.batch.decisions, &missing.expectations).is_err()
        );
    }

    #[test]
    fn go_holdout_materializes_reviewed_dispatch_methods() {
        let fixture = load_fixture().expect("reviewed fixture is valid");
        let manifest = fixture
            .manifest
            .languages
            .iter()
            .find(|language| language.language == "go")
            .expect("Go holdout exists");
        let parser: Arc<dyn ParseProvider> = Arc::new(
            TreeSitterProvider::new(parser_config().expect("parser config is valid"))
                .expect("parser is valid"),
        );
        let output = analyze_case(
            manifest,
            parser,
            &analysis_limits().expect("analysis limits are valid"),
        )
        .expect("Go holdout analyzes");
        let names = output
            .document()
            .entities
            .iter()
            .map(|entity| entity.display_name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            names.contains("FixtureNode09") && names.contains("FixtureNode10"),
            "Go dispatch methods are absent from {names:?}"
        );
    }

    fn scored_rust() -> ScoredLanguage {
        let fixture = load_fixture().expect("reviewed fixture is valid");
        let parser: Arc<dyn ParseProvider> = Arc::new(
            TreeSitterProvider::new(parser_config().expect("parser config is valid"))
                .expect("parser is valid"),
        );
        let output = analyze_case(
            &fixture.manifest.languages[0],
            parser,
            &analysis_limits().expect("analysis limits are valid"),
        )
        .expect("Rust holdout analyzes");
        score_document(
            output.document(),
            &fixture.manifest.languages[0],
            &fixture.answer_key.languages[0],
        )
        .expect("Rust holdout scores")
    }
}
