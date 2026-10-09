//! Bounded name index, scope scoring, and ambiguity-preserving decisions.
//!
//! Exact bindings require a unique candidate at or above the documented strong
//! threshold; tied or weaker evidence remains an explicit candidate set.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, Confidence, ContainerRef, CoverageStatus, EntityFlag, EntityKind, EntityRecord,
    ExtensionSupport, FileRecord, IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProvenanceRecord, validate_ir_document,
};

use crate::model::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, RejectedCandidate, RejectionReason, ResolutionBatch,
    ResolutionDecision, ResolutionError, ResolutionExplanation, ResolutionLimits,
    ResolutionOutcome, ResolutionPenalty, ResolutionPolicy, ResolutionRule, ResolutionSignal,
    ResolutionWorkEstimate, UnresolvedReason,
};

const EXACT_BINDING_THRESHOLD: u16 = 900;
const NAME_AND_LANGUAGE_SCORE: u16 = 600;
const SAME_SCOPE_BONUS: u16 = 350;
const SAME_FILE_BONUS: u16 = 300;
const IMPORTABLE_BONUS: u16 = 300;
const ANCESTOR_SCOPE_BASE_BONUS: u16 = 340;
const ANCESTOR_SCOPE_STEP_PENALTY: u16 = 20;
const UNIQUE_SCOPED_CALL_BONUS: u16 = 400;
const CROSS_FILE_PENALTY: u16 = 100;
const REPOSITORY_SCOPE_PENALTY: u16 = 50;
pub(crate) const MAX_SCOPE_DEPTH: usize = 64;

/// Language-neutral semantic candidate resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionEngine {
    pub(crate) limits: ResolutionLimits,
    pub(crate) policy: ResolutionPolicy,
}

impl ResolutionEngine {
    /// Creates a resolver with checked limits.
    #[must_use]
    pub fn new(limits: ResolutionLimits) -> Self {
        Self::with_policy(limits, ResolutionPolicy::default())
    }

    /// Creates a resolver with checked limits and an explicit promotion policy.
    #[must_use]
    pub const fn with_policy(limits: ResolutionLimits, policy: ResolutionPolicy) -> Self {
        Self { limits, policy }
    }

    /// Resolves unresolved and candidate occurrences in one normalized document.
    ///
    /// Exact results require a unique candidate with strong structural evidence.
    /// Ties, weaker matches, and truncated sets remain explicit candidates.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionError::InvalidDocument`] for invalid normalized
    /// facts, [`ResolutionError::Cancelled`] at a cooperative checkpoint, or a
    /// bounded internal-contract error when a score or count cannot be represented.
    pub fn resolve(
        &self,
        document: &NormalizedIrDocument,
        cancellation: &Cancellation,
    ) -> Result<ResolutionBatch, ResolutionError> {
        cancellation.check()?;
        validate_ir_document(document, &IrLimits::default(), &ExtensionSupport::default())
            .map_err(ResolutionError::InvalidDocument)?;

        let index = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            &document.extensions,
            &document.coverage_records,
            &document.occurrences,
            cancellation,
        )?;
        let mut work = ResolutionWorkBudget::new(self.limits.work_limit());
        work.consume_many(index.fsharp.preparation_work())?;
        let mut memo = ResolutionMemo::new(&document.occurrences, &index, cancellation)?;
        let mut decisions = Vec::new();
        for occurrence in &document.occurrences {
            cancellation.check()?;
            if index.occurrence_work(occurrence)?.is_none() {
                continue;
            }
            decisions.push(memo.resolve(self, occurrence, &index, &mut work, cancellation)?);
        }
        decisions.sort_unstable_by_key(|decision| decision.occurrence);

