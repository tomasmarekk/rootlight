//! Public resolver limits, outcomes, and source-free explanations.
//!
//! Explanations retain deterministic scoring evidence while candidate lists and
//! rejection details remain bounded by the same per-site resource ceiling.

use std::{collections::BTreeMap, error::Error, fmt};

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

/// Default operation-wide resolver work units.
pub const DEFAULT_RESOLUTION_WORK_LIMIT: usize = 2_000_000;

/// Absolute operation-wide resolver work units.
pub const MAX_RESOLUTION_WORK_LIMIT: usize = 2_000_000;

/// Checked resolver resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionLimits {
    candidate_limit: usize,
    work_limit: usize,
}

impl ResolutionLimits {
    /// Creates a checked candidate limit with the default work ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionLimitError`] when `candidate_limit` is zero or
    /// exceeds [`MAX_CANDIDATE_LIMIT`].
    pub const fn new(candidate_limit: usize) -> Result<Self, ResolutionLimitError> {
        if candidate_limit == 0 || candidate_limit > MAX_CANDIDATE_LIMIT {
            Err(ResolutionLimitError)
        } else {
            Ok(Self {
                candidate_limit,
                work_limit: DEFAULT_RESOLUTION_WORK_LIMIT,
            })
        }
    }

    /// Creates checked candidate and operation-wide work limits.
    ///
    /// # Errors
    ///
    /// Returns [`ResolutionLimitError`] when either limit is zero or exceeds
    /// its absolute ceiling.
    pub const fn with_work_limit(
        candidate_limit: usize,
        work_limit: usize,
    ) -> Result<Self, ResolutionLimitError> {
        if candidate_limit == 0
            || candidate_limit > MAX_CANDIDATE_LIMIT
            || work_limit == 0
            || work_limit > MAX_RESOLUTION_WORK_LIMIT
        {
            Err(ResolutionLimitError)
        } else {
            Ok(Self {
                candidate_limit,
                work_limit,
            })
        }
    }

    /// Returns the materialized candidate ceiling per occurrence.
    #[must_use]
    pub const fn candidate_limit(self) -> usize {
        self.candidate_limit
    }

    /// Returns the operation-wide occurrence and candidate-inspection ceiling.
    #[must_use]
    pub const fn work_limit(self) -> usize {
        self.work_limit
    }
}

/// Deterministic resolver work estimate for one normalized document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionWorkEstimate {
    /// Project evidence, occurrence and candidate work required by resolution.
    /// This is a lower bound when bounded project preparation stops early.
    pub required: usize,
    /// Configured operation-wide ceiling.
    pub limit: usize,
}

impl ResolutionWorkEstimate {
    /// Returns whether the complete resolution pass fits its configured limit.
    #[must_use]
    pub const fn fits(self) -> bool {
        self.required <= self.limit
    }
}

impl Default for ResolutionLimits {
    fn default() -> Self {
        Self {
            candidate_limit: DEFAULT_CANDIDATE_LIMIT,
            work_limit: DEFAULT_RESOLUTION_WORK_LIMIT,
        }
    }
}

/// Invalid resolver resource limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionLimitError;

impl fmt::Display for ResolutionLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("resolver limits exceed their checked ceilings")
    }
}

impl Error for ResolutionLimitError {}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DynamicCallCalibrationError {
    /// The language is not one of the reviewed dynamic adapters.
    UnsupportedLanguage,
    /// Precision is outside 0..=10,000 or below the 90 percent quality floor.
    PrecisionBelowFloor,
    /// An empty corpus cannot justify exact promotion.
    EmptyCorpus,
    /// The evaluation hid at least one ambiguous call behind an exact binding.
    HiddenExactBinding,
}

impl fmt::Display for DynamicCallCalibrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedLanguage => "dynamic call calibration language is unsupported",
            Self::PrecisionBelowFloor => {
                "dynamic call precision must be between 9000 and 10000 basis points"
            }
            Self::EmptyCorpus => "dynamic call calibration corpus must not be empty",
            Self::HiddenExactBinding => {
                "dynamic call calibration must not contain hidden exact bindings"
            }
        })
    }
}

impl Error for DynamicCallCalibrationError {}

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
    matches!(language, "javascript" | "python" | "tcl" | "typescript")
}

