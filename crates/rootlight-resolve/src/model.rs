//! Public resolver limits, outcomes, and source-free explanations.
//!
//! Explanations retain deterministic scoring evidence while candidate lists and
//! rejection details remain bounded by the same per-site resource ceiling.

use std::collections::BTreeMap;

use rootlight_cancel::Cancelled;
use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{
    Confidence, CoverageStatus, FactIdentityRecipeError, IrDocumentValidationError,
    IrValidationError, NormalizedIrDocument,
};

/// Stable language-neutral resolver identity.
pub const RESOLVER_PROVIDER_NAME: &str = "rootlight-resolve";

/// Stable language-neutral resolver rule-set version.
pub const RESOLVER_PROVIDER_VERSION: &str = "scope-v1";

/// Default number of candidate targets materialized for one occurrence.
pub const DEFAULT_CANDIDATE_LIMIT: usize = 256;

/// Absolute number of candidate targets materialized for one occurrence.
pub const MAX_CANDIDATE_LIMIT: usize = 4_096;

/// Checked resolver resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionLimits {
    candidate_limit: usize,
}

impl ResolutionLimits {
    /// Creates checked candidate limits.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionLimitError`] when `candidate_limit` is zero or
    /// exceeds [`MAX_CANDIDATE_LIMIT`].
    pub const fn new(candidate_limit: usize) -> Result<Self, ResolutionLimitError> {
        if candidate_limit == 0 || candidate_limit > MAX_CANDIDATE_LIMIT {
            Err(ResolutionLimitError)
        } else {
            Ok(Self { candidate_limit })
        }
    }

    /// Returns the materialized candidate ceiling per occurrence.
    #[must_use]
    pub const fn candidate_limit(self) -> usize {
        self.candidate_limit
    }
}

impl Default for ResolutionLimits {
    fn default() -> Self {
        Self {
            candidate_limit: DEFAULT_CANDIDATE_LIMIT,
        }
    }
}

/// Invalid resolver resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("resolver candidate limit must be between 1 and 4096")]
pub struct ResolutionLimitError;

/// Minimum measured dynamic-call precision required for exact promotion.
pub const MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS: u16 = 9_000;

/// Reviewed evidence permitting exact call promotion for one dynamic language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicCallCalibration {
    language: String,
    precision_basis_points: u16,
    sample_count: u64,
    hidden_exact_count: u64,
}

impl DynamicCallCalibration {
    /// Creates checked holdout evidence for one supported dynamic language.
    ///
    /// `precision_basis_points` uses 10,000 as 100 percent. Evidence is
    /// accepted only for registered dynamic-language adapters.
    ///
    /// # Errors
    ///
    /// Returns [`DynamicCallCalibrationError`] for an unsupported language,
    /// out-of-range or below-floor precision, an empty corpus, or any hidden
    /// exact binding in the evaluated ambiguity corpus.
    pub fn new(
        language: impl Into<String>,
        precision_basis_points: u16,
        sample_count: u64,
        hidden_exact_count: u64,
    ) -> Result<Self, DynamicCallCalibrationError> {
        let language = language.into();
        if !is_supported_dynamic_language(&language) {
            return Err(DynamicCallCalibrationError::UnsupportedLanguage);
        }
        if !(MIN_DYNAMIC_CALL_PRECISION_BASIS_POINTS..=10_000).contains(&precision_basis_points) {
            return Err(DynamicCallCalibrationError::PrecisionBelowFloor);
        }
        if sample_count == 0 {
            return Err(DynamicCallCalibrationError::EmptyCorpus);
        }
        if hidden_exact_count != 0 {
            return Err(DynamicCallCalibrationError::HiddenExactBinding);
        }
        Ok(Self {
            language,
            precision_basis_points,
            sample_count,
            hidden_exact_count,
        })
    }

    /// Returns the canonical dynamic-language identity.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns measured holdout precision in basis points.
    #[must_use]
    pub const fn precision_basis_points(&self) -> u16 {
        self.precision_basis_points
    }

    /// Returns the number of evaluated call sites.
    #[must_use]
    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }
}