        Ok(ResolutionBatch {
            repository: document.repository,
            generation: document.generation,
            decisions,
        })
    }

    /// Estimates the exact occurrence and same-name candidate inspections
    /// required by a complete resolution pass.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionError::InvalidDocument`] for invalid normalized
    /// facts, [`ResolutionError::Cancelled`] at a cooperative checkpoint, or
    /// [`ResolutionError::CountOverflow`] when the required work is not
    /// representable.
    pub fn estimate_work(
        &self,
        document: &NormalizedIrDocument,
        cancellation: &Cancellation,
    ) -> Result<ResolutionWorkEstimate, ResolutionError> {
        cancellation.check()?;
        validate_ir_document(document, &IrLimits::default(), &ExtensionSupport::default())
            .map_err(ResolutionError::InvalidDocument)?;

        let index = CandidateIndex::build(
            &document.files,
            &document.entities,
            &document.provenance,
            &document.extensions,
            &document.coverage_records,
            &document.occurrences,
            cancellation,
        )?;
        let mut memo = ResolutionMemo::new(&document.occurrences, &index, cancellation)?;
        let mut required = index.fsharp.preparation_work();
        for occurrence in &document.occurrences {
            cancellation.check()?;
            let site_work = index.occurrence_work(occurrence)?.unwrap_or(0);
            required = required
                .checked_add(memo.estimated_work(occurrence, &index, site_work))
                .ok_or(ResolutionError::CountOverflow)?;
        }
        Ok(ResolutionWorkEstimate {
            required,
            limit: self.limits.work_limit(),
        })
    }

    pub(crate) fn resolve_occurrence(
        &self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        work: &mut ResolutionWorkBudget,
        cancellation: &Cancellation,
    ) -> Result<ResolutionDecision, ResolutionError> {
        if let Some(decision) = index.classic.resolve(occurrence) {
            return Ok(decision);
        }
        if let Some(decision) = index.perl.resolve(occurrence) {
            return Ok(decision);
        }
        if let Some(decision) = index.fsharp.resolve(occurrence) {
            return Ok(decision);
        }
        if let Some(entry) = index
            .groovy
            .inherited_entry(occurrence)
            .or_else(|| index.groovy.static_import_entry(occurrence))
        {
            work.consume_many(entry.selection.work)?;
        }
        if let Some(decision) = index
            .groovy
            .resolve(occurrence, self.limits.candidate_limit())?
        {
            return Ok(decision);
        }
        let file_language = index
            .files
            .get(&occurrence.file)
            .map(|file| file.language.as_str())
            .ok_or(ResolutionError::InvalidScore)?;
        let language = if matches!(file_language, "markdown" | "html") {
            // Host provenance cannot label embedded native syntax. Missing
            // namespace evidence must not fall back to a host-language match.
            occurrence
                .syntax_kind
                .split_once('.')
                .map_or("", |(language, _)| language)
        } else {
            file_language
        };
        if language == "elm"
            && occurrence.syntax_kind == "elm.import"
            && occurrence.role == OccurrenceRole::ImportUse
        {
            return self.resolve_elm_import(occurrence, index, work, cancellation);
        }
        if language == "elm"
            && occurrence.syntax_kind == "elm.type.reference"
            && occurrence.role == OccurrenceRole::TypeUse
        {
            return self.resolve_elm_local_type(occurrence, index, work, cancellation);
        }
        let example =
            (file_language == "markdown").then(|| markdown_example(occurrence.enclosing, index));
        let matlab_primary_candidate =
            language == "matlab" && matlab_candidate_reference(occurrence);
        let rule = if matlab_primary_candidate {
            ResolutionRule::MatlabPrimaryFunctionCandidate
        } else if occurrence.role == OccurrenceRole::ImportUse {
            ResolutionRule::Import
        } else {
            ResolutionRule::LexicalScope
        };
        let scoped_javascript = is_javascript_lexical_reference(occurrence, index);
        let (scoped_candidates, scope_work) = if scoped_javascript {
            index.javascript_lexical_candidates(occurrence)
        } else {
            (Vec::new(), 0)
        };
        work.consume_many(scope_work)?;
        let same_spelling = if scoped_javascript {
            scoped_candidates.as_slice()
        } else {
            index
                .by_name_hash
                .get(&occurrence.syntactic_text_hash)
                .map(Vec::as_slice)
                .unwrap_or_default()
        };
        let mut candidates = BinaryHeap::with_capacity(self.limits.candidate_limit());
        let mut rejected = Vec::new();
        let mut candidate_total = 0_u64;
        let mut rejected_total = 0_u64;
        let mut kind_rejection_total = 0_u64;
        let mut best_score = None;
        let mut best_score_count = 0_u64;

        for indexed in same_spelling {
            cancellation.check()?;
            work.consume()?;
            let entity = indexed.entity;
            if matlab_primary_candidate && !matlab_primary_function(occurrence, entity, index) {
                continue;
            }
            let parameter_call = language == "r"
                && entity.kind == EntityKind::Parameter
                && occurrence.role == OccurrenceRole::CallSite
                && occurrence.syntax_kind == "r.call.call";
            let rejection = if entity.language != language {
                Some(RejectionReason::LanguageMismatch)
            } else if !parameter_call && !kind_supports_role(entity.kind, occurrence.role) {
                Some(RejectionReason::TargetKindMismatch)
            } else if entity.language == "typescript" && entity.kind == EntityKind::TypeParameter {
                // Native generic scopes, including anonymous type signatures,
                // must establish these bindings before language-neutral scoring.
                Some(RejectionReason::MissingLexicalEvidence)
            } else if example.is_some_and(|example| {
                example.is_none()
                    || entity_source_file(entity) != Some(occurrence.file)
                    || markdown_example(Some(entity.id), index) != example
            }) || parameter_outside_r_scope(occurrence, entity, index)
            {
                Some(RejectionReason::OutsideLexicalScope)
            } else {
                None
            };
            if let Some(reason) = rejection {
                if reason == RejectionReason::TargetKindMismatch {
                    kind_rejection_total = kind_rejection_total
                        .checked_add(1)
                        .ok_or(ResolutionError::CountOverflow)?;
                }
                rejected_total = rejected_total
                    .checked_add(1)
                    .ok_or(ResolutionError::CountOverflow)?;
                if rejected.len() < self.limits.candidate_limit() {
                    rejected.push(RejectedCandidate {
                        symbol: entity.id,
                        reason,
                    });
                }
                continue;
            }
            candidate_total = candidate_total
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            let mut candidate = score_candidate(occurrence, entity, indexed.name_match, index)?;
            if matlab_primary_candidate {
                // Runtime variables and path order can override an indexed .m file.
                candidate.score = confidence(candidate.score.get().min(399))?;
                candidate
                    .penalties
                    .push(ResolutionPenalty::UnverifiedMatlabSearchPath);
            }
            if parameter_call {
                // A formal binding is not proof of the function value supplied at runtime.
                candidate.score = confidence(candidate.score.get().min(899))?;
                candidate
                    .penalties
                    .push(ResolutionPenalty::IndirectCallableBinding);
            }
            if occurrence.role == OccurrenceRole::CallSite
                && !self.policy.allows_exact_call(language)
            {
                candidate.score = confidence(candidate.score.get().min(899))?;
                candidate
                    .penalties
                    .push(ResolutionPenalty::DynamicCallUncalibrated);
            }
            match best_score {
                None => {
                    best_score = Some(candidate.score);
                    best_score_count = 1;
                }
                Some(score) => match candidate.score.cmp(&score) {
                    Ordering::Greater => {
                        best_score = Some(candidate.score);
                        best_score_count = 1;
                    }
                    Ordering::Equal => {
                        best_score_count = best_score_count
                            .checked_add(1)
                            .ok_or(ResolutionError::CountOverflow)?;
                    }
                    Ordering::Less => {}
                },
            }
            let ranked = RankedCandidate(candidate);
            if candidates.len() < self.limits.candidate_limit() {
                candidates.push(ranked);
            } else if candidates
                .peek()
                .is_some_and(|worst| candidate_precedes(&ranked.0, &worst.0))
            {
                candidates.pop();
                candidates.push(ranked);
            }
        }

        let mut candidates = candidates
            .into_iter()
            .map(|candidate| candidate.0)
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(candidate_order);

        if candidate_total == 1 && is_scoped_call_syntax(&occurrence.syntax_kind) {
            let candidate = candidates
                .first_mut()
                .ok_or(ResolutionError::InvalidScore)?;
            let entity = index
                .entities
                .get(&candidate.symbol)
                .copied()
                .ok_or(ResolutionError::InvalidScore)?;
            let occurrence_tier = index
                .provenance
                .get(&occurrence.provenance)
                .map(|provenance| provenance.tier)
                .ok_or(ResolutionError::InvalidScore)?;
            let tier_ceiling =
                confidence_ceiling(occurrence_tier).min(confidence_ceiling(entity.tier));
            candidate.score = if candidate
                .positive_signals
                .contains(&ResolutionSignal::QualifiedNameHash)
                && is_static_erlang_call_binding(occurrence, entity, index)
            {
                // Exact atom/module/arity and source ownership prove this binding;
                // the native frontend's broader Tier D completeness is unchanged.
                confidence(EXACT_BINDING_THRESHOLD)?
            } else {
                confidence(
                    candidate
                        .score
                        .get()
                        .saturating_add(UNIQUE_SCOPED_CALL_BONUS)
                        .min(tier_ceiling),
                )?
            };
            candidate
                .positive_signals
                .push(ResolutionSignal::UniqueScopedCallCandidate);
            best_score = Some(candidate.score);
        }

        candidates.sort_unstable_by(candidate_order);
        rejected.sort_unstable_by_key(|candidate| candidate.symbol);

        let top_is_unique = best_score_count == 1
            && candidates
                .first()
                .is_some_and(|candidate| Some(candidate.score) == best_score);
        let exact = candidates
            .first()
            .filter(|candidate| top_is_unique && candidate.score.get() >= EXACT_BINDING_THRESHOLD)
            .map(|candidate| (candidate.symbol, candidate.score));
        let materialized_count = candidates.len();

        let outcome = if let Some((symbol, confidence)) = exact {
            ResolutionOutcome::Resolved { symbol, confidence }
        } else if let Some(top) = candidates.first() {
            ResolutionOutcome::Candidates {
                symbols: candidates
                    .iter()
                    .map(|candidate| candidate.symbol)
                    .collect(),
                total_count: candidate_total,
                completeness: if candidate_total
                    > u64::try_from(materialized_count)
                        .map_err(|_| ResolutionError::CountOverflow)?
                {
                    CoverageStatus::Bounded
                } else {
                    CoverageStatus::Complete
                },
                confidence: top.score,
            }
        } else {
            ResolutionOutcome::Unresolved {
                reason: unresolved_reason(occurrence.role, kind_rejection_total),
                confidence: confidence(0)?,
            }
        };
        let explanation = ResolutionExplanation {
            rule,
            provider_name: RESOLVER_PROVIDER_NAME,
            provider_version: RESOLVER_PROVIDER_VERSION,
            candidates,
            rejected_candidates: rejected,
            rejected_total,
            completeness_assumptions: vec![
                CompletenessAssumption::ValidatedNormalizedDocument,
                CompletenessAssumption::SingleGeneration,
                CompletenessAssumption::CanonicalNameHash,
                CompletenessAssumption::NoRepositoryExecution,
            ],
        };

        Ok(ResolutionDecision {
            occurrence: occurrence.id,
            outcome,
            explanation,
        })
    }

    fn resolve_elm_import(
        &self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        work: &mut ResolutionWorkBudget,
        cancellation: &Cancellation,
    ) -> Result<ResolutionDecision, ResolutionError> {
        let source_length = occurrence
            .source
            .span()
            .end_byte()
            .checked_sub(occurrence.source.span().start_byte())
            .ok_or(ResolutionError::InvalidScore)?;
        let mut candidates = Vec::new();
        let mut candidate_total = 0_u64;
        for indexed in index
            .by_name_hash
            .get(&occurrence.syntactic_text_hash)
            .into_iter()
            .flatten()
        {
            cancellation.check()?;
            work.consume()?;
            let entity = indexed.entity;
            let Some(target_file) = entity_source_file(entity) else {
                continue;
            };
            if indexed.name_match != NameMatch::Canonical
                || entity.language != "elm"
                || entity.kind != EntityKind::Module
                || entity.flags.contains(&EntityFlag::Synthetic)
                || entity.container != Some(ContainerRef::File(target_file))
                || target_file == occurrence.file
                || index
                    .files
                    .get(&target_file)
                    .map(|file| file.language.as_str())
                    != Some("elm")
                || u64::try_from(entity.canonical_name.len())
                    .map_err(|_| ResolutionError::CountOverflow)?
                    != source_length
            {
                continue;
            }
            candidate_total = candidate_total
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            if candidates.len() < self.limits.candidate_limit() {
                candidates.push(CandidateExplanation {
                    symbol: entity.id,
                    score: confidence(950)?,
                    positive_signals: vec![
                        ResolutionSignal::SourceDeclaredModule,
                        ResolutionSignal::CanonicalNameHash,
                        ResolutionSignal::SameLanguage,
                        ResolutionSignal::CompatibleKind,
                    ],
                    penalties: Vec::new(),
                });
            }
        }
        let outcome = if candidate_total == 1 {
            ResolutionOutcome::Resolved {
                symbol: candidates
                    .first()
                    .ok_or(ResolutionError::InvalidScore)?
                    .symbol,
                confidence: confidence(950)?,
            }
        } else if candidate_total > 1 {
            ResolutionOutcome::Candidates {
                symbols: candidates
                    .iter()
                    .map(|candidate| candidate.symbol)
                    .collect(),
                total_count: candidate_total,
                completeness: if u64::try_from(candidates.len())
                    .map_err(|_| ResolutionError::CountOverflow)?
                    == candidate_total
                {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                },
                confidence: confidence(950)?,
            }
        } else {
            ResolutionOutcome::Unresolved {
                reason: UnresolvedReason::MissingDependency,
                confidence: confidence(0)?,
            }
        };
        Ok(ResolutionDecision {
            occurrence: occurrence.id,
            outcome,
            explanation: ResolutionExplanation {
                rule: ResolutionRule::ElmModuleImport,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates,
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::CanonicalNameHash,
                    CompletenessAssumption::NoRepositoryExecution,
                ],
            },
        })
    }

    fn resolve_elm_local_type(
        &self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        work: &mut ResolutionWorkBudget,
        cancellation: &Cancellation,
    ) -> Result<ResolutionDecision, ResolutionError> {
        let source_length = occurrence
            .source
            .span()
            .end_byte()
            .checked_sub(occurrence.source.span().start_byte())
            .ok_or(ResolutionError::InvalidScore)?;
        let mut candidates = Vec::new();
        let mut candidate_total = 0_u64;
        for indexed in index
            .by_name_hash
            .get(&occurrence.syntactic_text_hash)
            .into_iter()
            .flatten()
        {
            cancellation.check()?;
            work.consume()?;
            let entity = indexed.entity;
            if indexed.name_match != NameMatch::Canonical
                || entity.language != "elm"
                || !matches!(entity.kind, EntityKind::Enum | EntityKind::TypeAlias)
                || entity.flags.contains(&EntityFlag::Synthetic)
                || entity_source_file(entity) != Some(occurrence.file)
                || entity.container != Some(ContainerRef::File(occurrence.file))
                || u64::try_from(entity.canonical_name.len())
                    .map_err(|_| ResolutionError::CountOverflow)?
                    != source_length
            {
                continue;
            }
            candidate_total = candidate_total
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            if candidates.len() < self.limits.candidate_limit() {
                candidates.push(CandidateExplanation {
                    symbol: entity.id,
                    score: confidence(950)?,
                    positive_signals: vec![
                        ResolutionSignal::CanonicalNameHash,
                        ResolutionSignal::SameLanguage,
                        ResolutionSignal::SameFile,
                        ResolutionSignal::CompatibleKind,
                    ],
                    penalties: Vec::new(),
                });
            }
        }
        let outcome = if candidate_total == 1 {
            ResolutionOutcome::Resolved {
                symbol: candidates
                    .first()
                    .ok_or(ResolutionError::InvalidScore)?
                    .symbol,
                confidence: confidence(950)?,
            }
        } else if candidate_total > 1 {
            ResolutionOutcome::Candidates {
                symbols: candidates
                    .iter()
                    .map(|candidate| candidate.symbol)
                    .collect(),
                total_count: candidate_total,
                completeness: if u64::try_from(candidates.len())
                    .map_err(|_| ResolutionError::CountOverflow)?
                    == candidate_total
                {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                },
                confidence: confidence(950)?,
            }
        } else {
            ResolutionOutcome::Unresolved {
                reason: UnresolvedReason::NoCandidate,
                confidence: confidence(0)?,
            }
        };
        Ok(ResolutionDecision {
            occurrence: occurrence.id,
            outcome,
            explanation: ResolutionExplanation {
                rule: ResolutionRule::ElmLocalType,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates,
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::CanonicalNameHash,
                    CompletenessAssumption::NoRepositoryExecution,
                ],
            },
        })
    }
}