/// Language-neutral rule that produced one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionRule {
    /// A source-declared primary MATLAB function is a possible path target.
    MatlabPrimaryFunctionCandidate,
    /// A unique authored Elm module declaration matches the imported module name.
    ElmModuleImport,
    /// A unique source-declared Elm type in the same file matches a type use.
    ElmLocalType,
    /// A native receiver's project-owned declared type selects one accessible member.
    ClassicProjectMember,
    /// A class's explicit dispatch attribute selects an argument-validated default member.
    ClassicProjectDefaultMember,
    /// A source-exact named label selects a parameter of an argument-validated project call.
    ClassicProjectArgument,
    /// A unique accessible procedural callback in the same verified project.
    ClassicProjectPointer,
    /// A verified project module qualifies a member expression without lexical shadowing.
    ClassicProjectModule,
    /// A verified public project enum qualifies one of its declared constants.
    ClassicProjectEnum,
    /// An unshadowed explicit qualifier names the mandatory VBA standard library.
    VbaStandardNamespace,
    /// A read explicitly qualified by VBA names a specified standard constant.
    VbaStandardConstant,
    /// An unshadowed value use names a public constant of a standard global module.
    VbaStandardImplicitConstant,
    /// An explicit standard-library member names a specified function declaration.
    VbaStandardFunction,
    /// A complete argument list matches a source-qualified standard function.
    VbaStandardCall,
    /// An unshadowed explicit receiver names the standard Err default instance.
    VbaErrInstance,
    /// An explicit Err member matches a readable or writable property contract.
    VbaErrProperty,
    /// A statement callee names one of the standard Err object's methods.
    VbaErrMethod,
    /// A complete argument list matches a standard Err method declaration.
    VbaErrCall,
    /// An unshadowed native type use names the standard Collection class.
    VbaCollectionType,
    /// A source-exact New expression constructs the standard Collection class.
    VbaCollectionConstruction,
    /// A project-proven Collection receiver selects a standard member contract.
    VbaCollectionMember,
    /// A Collection member or default Item call has a complete valid argument list.
    VbaCollectionCall,
    /// Declarative classic BASIC project membership selects one native class type.
    ClassicProjectType,
    /// A version-matched native project reference exposes one source-declared class.
    ClassicReferencedType,
    /// Instantiation of a project-owned ordinary class and its source initializer.
    ClassicProjectConstruction,
    /// Native static import or call context selects one member or namespace declaration.
    GroovyStaticImport,
    /// Native fully qualified Groovy parent type with source-owned lexical context.
    GroovyQualifiedHeader,
    /// Source-owned Groovy hierarchy and lexical type selection across files.
    GroovyInheritedType,
    /// Native Groovy type import with source-owned package and type containers.
    GroovyTypeImport,
    /// Native Perl package CODE storage in the indexed dependency snapshot.
    PerlPackageStorage,
    /// Native public F# module export in one source-declared project order.
    FSharpQualifiedModule,
    /// Lexical scope and file containment resolution.
    LexicalScope,
    /// Import or module-name resolution over available normalized facts.
    Import,
}

/// Positive deterministic evidence contributing to one candidate score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionSignal {
    /// The imported Elm name selects one source-declared module in the generation.
    SourceDeclaredModule,
    /// Exact source membership isolates a native declaration from other projects.
    NativeProjectMembership,
    /// Native static-import evidence identifies a namespace or one authored static method.
    NativeStaticImport,
    /// A native qualified header selects an exact package/type declaration path.
    NativeQualifiedHeader,
    /// Native parent headers and lexical owner context identify the selected type.
    NativeInheritance,
    /// Native import target and canonical declaration ownership identify one type.
    NativeTypeImport,
    /// Source-bound native package/storage identities match without name scoring.
    NativePackageStorage,
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
    /// A syntactically scoped call has one compatible same-name target.
    UniqueScopedCallCandidate,
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
    /// A callable binding does not identify the value supplied at runtime.
    IndirectCallableBinding,
    /// The MATLAB search path and runtime variable precedence are unverified.
    UnverifiedMatlabSearchPath,
}

/// Why a same-spelling entity was excluded from the candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The entity language differs from the occurrence file language.
    LanguageMismatch,
    /// The entity kind cannot satisfy the occurrence role.
    TargetKindMismatch,
    /// A lexically owned parameter belongs to an unrelated function scope.
    OutsideLexicalScope,
    /// The language adapter has not established the required lexical binding.
    MissingLexicalEvidence,
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
    /// Links identify indexed package declarations, not evaluated loader paths or runtime values.
    IndexedPackageSnapshot,
    /// Literal ordered project members constrain links, without evaluated build closure.
    SourceDeclaredProjectOrder,
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
#[derive(Debug)]
pub enum ResolutionError {
    /// Classic source bytes or native container identity are stale or duplicated.
    ClassicSourceIdentity,
    /// The normalized input failed validation.
    InvalidDocument(IrDocumentValidationError),
    /// Cooperative cancellation stopped resolution.
    Cancelled(Cancelled),
    /// A bounded collection length could not be represented in the IR count domain.
    CountOverflow,
    /// Resolution exhausted its operation-wide project, occurrence and candidate work.
    WorkLimit {
        /// Maximum project evidence, occurrence and candidate work allowed.
        maximum: usize,
    },
    /// An internal score escaped the documented confidence range.
    InvalidScore,
    /// Resolver producer metadata violated normalized-IR label constraints.
    InvalidProducer(IrValidationError),
    /// A typed resolver fact identity could not be derived.
    FactIdentity(FactIdentityRecipeError),
    /// Applying a changed fact would require an unsupported dependent identity cascade.
    UnsupportedIdentityRemap,
}

impl fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClassicSourceIdentity => {
                f.write_str("classic project source identity differs from the indexed generation")
            }
            Self::InvalidDocument(_) => f.write_str("resolver input document is invalid"),
            Self::Cancelled(error) => fmt::Display::fmt(error, f),
            Self::CountOverflow => f.write_str("resolver result count is not representable"),
            Self::WorkLimit { maximum } => write!(f, "resolver work exceeds {maximum} units"),
            Self::InvalidScore => f.write_str("resolver score escaped the confidence range"),
            Self::InvalidProducer(_) => f.write_str("resolver producer identity is invalid"),
            Self::FactIdentity(_) => f.write_str("resolver fact identity could not be derived"),
            Self::UnsupportedIdentityRemap => {
                f.write_str("resolver application requires an unsupported dependent identity remap")
            }
        }
    }
}

impl Error for ResolutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidDocument(error) => Some(error),
            Self::InvalidProducer(error) => Some(error),
            Self::FactIdentity(error) => Some(error),
            // Cancellation is a transparent wrapper, not an extra cause layer.
            Self::Cancelled(error) => error.source(),
            Self::ClassicSourceIdentity
            | Self::CountOverflow
            | Self::WorkLimit { .. }
            | Self::InvalidScore
            | Self::UnsupportedIdentityRemap => None,
        }
    }
}

impl From<Cancelled> for ResolutionError {
    fn from(error: Cancelled) -> Self {
        Self::Cancelled(error)
    }
}