/// Invalid evidence for dynamic-call exact promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DynamicCallCalibrationError {
    /// The language is not one of the reviewed dynamic adapters.
    #[error("dynamic call calibration language is unsupported")]
    UnsupportedLanguage,
    /// Precision is outside 0..=10,000 or below the 90 percent quality floor.
    #[error("dynamic call precision must be between 9000 and 10000 basis points")]
    PrecisionBelowFloor,
    /// An empty corpus cannot justify exact promotion.
    #[error("dynamic call calibration corpus must not be empty")]
    EmptyCorpus,
    /// The evaluation hid at least one ambiguous call behind an exact binding.
    #[error("dynamic call calibration must not contain hidden exact bindings")]
    HiddenExactBinding,
}

/// Conservative policy controlling evidence-dependent exact promotion.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolutionPolicy {
    dynamic_call_calibrations: BTreeMap<String, DynamicCallCalibration>,
}

impl ResolutionPolicy {
    /// Adds reviewed dynamic-call evidence, replacing evidence for that language.
    #[must_use]
    pub fn with_dynamic_call_calibration(mut self, calibration: DynamicCallCalibration) -> Self {
        self.dynamic_call_calibrations
            .insert(calibration.language.clone(), calibration);
        self
    }

    pub(crate) fn allows_exact_call(&self, language: &str) -> bool {
        !is_supported_dynamic_language(language)
            || self.dynamic_call_calibrations.contains_key(language)
    }

    pub(crate) fn append_configuration(&self, bytes: &mut Vec<u8>) {
        for calibration in self.dynamic_call_calibrations.values() {
            bytes.extend_from_slice(&(calibration.language.len() as u64).to_be_bytes());
            bytes.extend_from_slice(calibration.language.as_bytes());
            bytes.extend_from_slice(&calibration.precision_basis_points.to_be_bytes());
            bytes.extend_from_slice(&calibration.sample_count.to_be_bytes());
            bytes.extend_from_slice(&calibration.hidden_exact_count.to_be_bytes());
        }
    }
}

fn is_supported_dynamic_language(language: &str) -> bool {
    matches!(language, "javascript" | "python" | "typescript")
}

/// Language-neutral rule that produced one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionRule {
    /// Lexical scope and file containment resolution.
    LexicalScope,
    /// Import or module-name resolution over available normalized facts.
    Import,
}

/// Positive deterministic evidence contributing to one candidate score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionSignal {
    /// The entity canonical-name hash matches the occurrence text hash.
    CanonicalNameHash,
    /// The entity presentation name matches an explicit alias spelling.
    DisplayNameHash,
    /// The entity qualified name matches the complete occurrence spelling.
    QualifiedNameHash,
    /// The entity and containing file use the same language identity.
    SameLanguage,
    /// The entity is the enclosing symbol, such as a recursive call.
    EnclosingEntity,
    /// The entity is declared directly in the occurrence's lexical scope.
    SameScope,
    /// The entity is declared in a containing lexical scope.
    AncestorScope {
        /// Number of semantic-container hops to the declaring scope.
        depth: u16,
    },
    /// The entity is declared in the occurrence's file.
    SameFile,
    /// An import-compatible declaration is present in the validated generation.
    ImportableDeclaration,
    /// The entity kind is compatible with the occurrence role.
    CompatibleKind,
}

/// Deterministic evidence reducing or withholding one candidate's scope score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionPenalty {
    /// The declaration is outside the occurrence's file and known scope chain.
    CrossFile,
    /// The declaration has only repository-wide or unknown scope evidence.
    RepositoryScope,
    /// Dynamic-call evidence lacks a reviewed precision calibration.
    DynamicCallUncalibrated,
}

/// Why a same-spelling entity was excluded from the candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The entity language differs from the occurrence file language.
    LanguageMismatch,
    /// The entity kind cannot satisfy the occurrence role.
    TargetKindMismatch,
}

/// One bounded rejected-candidate explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedCandidate {
    /// Rejected semantic target.
    pub symbol: SymbolId,
    /// Deterministic exclusion reason.
    pub reason: RejectionReason,
}

/// Assumption that bounds interpretation of one resolution decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompletenessAssumption {
    /// The normalized document passed common ownership, reference, and quota validation.
    ValidatedNormalizedDocument,
    /// All considered facts belong to one repository generation.
    SingleGeneration,
    /// Matching uses the hash of the entity's canonical name.
    CanonicalNameHash,
    /// No build, package-manager, generator, test, or repository binary was executed.
    NoRepositoryExecution,
}

/// Deterministic score evidence for one compatible candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateExplanation {
    /// Candidate semantic target.
    pub symbol: SymbolId,
    /// Fixed-point score after positive evidence and scope penalties.
    pub score: Confidence,
    /// Positive signals in stable enum order.
    pub positive_signals: Vec<ResolutionSignal>,
    /// Penalties in stable enum order.
    pub penalties: Vec<ResolutionPenalty>,
}