fn candidate_order(left: &CandidateExplanation, right: &CandidateExplanation) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.symbol.cmp(&right.symbol))
}

fn candidate_precedes(left: &CandidateExplanation, right: &CandidateExplanation) -> bool {
    candidate_order(left, right) == Ordering::Less
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RankedCandidate(CandidateExplanation);

impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .score
            .cmp(&self.0.score)
            .then_with(|| self.0.symbol.cmp(&other.0.symbol))
    }
}

impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ResolutionContextKey {
    file: FileId,
    enclosing: Option<SymbolId>,
    spelling: ContentHash,
    provenance: FactId,
    role: OccurrenceRole,
    syntax_kind: usize,
}

impl ResolutionContextKey {
    fn for_occurrence(occurrence: &OccurrenceRecord, index: &CandidateIndex<'_>) -> Option<Self> {
        if !Self::reusable(occurrence, index) {
            return None;
        }
        let syntax_kind = *index.syntax_kinds.get(occurrence.syntax_kind.as_str())?;
        Some(Self {
            file: occurrence.file,
            enclosing: occurrence.enclosing,
            spelling: occurrence.syntactic_text_hash,
            provenance: occurrence.provenance,
            role: occurrence.role,
            syntax_kind,
        })
    }

    fn reusable(occurrence: &OccurrenceRecord, index: &CandidateIndex<'_>) -> bool {
        if matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            || !resolvable_occurrence(occurrence)
            || !index
                .by_name_hash
                .contains_key(&occurrence.syntactic_text_hash)
            || is_javascript_lexical_reference(occurrence, index)
            || index.classic.proof(occurrence).is_some()
            || index.perl.eligible(occurrence)
            || index.fsharp.eligible(occurrence)
            || index.groovy.proof(occurrence).is_some()
            || index.groovy.inherited_entry(occurrence).is_some()
            || index.groovy.static_import_entry(occurrence).is_some()
        {
            return false;
        }
        let Some(file) = index.files.get(&occurrence.file) else {
            return false;
        };
        let language = if matches!(file.language.as_str(), "markdown" | "html") {
            occurrence
                .syntax_kind
                .split_once('.')
                .map_or("", |(language, _)| language)
        } else {
            file.language.as_str()
        };
        // Native routes may inspect source length or occurrence-owned evidence.
        // Only ordinary scoring is independent of the omitted source span and ID.
        !((language == "elm"
            && ((occurrence.syntax_kind == "elm.import"
                && occurrence.role == OccurrenceRole::ImportUse)
                || (occurrence.syntax_kind == "elm.type.reference"
                    && occurrence.role == OccurrenceRole::TypeUse)))
            || (language == "matlab" && matlab_candidate_reference(occurrence))
            || (language == "erlang" && occurrence.syntax_kind == "erlang.function.scoped_call"))
    }
}

