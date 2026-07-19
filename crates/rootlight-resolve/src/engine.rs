//! Bounded name index, scope scoring, and ambiguity-preserving decisions.
//!
//! Exact bindings require a unique candidate at or above the documented strong
//! threshold; tied or weaker evidence remains an explicit candidate set.

use std::collections::BTreeMap;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    AnalysisTier, Confidence, ContainerRef, CoverageStatus, EntityKind, EntityRecord,
    ExtensionSupport, FileRecord, IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProvenanceRecord, validate_ir_document,
};

use crate::model::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, RejectedCandidate, RejectionReason, ResolutionBatch,
    ResolutionDecision, ResolutionError, ResolutionExplanation, ResolutionLimits,
    ResolutionOutcome, ResolutionPenalty, ResolutionRule, ResolutionSignal, UnresolvedReason,
};

const EXACT_BINDING_THRESHOLD: u16 = 900;
const NAME_AND_LANGUAGE_SCORE: u16 = 600;
const SAME_SCOPE_BONUS: u16 = 350;
const SAME_FILE_BONUS: u16 = 300;
const IMPORTABLE_BONUS: u16 = 300;
const ANCESTOR_SCOPE_BASE_BONUS: u16 = 340;
const ANCESTOR_SCOPE_STEP_PENALTY: u16 = 20;
const CROSS_FILE_PENALTY: u16 = 100;
const REPOSITORY_SCOPE_PENALTY: u16 = 50;
const MAX_SCOPE_DEPTH: usize = 64;

/// Language-neutral semantic candidate resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionEngine {
    pub(crate) limits: ResolutionLimits,
}

impl ResolutionEngine {
    /// Creates a resolver with checked limits.
    #[must_use]
    pub const fn new(limits: ResolutionLimits) -> Self {
        Self { limits }
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

        let index = CandidateIndex::build(document, cancellation)?;
        let mut decisions = Vec::with_capacity(document.occurrences.len());
        for occurrence in &document.occurrences {
            cancellation.check()?;
            if matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
                || !resolvable_role(occurrence.role)
            {
                continue;
            }
            decisions.push(self.resolve_occurrence(occurrence, &index, cancellation)?);
        }
        decisions.sort_unstable_by_key(|decision| decision.occurrence);

        Ok(ResolutionBatch {
            repository: document.repository,
            generation: document.generation,
            decisions,
        })
    }

    fn resolve_occurrence(
        &self,
        occurrence: &OccurrenceRecord,
        index: &CandidateIndex<'_>,
        cancellation: &Cancellation,
    ) -> Result<ResolutionDecision, ResolutionError> {
        let language = index
            .files
            .get(&occurrence.file)
            .map(|file| file.language.as_str())
            .ok_or(ResolutionError::InvalidScore)?;
        let rule = if occurrence.role == OccurrenceRole::ImportUse {
            ResolutionRule::Import
        } else {
            ResolutionRule::LexicalScope
        };
        let same_spelling = index
            .by_name_hash
            .get(&occurrence.syntactic_text_hash)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut candidates = Vec::new();
        let mut rejected = Vec::new();
        let mut rejected_total = 0_u64;
        let mut kind_rejection_total = 0_u64;

        for indexed in same_spelling {
            cancellation.check()?;
            let entity = indexed.entity;
            let rejection = if entity.language != language {
                Some(RejectionReason::LanguageMismatch)
            } else if !kind_supports_role(entity.kind, occurrence.role) {
                Some(RejectionReason::TargetKindMismatch)
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
            candidates.push(score_candidate(
                occurrence,
                entity,
                indexed.name_match,
                index,
            )?);
        }

        candidates.sort_unstable_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.symbol.cmp(&right.symbol))
        });
        rejected.sort_unstable_by_key(|candidate| candidate.symbol);

        let total_count =
            u64::try_from(candidates.len()).map_err(|_| ResolutionError::CountOverflow)?;
        let top_is_unique = candidates
            .get(1)
            .is_none_or(|second| second.score < candidates[0].score);
        let exact = candidates
            .first()
            .filter(|candidate| top_is_unique && candidate.score.get() >= EXACT_BINDING_THRESHOLD)
            .map(|candidate| (candidate.symbol, candidate.score));
        let materialized_count = candidates.len().min(self.limits.candidate_limit());
        candidates.truncate(materialized_count);

        let outcome = if let Some((symbol, confidence)) = exact {
            ResolutionOutcome::Resolved { symbol, confidence }
        } else if let Some(top) = candidates.first() {
            ResolutionOutcome::Candidates {
                symbols: candidates
                    .iter()
                    .map(|candidate| candidate.symbol)
                    .collect(),
                total_count,
                completeness: if total_count
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
}

impl Default for ResolutionEngine {
    fn default() -> Self {
        Self::new(ResolutionLimits::default())
    }
}

struct CandidateIndex<'a> {
    by_name_hash: BTreeMap<ContentHash, Vec<IndexedCandidate<'a>>>,
    entities: BTreeMap<SymbolId, &'a EntityRecord>,
    files: BTreeMap<FileId, &'a FileRecord>,
    provenance: BTreeMap<FactId, &'a ProvenanceRecord>,
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
    fn build(
        document: &'a NormalizedIrDocument,
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        let mut by_name_hash = BTreeMap::<ContentHash, Vec<IndexedCandidate<'_>>>::new();
        let mut entities = BTreeMap::new();
        let mut files = BTreeMap::new();
        let mut provenance = BTreeMap::new();
        for file in &document.files {
            cancellation.check()?;
            files.insert(file.id, file);
        }
        for entity in &document.entities {
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
        }
        for record in &document.provenance {
            cancellation.check()?;
            provenance.insert(record.id, record);
        }
        for entries in by_name_hash.values_mut() {
            entries.sort_unstable_by_key(|entry| (entry.entity.id, entry.name_match));
            entries.dedup_by_key(|entry| entry.entity.id);
        }
        Ok(Self {
            by_name_hash,
            entities,
            files,
            provenance,
        })
    }
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

fn resolvable_role(role: OccurrenceRole) -> bool {
    !matches!(
        role,
        OccurrenceRole::Definition
            | OccurrenceRole::Declaration
            | OccurrenceRole::Documentation
            | OccurrenceRole::StringEvidence
    )
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