/// Source-free evidence explaining one resolution outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionExplanation {
    /// Deterministic rule family.
    pub rule: ResolutionRule,
    /// Stable provider identity.
    pub provider_name: &'static str,
    /// Stable provider rule-set version.
    pub provider_version: &'static str,
    /// Materialized compatible candidates in decision rank order.
    pub candidates: Vec<CandidateExplanation>,
    /// Materialized rejections in symbol order.
    pub rejected_candidates: Vec<RejectedCandidate>,
    /// Rejection count before truncation.
    pub rejected_total: u64,
    /// Assumptions constraining completeness and interpretation.
    pub completeness_assumptions: Vec<CompletenessAssumption>,
}

/// One semantic resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionOutcome {
    /// A uniquely supported target.
    Resolved {
        /// Resolved semantic target.
        symbol: SymbolId,
        /// Calibrated support for the exact binding.
        confidence: Confidence,
    },
    /// A bounded set of possible targets.
    Candidates {
        /// Materialized targets in deterministic rank order.
        symbols: Vec<SymbolId>,
        /// Number of compatible candidates before truncation.
        total_count: u64,
        /// Completeness of the materialized target set.
        completeness: CoverageStatus,
        /// Support for the highest-ranked candidate.
        confidence: Confidence,
    },
    /// A site retained without an invented target.
    Unresolved {
        /// Source-free reason the resolver could not select candidates.
        reason: UnresolvedReason,
        /// Calibrated unresolved confidence, normally zero.
        confidence: Confidence,
    },
}

/// Source-free reason for retaining an unresolved occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnresolvedReason {
    /// No matching declaration was present in the validated input.
    NoCandidate,
    /// An import target was absent from the available dependency facts.
    MissingDependency,
    /// Matching declarations had kinds incompatible with the occurrence role.
    UnsupportedTargetKind,
}

/// One occurrence decision and its evidence explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionDecision {
    /// Occurrence being resolved.
    pub occurrence: FactId,
    /// Exact, candidate, or unresolved result.
    pub outcome: ResolutionOutcome,
    /// Deterministic source-free scoring evidence.
    pub explanation: ResolutionExplanation,
}

/// Deterministic decisions for one immutable repository generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionBatch {
    /// Repository owning every decision.
    pub repository: RepositoryId,
    /// Immutable generation used for every decision.
    pub generation: GenerationId,
    /// Decisions sorted by occurrence identity.
    pub decisions: Vec<ResolutionDecision>,
}

/// Immutable build identity supplied by the host applying resolver facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverFactContext {
    binary_digest: ContentHash,
}

impl ResolverFactContext {
    /// Creates resolver-fact context from the producing binary digest.
    #[must_use]
    pub const fn new(binary_digest: ContentHash) -> Self {
        Self { binary_digest }
    }

    /// Returns the producing binary digest recorded in resolver provenance.
    #[must_use]
    pub const fn binary_digest(self) -> ContentHash {
        self.binary_digest
    }
}

/// Canonical resolved IR paired with the decisions that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedResolution {
    /// Validated and canonicalized normalized facts.
    pub document: NormalizedIrDocument,
    /// Source-free decision and explanation bundle.
    pub batch: ResolutionBatch,
}

/// Semantic resolution failure.
#[derive(Debug, thiserror::Error)]
pub enum ResolutionError {
    /// The normalized input failed validation.
    #[error("resolver input document is invalid")]
    InvalidDocument(#[source] IrDocumentValidationError),
    /// Cooperative cancellation stopped resolution.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// A bounded collection length could not be represented in the IR count domain.
    #[error("resolver result count is not representable")]
    CountOverflow,
    /// An internal score escaped the documented confidence range.
    #[error("resolver score escaped the confidence range")]
    InvalidScore,
    /// Resolver producer metadata violated normalized-IR label constraints.
    #[error("resolver producer identity is invalid")]
    InvalidProducer(#[source] IrValidationError),
    /// A typed resolver fact identity could not be derived.
    #[error("resolver fact identity could not be derived")]
    FactIdentity(#[source] FactIdentityRecipeError),
    /// Applying a changed fact would require an unsupported dependent identity cascade.
    #[error("resolver application requires an unsupported dependent identity remap")]
    UnsupportedIdentityRemap,
}