pub(crate) struct ResolutionMemo {
    repeated: BTreeSet<ResolutionContextKey>,
    estimated: BTreeSet<ResolutionContextKey>,
    decisions: BTreeMap<ResolutionContextKey, ResolutionDecision>,
}

impl ResolutionMemo {
    pub(crate) fn new(
        occurrences: &[OccurrenceRecord],
        index: &CandidateIndex<'_>,
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut seen = BTreeSet::new();
        let mut repeated = BTreeSet::new();
        for occurrence in occurrences {
            cancellation.check()?;
            if let Some(key) = ResolutionContextKey::for_occurrence(occurrence, index)
                && !seen.insert(key)
            {
                repeated.insert(key);
            }
        }
        Ok(Self {
            repeated,
            estimated: BTreeSet::new(),
            decisions: BTreeMap::new(),
        })
    }

    pub(crate) fn estimated_work(
        &mut self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        site_work: usize,
    ) -> usize {
        if site_work > 0
            && let Some(key) = ResolutionContextKey::for_occurrence(occurrence, index)
            && self.repeated.contains(&key)
            && !self.estimated.insert(key)
        {
            return 1;
        }
        site_work
    }

    pub(crate) fn repeated_key(
        &self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
    ) -> Option<ResolutionContextKey> {
        let key = ResolutionContextKey::for_occurrence(occurrence, index)?;
        self.repeated.contains(&key).then_some(key)
    }

    pub(crate) fn resolve(
        &mut self,
        engine: &ResolutionEngine,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        work: &mut ResolutionWorkBudget,
        cancellation: &Cancellation,
    ) -> Result<ResolutionDecision, ResolutionError> {
        work.consume()?;
        if let Some(key) = ResolutionContextKey::for_occurrence(occurrence, index)
            && self.repeated.contains(&key)
        {
            if let Some(decision) = self.decisions.get(&key) {
                let mut decision = decision.clone();
                decision.occurrence = occurrence.id;
                return Ok(decision);
            }
            // Scoring depends on this context, not the source span or fact ID.
            let decision = engine.resolve_occurrence(occurrence, index, work, cancellation)?;
            self.decisions.insert(key, decision.clone());
            return Ok(decision);
        }
        engine.resolve_occurrence(occurrence, index, work, cancellation)
    }
}

pub(crate) struct ResolutionWorkBudget {
    remaining: usize,
    maximum: usize,
}

impl ResolutionWorkBudget {
    pub(crate) const fn new(maximum: usize) -> Self {
        Self {
            remaining: maximum,
            maximum,
        }
    }

    pub(crate) fn consume(&mut self) -> Result<(), ResolutionError> {
        self.consume_many(1)
    }

    pub(crate) fn consume_many(&mut self, amount: usize) -> Result<(), ResolutionError> {
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or(ResolutionError::WorkLimit {
                maximum: self.maximum,
            })?;
        Ok(())
    }

    pub(crate) const fn consumed(&self) -> usize {
        self.maximum - self.remaining
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.remaining
    }
}

impl Default for ResolutionEngine {
    fn default() -> Self {
        Self::new(ResolutionLimits::default())
    }
}

pub(crate) struct CandidateIndex<'a> {
    pub(crate) classic: crate::classic::ClassicIndex<'a>,
    pub(crate) groovy: crate::groovy::GroovyIndex<'a>,
    pub(crate) perl: crate::perl::PerlIndex<'a>,
    pub(crate) fsharp: crate::fsharp::FSharpIndex<'a>,
    by_name_hash: BTreeMap<ContentHash, Vec<IndexedCandidate<'a>>>,
    javascript_by_scope: BTreeMap<(ContentHash, FileId, ContainerRef), Vec<IndexedCandidate<'a>>>,
    pub(crate) entities: BTreeMap<SymbolId, &'a EntityRecord>,
    pub(crate) files: BTreeMap<FileId, &'a FileRecord>,
    pub(crate) provenance: BTreeMap<FactId, &'a ProvenanceRecord>,
    syntax_kinds: BTreeMap<String, usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NameMatch {
    Canonical,
    Display,
    Qualified,
}

#[derive(Clone, Copy)]
struct IndexedCandidate<'a> {
    entity: &'a EntityRecord,
    name_match: NameMatch,
}

impl<'a> CandidateIndex<'a> {
    pub(crate) fn build(
        file_records: &'a [FileRecord],
        entity_records: &'a [EntityRecord],
        provenance_records: &'a [ProvenanceRecord],
        extension_records: &'a [rootlight_ir::ExtensionEnvelope],
        coverage_records: &'a [rootlight_ir::CoverageRecord],
        occurrence_records: &[OccurrenceRecord],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut by_name_hash = BTreeMap::<ContentHash, Vec<IndexedCandidate<'_>>>::new();
        let mut javascript_by_scope = BTreeMap::new();
        let mut entities = BTreeMap::new();
        let mut files = BTreeMap::new();
        let mut provenance = BTreeMap::new();
        for file in file_records {
            cancellation.check()?;
            files.insert(file.id, file);
        }
        for entity in entity_records {
            cancellation.check()?;
            entities.insert(entity.id, entity);
            index_name(
                &mut by_name_hash,
                entity.canonical_name.as_bytes(),
                entity,
                NameMatch::Canonical,
            );
            index_name(
                &mut by_name_hash,
                entity.display_name.as_bytes(),
                entity,
                NameMatch::Display,
            );
            index_name(
                &mut by_name_hash,
                entity.qualified_name.as_bytes(),
                entity,
                NameMatch::Qualified,
            );
            if entity.language == "javascript"
                && let Some(file) = entity_source_file(entity)
            {
                // Bare names cannot bind declarations in sibling functions or
                // unrelated files without source-owned import evidence.
                let owner = match entity.container {
                    Some(ContainerRef::Entity(parent)) => ContainerRef::Entity(parent),
                    Some(ContainerRef::File(file)) => ContainerRef::File(file),
                    Some(ContainerRef::Repository(_)) | None => ContainerRef::File(file),
                };
                for (name, name_match) in [
                    (entity.canonical_name.as_bytes(), NameMatch::Canonical),
                    (entity.display_name.as_bytes(), NameMatch::Display),
                    (entity.qualified_name.as_bytes(), NameMatch::Qualified),
                ] {
                    let indexed = IndexedCandidate { entity, name_match };
                    javascript_by_scope
                        .entry((content_hash(name), file, owner))
                        .or_insert_with(Vec::new)
                        .push(indexed);
                }
            }
        }
        for record in provenance_records {
            cancellation.check()?;
            provenance.insert(record.id, record);
        }
        for entries in by_name_hash.values_mut() {
            entries.sort_unstable_by_key(|entry| (entry.entity.id, entry.name_match));
            entries.dedup_by_key(|entry| entry.entity.id);
        }
        for entries in javascript_by_scope.values_mut() {
            entries.sort_unstable_by_key(|entry| (entry.entity.id, entry.name_match));
            entries.dedup_by_key(|entry| entry.entity.id);
        }
        let mut index = Self {
            classic: crate::classic::ClassicIndex::default(),
            groovy: crate::groovy::GroovyIndex::build(
                &files,
                &entities,
                &provenance,
                coverage_records,
                extension_records,
                occurrence_records,
                cancellation,
            )?,
            perl: crate::perl::PerlIndex::build(
                &files,
                &entities,
                &provenance,
                extension_records,
                cancellation,
            )?,
            fsharp: crate::fsharp::FSharpIndex::build(
                &files,
                &entities,
                &provenance,
                extension_records,
                cancellation,
            )?,
            by_name_hash,
            javascript_by_scope,
            entities,
            files,
            provenance,
            syntax_kinds: BTreeMap::new(),
        };
        for occurrence in occurrence_records {
            cancellation.check()?;
            if ResolutionContextKey::reusable(occurrence, &index)
                && !index
                    .syntax_kinds
                    .contains_key(occurrence.syntax_kind.as_str())
            {
                // Own each distinct label so application can mutate occurrences;
                // full-string lookup preserves identity without hash collisions.
                index.syntax_kinds.insert(occurrence.syntax_kind.clone(), 0);
            }
        }
        for (token, value) in index.syntax_kinds.values_mut().enumerate() {
            cancellation.check()?;
            *value = token;
        }
        Ok(index)
    }

    pub(crate) fn occurrence_work(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Result<Option<usize>, ResolutionError> {
        if !matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            && let Some(entry) = self
                .groovy
                .inherited_entry(occurrence)
                .or_else(|| self.groovy.static_import_entry(occurrence))
        {
            return entry
                .selection
                .work
                .checked_add(1)
                .map(Some)
                .ok_or(ResolutionError::CountOverflow);
        }
        if !matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            && (self.perl.eligible(occurrence)
                || self.fsharp.eligible(occurrence)
                || self.groovy.proof(occurrence).is_some()
                || self.classic.proof(occurrence).is_some())
        {
            return Ok(Some(1));
        }
        if matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            || !resolvable_occurrence(occurrence)
        {
            return Ok(None);
        }
        if matlab_candidate_reference(occurrence)
            && (self
                .files
                .get(&occurrence.file)
                .is_none_or(|file| file.language != "matlab")
                || !self
                    .by_name_hash
                    .get(&occurrence.syntactic_text_hash)
                    .is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| matlab_primary_function(occurrence, entry.entity, self))
                    }))
        {
            return Ok(None);
        }
        if is_javascript_lexical_reference(occurrence, self) {
            let (candidates, scope_work) = self.javascript_lexical_candidates(occurrence);
            return 1_usize
                .checked_add(scope_work)
                .and_then(|work| work.checked_add(candidates.len()))
                .map(Some)
                .ok_or(ResolutionError::CountOverflow);
        }
        1_usize
            .checked_add(
                self.by_name_hash
                    .get(&occurrence.syntactic_text_hash)
                    .map_or(0, Vec::len),
            )
            .map(Some)
            .ok_or(ResolutionError::CountOverflow)
    }

    fn javascript_lexical_candidates(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> (Vec<IndexedCandidate<'a>>, usize) {
        let mut candidates = Vec::new();
        let hash = occurrence.syntactic_text_hash;
        if let Some(entries) = self.javascript_by_scope.get(&(
            hash,
            occurrence.file,
            ContainerRef::File(occurrence.file),
        )) {
            candidates.extend_from_slice(entries);
        }
        let mut scope_work = 0;
        let mut current = occurrence.enclosing;
        let mut visited = BTreeSet::new();
        for _ in 0..MAX_SCOPE_DEPTH {
            let Some(symbol) = current else { break };
            if !visited.insert(symbol) {
                break;
            }
            scope_work += 1;
            if let Some(entries) =
                self.javascript_by_scope
                    .get(&(hash, occurrence.file, ContainerRef::Entity(symbol)))
            {
                candidates.extend_from_slice(entries);
            }
            current = self.entities.get(&symbol).and_then(|entity| {
                if let Some(ContainerRef::Entity(parent)) = entity.container {
                    Some(parent)
                } else {
                    None
                }
            });
        }
        candidates.sort_unstable_by_key(|entry| (entry.entity.id, entry.name_match));
        candidates.dedup_by_key(|entry| entry.entity.id);
        (candidates, scope_work)
    }
}

fn is_javascript_lexical_reference(
    occurrence: &OccurrenceRecord,
    index: &CandidateIndex<'_>,
) -> bool {
    occurrence.role == OccurrenceRole::Reference
        && occurrence.syntax_kind == "javascript.identifier.reference"
        && index
            .files
            .get(&occurrence.file)
            .is_some_and(|file| file.language == "javascript")
}

fn score_candidate(
    occurrence: &OccurrenceRecord,
    entity: &EntityRecord,
    name_match: NameMatch,
    index: &CandidateIndex<'_>,
) -> Result<CandidateExplanation, ResolutionError> {
    let mut score = NAME_AND_LANGUAGE_SCORE;
    let mut positive_signals = vec![
        name_match.signal(),
        ResolutionSignal::SameLanguage,
        ResolutionSignal::CompatibleKind,
    ];
    let mut penalties = Vec::new();

    if occurrence.role == OccurrenceRole::ImportUse {
        score = score.saturating_add(IMPORTABLE_BONUS);
        positive_signals.push(ResolutionSignal::ImportableDeclaration);
    } else if occurrence.enclosing == Some(entity.id) {
        score = score.saturating_add(SAME_SCOPE_BONUS);
        positive_signals.push(ResolutionSignal::EnclosingEntity);
    } else if let Some(depth) = declaring_scope_depth(occurrence, entity, index) {
        let depth = u16::try_from(depth).map_err(|_| ResolutionError::InvalidScore)?;
        let deduction = depth.saturating_mul(ANCESTOR_SCOPE_STEP_PENALTY);
        let bonus = ANCESTOR_SCOPE_BASE_BONUS.saturating_sub(deduction);
        score = score.saturating_add(bonus);
        if depth == 0 {
            positive_signals.push(ResolutionSignal::SameScope);
        } else {
            positive_signals.push(ResolutionSignal::AncestorScope { depth });
        }
    } else if entity_source_file(entity) == Some(occurrence.file)
        || entity.container == Some(ContainerRef::File(occurrence.file))
    {
        score = score.saturating_add(SAME_FILE_BONUS);
        positive_signals.push(ResolutionSignal::SameFile);
    } else if matches!(entity.container, None | Some(ContainerRef::Repository(_))) {
        score = score.saturating_sub(REPOSITORY_SCOPE_PENALTY);
        penalties.push(ResolutionPenalty::RepositoryScope);
    } else {
        score = score.saturating_sub(CROSS_FILE_PENALTY);
        penalties.push(ResolutionPenalty::CrossFile);
    }

    let occurrence_tier = index
        .provenance
        .get(&occurrence.provenance)
        .map(|provenance| provenance.tier)
        .ok_or(ResolutionError::InvalidScore)?;
    let tier_ceiling = confidence_ceiling(occurrence_tier).min(confidence_ceiling(entity.tier));

    Ok(CandidateExplanation {
        symbol: entity.id,
        score: confidence(score.min(tier_ceiling))?,
        positive_signals,
        penalties,
    })
}

fn is_scoped_call_syntax(syntax_kind: &str) -> bool {
    syntax_kind.ends_with(".scoped_call")
}

fn is_static_erlang_call_binding(
    occurrence: &OccurrenceRecord,
    target: &EntityRecord,
    index: &CandidateIndex<'_>,
) -> bool {
    if occurrence.role != OccurrenceRole::CallSite
        || occurrence.syntax_kind != "erlang.function.scoped_call"
        || index
            .files
            .get(&occurrence.file)
            .is_none_or(|file| file.language != "erlang")
        || target.language != "erlang"
        || target.kind != EntityKind::Function
    {
        return false;
    }
    let Some(caller) = occurrence.enclosing.and_then(|id| index.entities.get(&id)) else {
        return false;
    };
    let Some(caller_source) = caller.evidence.source.as_ref() else {
        return false;
    };
    if caller.language != "erlang"
        || caller.kind != EntityKind::Function
        || caller_source.span().file() != occurrence.file
        || caller_source.content_hash() != occurrence.source.content_hash()
        || caller_source.span().start_byte() > occurrence.source.span().start_byte()
        || caller_source.span().end_byte() < occurrence.source.span().end_byte()
    {
        return false;
    }
    let Some(ContainerRef::Entity(module_id)) = target.container else {
        return false;
    };
    let Some(module) = index.entities.get(&module_id) else {
        return false;
    };
    let (Some(module_source), Some(target_source)) = (
        module.evidence.source.as_ref(),
        target.evidence.source.as_ref(),
    ) else {
        return false;
    };
    module.language == "erlang"
        && module.kind == EntityKind::Module
        && module_source.span().file() == target_source.span().file()
        && module_source.content_hash() == target_source.content_hash()
        && target
            .qualified_name
            .strip_prefix(&module.canonical_name)
            .and_then(|name| name.strip_prefix("::"))
            == Some(target.canonical_name.as_str())
}

impl NameMatch {
    fn signal(self) -> ResolutionSignal {
        match self {
            Self::Canonical => ResolutionSignal::CanonicalNameHash,
            Self::Display => ResolutionSignal::DisplayNameHash,
            Self::Qualified => ResolutionSignal::QualifiedNameHash,
        }
    }
}

fn index_name<'a>(
    index: &mut BTreeMap<ContentHash, Vec<IndexedCandidate<'a>>>,
    name: &[u8],
    entity: &'a EntityRecord,
    name_match: NameMatch,
) {
    index
        .entry(content_hash(name))
        .or_default()
        .push(IndexedCandidate { entity, name_match });
}

fn declaring_scope_depth(
    occurrence: &OccurrenceRecord,
    candidate: &EntityRecord,
    index: &CandidateIndex<'_>,
) -> Option<usize> {
    let target_container = candidate.container?;
    let mut current = occurrence.enclosing;
    for depth in 0..MAX_SCOPE_DEPTH {
        let scope = current?;
        if target_container == ContainerRef::Entity(scope) {
            return Some(depth);
        }
        current = index.entities.get(&scope).and_then(|entity| {
            if let Some(ContainerRef::Entity(parent)) = entity.container {
                Some(parent)
            } else {
                None
            }
        });
    }
    None
}

fn markdown_example(mut current: Option<SymbolId>, index: &CandidateIndex<'_>) -> Option<SymbolId> {
    for _ in 0..MAX_SCOPE_DEPTH {
        let entity = index.entities.get(&current?)?;
        let parent = match entity.container {
            Some(ContainerRef::Entity(parent)) => parent,
            _ => return None,
        };
        // A synthetic nested host module owns an authored code example. The
        // file module itself is not a shared lexical scope between examples.
        if entity.language == "markdown"
            && entity.kind == EntityKind::Module
            && entity.flags.contains(&rootlight_ir::EntityFlag::Synthetic)
        {
            return Some(entity.id);
        }
        current = Some(parent);
    }
    None
}

fn parameter_outside_r_scope(
    occurrence: &OccurrenceRecord,
    candidate: &EntityRecord,
    index: &CandidateIndex<'_>,
) -> bool {
    if candidate.language != "r"
        || candidate.kind != EntityKind::Parameter
        || !matches!(
            occurrence.syntax_kind.as_str(),
            "r.identifier.reference" | "r.call.call"
        )
    {
        return false;
    }
    let Some(ContainerRef::Entity(owner)) = candidate.container else {
        return false;
    };
    let mut current = occurrence.enclosing;
    for _ in 0..MAX_SCOPE_DEPTH {
        let Some(scope) = current else {
            return true;
        };
        if scope == owner {
            return false;
        }
        let Some(entity) = index.entities.get(&scope) else {
            return false;
        };
        current = match entity.container {
            Some(ContainerRef::Entity(parent)) => Some(parent),
            _ => None,
        };
    }
    // Exhausting the bounded walk is uncertainty, not evidence of invisibility.
    false
}

fn entity_source_file(entity: &EntityRecord) -> Option<FileId> {
    entity
        .evidence
        .source
        .as_ref()
        .map(|source| source.span().file())
}

fn confidence(score: u16) -> Result<Confidence, ResolutionError> {
    Confidence::new(score).map_err(|_| ResolutionError::InvalidScore)
}

fn confidence_ceiling(tier: AnalysisTier) -> u16 {
    match tier {
        AnalysisTier::TierA => 1_000,
        AnalysisTier::TierB => 999,
        AnalysisTier::TierC => 699,
        AnalysisTier::TierD => 399,
        _ => 399,
    }
}

fn unresolved_reason(role: OccurrenceRole, kind_rejection_total: u64) -> UnresolvedReason {
    if kind_rejection_total > 0 {
        UnresolvedReason::UnsupportedTargetKind
    } else if role == OccurrenceRole::ImportUse {
        UnresolvedReason::MissingDependency
    } else {
        UnresolvedReason::NoCandidate
    }
}

pub(crate) fn resolvable_occurrence(occurrence: &OccurrenceRecord) -> bool {
    // Common Lisp readtable, package and function/macro bindings need
    // language-owned proof; written qualification does not establish a target.
    if occurrence.syntax_kind.starts_with("common_lisp.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Written qualifiers, overloads and accessor names do not prove Pascal
    // targets. Preserve unresolved source until language-owned binding exists.
    if occurrence.syntax_kind.starts_with("pascal.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Apex member/overload/platform targets require language-owned proof;
    // spelling scores cannot supply a target the adapter left unresolved.
    if occurrence.syntax_kind.starts_with("apex.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Macro expansion, FCMP environments and dataset runtime identity require
    // SAS-owned proof; a matching source spelling cannot establish those targets.
    if occurrence.syntax_kind.starts_with("sas.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Package/schema lookup, overload selection and call-or-index syntax need
    // PL/SQL-owned evidence; spelling scores cannot establish those targets.
    if occurrence.syntax_kind.starts_with("plsql.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Fortran USE association, case-insensitive binding and call-or-index
    // disambiguation require language-owned proof, not same-spelling scoring.
    if occurrence.syntax_kind.starts_with("fortran.")
        && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    {
        return false;
    }
    // Duplicate local type declarations can collapse to one durable identity.
    // The parser retains the written ambiguity, which name scoring must not erase.
    if occurrence.syntax_kind == "elm.ambiguous_type.reference" {
        return false;
    }
    // YAML serialization bindings depend on source order within one document.
    // A name/scope score cannot recover a binding the adapter could not prove.
    if occurrence.syntax_kind == "yaml.alias.reference" {
        return false;
    }
    // Markdown labels bind within their document; destinations need URI/path
    // semantics. A code symbol with the same spelling proves neither target.
    if occurrence.syntax_kind.starts_with("markdown.") {
        return false;
    }
    // Nix lexical identity belongs to the adapter's recursive environments.
    // Remaining reads, attribute selections and calls need evidence that a
    // spelling-only score cannot supply, including correct inherit visibility.
    if occurrence.syntax_kind.starts_with("nix.") {
        return false;
    }
    // Haskell imports, type classes and local pattern bindings need lexical and
    // type evidence. Matching a written name cannot establish those targets.
    if occurrence.syntax_kind.starts_with("haskell.") {
        return false;
    }
    // F# open declarations and member/type targets require language-owned name
    // resolution. A spelling score cannot establish those bindings.
    if occurrence.syntax_kind.starts_with("fsharp.") {
        return false;
    }
    // COBOL storage qualification, CALL/PERFORM targets and COPY inputs need
    // language-owned evidence; matching a written name proves none of them.
    if occurrence.syntax_kind.starts_with("cobol.") {
        return false;
    }
    // MATLAB applications can index arrays or call functions. Only source-owned
    // primary function files may be offered as uncertain cross-file candidates.
    if occurrence.syntax_kind.starts_with("matlab.") {
        return matlab_candidate_reference(occurrence);
    }
    // VBA names bind case-insensitively inside language-owned environments.
    // Exact-spelling scores cannot recover a rejected local or dynamic target.
    if occurrence.syntax_kind.starts_with("vba.") {
        return false;
    }
    // Perl sigils, package context and runtime imports require language-owned
    // binding evidence; identical spelling alone does not establish a target.
    if occurrence.syntax_kind.starts_with("perl.") {
        return false;
    }
    // Groovy lexical and generic visibility is established by the adapter.
    // Name scoring cannot recover a rejected static binding or dynamic receiver.
    if occurrence.syntax_kind.starts_with("groovy.") {
        return false;
    }
    // SCSS mixins, selectors, and module URLs require Sass-owned visibility
    // and selector semantics. A same-spelling symbol is not binding evidence.
    if occurrence.syntax_kind.starts_with("scss.") {
        return false;
    }
    // ECMAScript member leaves require their receiver's module/type evidence.
    // Re-scoring a source-backed adapter gap by spelling would invent a target.
    if matches!(
        occurrence.syntax_kind.as_str(),
        "javascript.member_name.reference"
            | "typescript.member_name.reference"
            | "typescript.type_member_name.reference"
            | "typescript.type_namespace_member.reference"
            | "typescript.type_query_member_name.reference"
            | "typescript.type_namespace_root.reference"
            | "javascript.unavailable_name.reference"
            | "javascript.unavailable_name.call"
            | "typescript.unavailable_name.reference"
            | "typescript.unavailable_name.call"
    ) {
        return false;
    }
    // R namespaces, object members and computed callees need their own environment
    // evidence. A matching local spelling is not even a justified candidate.
    if matches!(
        occurrence.syntax_kind.as_str(),
        "r.namespace_name.reference"
            | "r.namespace_call.call"
            | "r.member_name.reference"
            | "r.member_call.call"
            | "r.computed_call.call"
            | "r.unavailable_name.reference"
            | "r.unavailable_name.call"
    ) {
        return false;
    }
    !matches!(
        occurrence.role,
        OccurrenceRole::Definition
            | OccurrenceRole::Declaration
            | OccurrenceRole::Documentation
            | OccurrenceRole::StringEvidence
    )
}

fn matlab_candidate_reference(occurrence: &OccurrenceRecord) -> bool {
    occurrence.role == OccurrenceRole::Reference
        && matches!(
            occurrence.syntax_kind.as_str(),
            "matlab.identifier.reference" | "matlab.unqualified_function_handle_name.reference"
        )
}

fn matlab_primary_function(
    occurrence: &OccurrenceRecord,
    entity: &EntityRecord,
    index: &CandidateIndex<'_>,
) -> bool {
    if entity.language != "matlab" || entity.kind != EntityKind::Function {
        return false;
    }
    let Some(ContainerRef::Entity(module_id)) = entity.container else {
        return false;
    };
    let Some(module) = index.entities.get(&module_id) else {
        return false;
    };
    let Some(ContainerRef::File(file_id)) = module.container else {
        return false;
    };
    if module.kind != EntityKind::Module
        || module.language != "matlab"
        || file_id == occurrence.file
        || entity_source_file(module) != Some(file_id)
        || entity_source_file(entity) != Some(file_id)
    {
        return false;
    }
    let Some(file) = index.files.get(&file_id) else {
        return false;
    };
    if file.language != "matlab" {
        return false;
    }
    let Some(stem) = file
        .path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".m"))
    else {
        return false;
    };
    // Package, class and private directories have their own visibility rules.
    !file
        .path
        .split('/')
        .any(|segment| segment == "private" || segment.starts_with('+') || segment.starts_with('@'))
        && stem == entity.canonical_name
}

fn kind_supports_role(kind: EntityKind, role: OccurrenceRole) -> bool {
    match role {
        OccurrenceRole::CallSite => matches!(
            kind,
            EntityKind::Function
                | EntityKind::Method
                | EntityKind::Constructor
                | EntityKind::Closure
                | EntityKind::ExternalSymbol
        ),
        OccurrenceRole::TypeUse
        | OccurrenceRole::InheritanceUse
        | OccurrenceRole::ImplementationUse => matches!(
            kind,
            EntityKind::Class
                | EntityKind::Struct
                | EntityKind::Enum
                | EntityKind::Union
                | EntityKind::TypeAlias
                | EntityKind::Trait
                | EntityKind::Interface
                | EntityKind::Protocol
                | EntityKind::TypeParameter
                | EntityKind::ExternalSymbol
        ),
        OccurrenceRole::ImportUse => matches!(
            kind,
            EntityKind::Package
                | EntityKind::BuildTarget
                | EntityKind::Directory
                | EntityKind::File
                | EntityKind::Module
                | EntityKind::Namespace
                | EntityKind::Import
                | EntityKind::Export
                | EntityKind::ExternalSymbol
        ),
        OccurrenceRole::RouteUse => matches!(
            kind,
            EntityKind::Route
                | EntityKind::Service
                | EntityKind::Function
                | EntityKind::Method
                | EntityKind::ExternalSymbol
        ),
        OccurrenceRole::TestUse => matches!(
            kind,
            EntityKind::Test
                | EntityKind::Function
                | EntityKind::Method
                | EntityKind::ExternalSymbol
        ),
        OccurrenceRole::Definition
        | OccurrenceRole::Declaration
        | OccurrenceRole::Documentation
        | OccurrenceRole::StringEvidence => false,
        OccurrenceRole::Reference
        | OccurrenceRole::Write
        | OccurrenceRole::Read
        | OccurrenceRole::DecoratorUse
        | OccurrenceRole::MacroUse => !matches!(
            kind,
            EntityKind::Repository
                | EntityKind::Worktree
                | EntityKind::Directory
                | EntityKind::Commit
                | EntityKind::Change
                | EntityKind::CommunityView
        ),
    }
}

#[cfg(test)]
mod source_owned_target_tests {
    use rootlight_ids::{GenerationId, RepositoryId};
    use rootlight_ir::{FactEvidence, SourceRef, SourceSpan};

    use super::*;

    #[test]
    fn unproven_language_references_cannot_acquire_spelling_only_targets() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let hash = content_hash(b"Value");
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 5).unwrap(),
            hash,
            None,
        );
        let mut occurrence = OccurrenceRecord {
            id: FactId::from_bytes([4; 20]),
            repository,
            generation,
            file,
            source: source.clone(),
            role: OccurrenceRole::Reference,
            enclosing: None,
            target: OccurrenceTarget::Unresolved { text_hash: hash },
            syntactic_text_hash: hash,
            syntax_kind: String::new(),
            provenance: FactId::from_bytes([5; 20]),
            confidence: Confidence::new(0).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        for syntax in [
            "common_lisp.symbol.reference",
            "common_lisp.function.scoped_call",
            "sas.macro_call.reference",
            "sas.macro_variable.reference",
            "sas.dataset.reference",
            "apex.identifier.reference",
            "apex.member.reference",
            "apex.unproven_call.reference",
            "pascal.identifier.reference",
            "pascal.unit_reference.reference",
            "pascal.member_reference.reference",
        ] {
            occurrence.syntax_kind = syntax.to_owned();
            assert!(!resolvable_occurrence(&occurrence), "{syntax}");
            occurrence.role = OccurrenceRole::CallSite;
            assert!(!resolvable_occurrence(&occurrence), "{syntax}");
            occurrence.role = OccurrenceRole::Reference;
        }
        occurrence.syntax_kind = "python.identifier.reference".to_owned();
        assert!(resolvable_occurrence(&occurrence));
        occurrence.target = OccurrenceTarget::Resolved {
            symbol: SymbolId::from_bytes([6; 20]),
        };
        for syntax in [
            "common_lisp.function.scoped_call",
            "sas.macro_call.reference",
            "apex.unproven_call.reference",
            "pascal.member_reference.reference",
        ] {
            occurrence.syntax_kind = syntax.to_owned();
            assert!(
                resolvable_occurrence(&occurrence),
                "the guard does not erase independent target proof"
            );
        }
        occurrence.role = OccurrenceRole::Definition;
        assert!(!resolvable_occurrence(&occurrence));
    }
}
