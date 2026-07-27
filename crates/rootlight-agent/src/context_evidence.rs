//! Typed, bounded evidence planning for generation-pinned context packs.
//!
//! This module owns source-free provider selection and the validation boundary
//! for repository-derived evidence. Concrete adapters remain outside the agent
//! domain and cannot influence provider order or candidate identity.

use std::time::Instant;

use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_ir::SourceRef;
use rootlight_mcp_contract::{
    TrustClassification,
    completeness::{CompletenessState, LimitingResource, LimitingResourceKind, ResultCompleteness},
    context::{ContextSection, SourcePolicy},
};

use crate::{
    context_pack::EvidenceRole,
    context_pack_request::{CanonicalContextPackRequest, ContextPackObjective},
    policy::{
        BudgetCharge, BudgetLedger, BudgetResource, CancellationSignal, ExecutionPolicyError,
    },
    port::AgentPortFuture,
};

/// Maximum provider invocations admitted for one canonical request.
pub const MAX_CONTEXT_PROVIDER_CALLS: usize = 64;

/// Maximum candidates retained from one provider invocation.
pub const MAX_CANDIDATES_PER_PROVIDER: u16 = 32;

/// Maximum source references or dependency edges on one candidate.
pub const MAX_CANDIDATE_LINKS: usize = 16;

/// Maximum distinct provider omission groups retained by one corpus.
pub const MAX_PROVIDER_OMISSIONS: usize = 32;

/// One canonical seed category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceSeedKind {
    /// Stable symbol identities.
    Symbol,
    /// Repository-relative paths.
    Path,
    /// Route or service names.
    Route,
    /// Stable test symbol identities.
    Test,
    /// Opaque located-result handle.
    Located,
    /// Opaque change descriptor.
    Change,
    /// Opaque plan descriptor.
    Plan,
}

impl EvidenceSeedKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Symbol => 0,
            Self::Path => 1,
            Self::Route => 2,
            Self::Test => 3,
            Self::Located => 4,
            Self::Change => 5,
            Self::Plan => 6,
        }
    }
}

/// One category-preserving canonical evidence anchor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceAnchor {
    /// Stable symbol identity.
    Symbol(SymbolId),
    /// Canonical repository-relative path.
    Path(String),
    /// Canonical route or service name.
    Route(String),
    /// Stable test symbol identity.
    Test(SymbolId),
    /// Exact bounded located-result handle.
    Located(String),
    /// Exact bounded change descriptor.
    Change(String),
    /// Exact bounded plan descriptor.
    Plan(String),
}

impl EvidenceAnchor {
    /// Returns the preserved seed category.
    #[must_use]
    pub const fn kind(&self) -> EvidenceSeedKind {
        match self {
            Self::Symbol(_) => EvidenceSeedKind::Symbol,
            Self::Path(_) => EvidenceSeedKind::Path,
            Self::Route(_) => EvidenceSeedKind::Route,
            Self::Test(_) => EvidenceSeedKind::Test,
            Self::Located(_) => EvidenceSeedKind::Located,
            Self::Change(_) => EvidenceSeedKind::Change,
            Self::Plan(_) => EvidenceSeedKind::Plan,
        }
    }

    fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hasher.update(&[self.kind().tag()]);
        match self {
            Self::Symbol(value) | Self::Test(value) => {
                hash_bytes(hasher, value.as_bytes());
            }
            Self::Path(value)
            | Self::Route(value)
            | Self::Located(value)
            | Self::Change(value)
            | Self::Plan(value) => hash_bytes(hasher, value.as_bytes()),
        }
    }
}

/// Bounded repository evidence provider selected by the agent planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceProvider {
    /// Resolve non-symbol anchors to stable graph identities.
    Locate,
    /// Fetch declarations and signatures.
    Definition,
    /// Fetch implementation bodies or exact implementation anchors.
    Implementation,
    /// Traverse callers and callees.
    Relationships,
    /// Select relevant tests and fixtures.
    Tests,
    /// Retrieve component and dependency-boundary evidence.
    Architecture,
    /// Analyze change impact and observed risk.
    ChangeImpact,
    /// Retrieve generation-aware history evidence.
    History,
    /// Retrieve bounded planning artifacts and their dependencies.
    Planning,
    /// Read exact generation-pinned source excerpts.
    Source,
}

impl EvidenceProvider {
    /// Stable provider name used only in source-free diagnostics.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Locate => "code.locate",
            Self::Definition => "symbol.explain",
            Self::Implementation => "source.read",
            Self::Relationships => "symbol.relationships",
            Self::Tests => "tests.select",
            Self::Architecture => "architecture.overview",
            Self::ChangeImpact => "change.impact",
            Self::History => "history.compare",
            Self::Planning => "plan.change",
            Self::Source => "source.read",
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Locate => 0,
            Self::Definition => 1,
            Self::Implementation => 2,
            Self::Relationships => 3,
            Self::Tests => 4,
            Self::Architecture => 5,
            Self::ChangeImpact => 6,
            Self::History => 7,
            Self::Planning => 8,
            Self::Source => 9,
        }
    }
}

/// Observed evidence domain used to assign provenance independently of display
/// text and public tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceProvenance {
    /// Stable syntax or semantic graph records.
    Graph,
    /// Generation-pinned source index.
    Source,
    /// Test discovery or coverage index.
    TestIndex,
    /// Architecture aggregation derived from graph records.
    ArchitectureAnalysis,
    /// Bounded impact or risk analysis.
    ChangeAnalysis,
    /// Generation or revision history index.
    HistoryAnalysis,
    /// Validated planning artifact.
    PlanArtifact,
}

impl EvidenceProvenance {
    const fn tag(self) -> u8 {
        match self {
            Self::Graph => 0,
            Self::Source => 1,
            Self::TestIndex => 2,
            Self::ArchitectureAnalysis => 3,
            Self::ChangeAnalysis => 4,
            Self::HistoryAnalysis => 5,
            Self::PlanArtifact => 6,
        }
    }
}

/// Stable identity for one provider invocation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderInvocationId(String);

impl ProviderInvocationId {
    /// Borrows the source-free invocation identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One deterministic and bounded provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProviderInvocation {
    id: ProviderInvocationId,
    repository: RepositoryId,
    generation: GenerationId,
    objective: ContextPackObjective,
    task: String,
    provider: EvidenceProvider,
    role: EvidenceRole,
    anchors: Vec<EvidenceAnchor>,
    max_candidates: u16,
    reservation: BudgetCharge,
}

impl EvidenceProviderInvocation {
    /// Returns the stable invocation identity.
    #[must_use]
    pub const fn id(&self) -> &ProviderInvocationId {
        &self.id
    }

    /// Returns the exact repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the exact generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the canonical task objective.
    #[must_use]
    pub const fn objective(&self) -> ContextPackObjective {
        self.objective
    }

    /// Returns the normalized source-free task text.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Returns the selected provider.
    #[must_use]
    pub const fn provider(&self) -> EvidenceProvider {
        self.provider
    }

    /// Returns the evidence role requested from the provider.
    #[must_use]
    pub const fn role(&self) -> EvidenceRole {
        self.role
    }

    /// Returns canonical anchors in deterministic order.
    #[must_use]
    pub fn anchors(&self) -> &[EvidenceAnchor] {
        &self.anchors
    }

    /// Returns the hard candidate ceiling for this call.
    #[must_use]
    pub const fn max_candidates(&self) -> u16 {
        self.max_candidates
    }

    /// Returns the conservative parent-ledger reservation.
    #[must_use]
    pub const fn reservation(&self) -> BudgetCharge {
        self.reservation
    }
}

/// Complete deterministic provider plan for one canonical request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProviderPlan {
    request_digest: [u8; 32],
    invocations: Vec<EvidenceProviderInvocation>,
}

impl EvidenceProviderPlan {
    /// Returns the canonical request digest bound to this plan.
    #[must_use]
    pub const fn request_digest(&self) -> [u8; 32] {
        self.request_digest
    }

    /// Returns provider calls in their stable dispatch order.
    #[must_use]
    pub fn invocations(&self) -> &[EvidenceProviderInvocation] {
        &self.invocations
    }
}

/// Failure to construct a bounded provider plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceProviderPlanError {
    /// The provider fan-out ceiling would be exceeded.
    #[error("context evidence provider fan-out exceeds its hard ceiling")]
    FanoutExceeded,
    /// A canonical request unexpectedly contained no evidence anchor.
    #[error("context evidence provider plan has no anchor")]
    NoAnchors,
}

/// Stateless registry that maps canonical requests to bounded provider calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextEvidenceProviderRegistry;

impl ContextEvidenceProviderRegistry {
    /// Builds the deterministic provider plan for all present seed categories.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceProviderPlanError`] when no anchor exists or the hard
    /// provider fan-out ceiling cannot represent the request.
    pub fn plan(
        &self,
        request: &CanonicalContextPackRequest,
    ) -> Result<EvidenceProviderPlan, EvidenceProviderPlanError> {
        let anchors = grouped_anchors(request);
        if anchors.is_empty() {
            return Err(EvidenceProviderPlanError::NoAnchors);
        }

        let roles = request.requested_roles();
        let mut invocations = Vec::new();
        for (kind, grouped) in anchors {
            for role in &roles {
                push_invocation(
                    request,
                    kind,
                    provider_for(kind, *role),
                    *role,
                    &grouped,
                    &mut invocations,
                )?;
            }

            if request.sections().contains(&ContextSection::History) {
                push_invocation(
                    request,
                    kind,
                    EvidenceProvider::History,
                    EvidenceRole::Change,
                    &grouped,
                    &mut invocations,
                )?;
            }
            if matches!(kind, EvidenceSeedKind::Change | EvidenceSeedKind::Plan)
                || request.objective() == ContextPackObjective::Migration
            {
                push_invocation(
                    request,
                    kind,
                    EvidenceProvider::Planning,
                    EvidenceRole::Change,
                    &grouped,
                    &mut invocations,
                )?;
            }
        }

        Ok(EvidenceProviderPlan {
            request_digest: request.digest_bytes(),
            invocations,
        })
    }
}

/// Stable identity for one validated evidence candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceCandidateId(String);

impl EvidenceCandidateId {
    /// Borrows the candidate identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider-independent key used to deduplicate equivalent observed evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceDedupKey(String);

impl EvidenceDedupKey {
    /// Borrows the deduplication key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Transport observation returned by one bounded provider adapter.
///
/// The adapter reports only facts present in the provider response. Candidate
/// role, provenance, trust, confidence, cost, stable ID, and deduplication key
/// are assigned by the agent from the authoritative invocation and these facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProviderObservation {
    /// Provider-response record category, independent of context-pack roles.
    pub kind: EvidenceProviderObservationKind,
    /// Stable symbol identity when the evidence describes a symbol.
    pub symbol_id: Option<SymbolId>,
    /// Canonical source-free identity supplied by the provider.
    pub identity: String,
    /// Fixed-point confidence reported by the provider, when one exists.
    pub observed_score: Option<u16>,
    /// Provider-native relevance when it differs from evidence confidence.
    pub observed_relevance: Option<u16>,
    /// Material size observed by the adapter.
    pub estimated_tokens: u64,
    /// Exact source bytes observed by the adapter.
    pub source_bytes: u64,
    /// Exact generation-pinned source references.
    pub source_refs: Vec<SourceRef>,
}

/// Transport-level record categories exposed by provider responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceProviderObservationKind {
    /// Ordinary result row from the selected provider.
    Primary,
    /// Aggregate risk facts from one change-impact response.
    ChangeRiskSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceCandidateDraft {
    pub(crate) repository: RepositoryId,
    pub(crate) generation: GenerationId,
    pub(crate) invocation: ProviderInvocationId,
    pub(crate) provider: EvidenceProvider,
    pub(crate) role: EvidenceRole,
    pub(crate) provenance: EvidenceProvenance,
    pub(crate) symbol_id: Option<SymbolId>,
    pub(crate) identity: String,
    pub(crate) relevance: u16,
    pub(crate) confidence: u16,
    pub(crate) cost: BudgetCharge,
    pub(crate) source_refs: Vec<SourceRef>,
    pub(crate) dependencies: Vec<EvidenceCandidateId>,
}

/// One fully validated typed evidence candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedEvidenceCandidate {
    id: EvidenceCandidateId,
    dedup_key: EvidenceDedupKey,
    repository: RepositoryId,
    generation: GenerationId,
    invocation: ProviderInvocationId,
    provider: EvidenceProvider,
    role: EvidenceRole,
    provenance: EvidenceProvenance,
    trust: TrustClassification,
    symbol_id: Option<SymbolId>,
    identity: String,
    relevance: u16,
    confidence: u16,
    cost: BudgetCharge,
    source_refs: Vec<SourceRef>,
    dependencies: Vec<EvidenceCandidateId>,
}

impl TypedEvidenceCandidate {
    /// Validates and canonicalizes one provider observation.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceCandidateError`] when identity, confidence, cost,
    /// source references, or dependency bounds are invalid.
    pub(crate) fn from_draft(
        expected_repository: RepositoryId,
        expected_generation: GenerationId,
        mut draft: EvidenceCandidateDraft,
    ) -> Result<Self, EvidenceCandidateError> {
        if draft.repository != expected_repository {
            return Err(EvidenceCandidateError::RepositoryMismatch);
        }
        if draft.generation != expected_generation {
            return Err(EvidenceCandidateError::GenerationMismatch);
        }
        if draft.identity.is_empty() || draft.identity.len() > 4096 {
            return Err(EvidenceCandidateError::InvalidIdentity);
        }
        if draft.relevance > 1_000 || draft.confidence > 1_000 {
            return Err(EvidenceCandidateError::InvalidScore);
        }
        if draft.cost.results == 0
            || draft.cost.tokens == 0
            || draft.cost.tokens > 32_000
            || draft.source_refs.len() > MAX_CANDIDATE_LINKS
            || draft.dependencies.len() > MAX_CANDIDATE_LINKS
        {
            return Err(EvidenceCandidateError::InvalidCostOrBounds);
        }
        if draft.source_refs.iter().any(|source| {
            source.repository() != expected_repository || source.generation() != expected_generation
        }) {
            return Err(EvidenceCandidateError::SourceIdentityMismatch);
        }

        draft.source_refs.sort();
        draft.source_refs.dedup();
        draft.dependencies.sort();
        draft.dependencies.dedup();
        let dedup_key = candidate_dedup_key(&draft);
        let id = candidate_id(&draft, &dedup_key);

        Ok(Self {
            id,
            dedup_key,
            repository: draft.repository,
            generation: draft.generation,
            invocation: draft.invocation,
            provider: draft.provider,
            role: draft.role,
            provenance: draft.provenance,
            trust: TrustClassification::UntrustedRepositoryData,
            symbol_id: draft.symbol_id,
            identity: draft.identity,
            relevance: draft.relevance,
            confidence: draft.confidence,
            cost: draft.cost,
            source_refs: draft.source_refs,
            dependencies: draft.dependencies,
        })
    }

    /// Returns the stable candidate identity.
    #[must_use]
    pub const fn id(&self) -> &EvidenceCandidateId {
        &self.id
    }

    /// Returns the provider-independent deduplication key.
    #[must_use]
    pub const fn dedup_key(&self) -> &EvidenceDedupKey {
        &self.dedup_key
    }

    /// Returns the authoritative repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the authoritative generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the invocation that produced this candidate.
    #[must_use]
    pub const fn invocation(&self) -> &ProviderInvocationId {
        &self.invocation
    }

    /// Returns the source provider domain.
    #[must_use]
    pub const fn provider(&self) -> EvidenceProvider {
        self.provider
    }

    /// Returns the evidence role.
    #[must_use]
    pub const fn role(&self) -> EvidenceRole {
        self.role
    }

    /// Returns observed provenance.
    #[must_use]
    pub const fn provenance(&self) -> EvidenceProvenance {
        self.provenance
    }

    /// Returns the repository-data trust classification.
    #[must_use]
    pub const fn trust(&self) -> TrustClassification {
        self.trust
    }

    /// Returns the described symbol when one exists.
    #[must_use]
    pub const fn symbol_id(&self) -> Option<SymbolId> {
        self.symbol_id
    }

    /// Returns the canonical source-free provider identity.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Returns relevance from observed evidence.
    #[must_use]
    pub const fn relevance(&self) -> u16 {
        self.relevance
    }

    /// Returns confidence from observed evidence.
    #[must_use]
    pub const fn confidence(&self) -> u16 {
        self.confidence
    }

    /// Returns the conservative materialization cost.
    #[must_use]
    pub const fn cost(&self) -> BudgetCharge {
        self.cost
    }

    /// Returns exact generation-pinned source references.
    #[must_use]
    pub fn source_refs(&self) -> &[SourceRef] {
        &self.source_refs
    }

    /// Returns stable dependency identities.
    #[must_use]
    pub fn dependencies(&self) -> &[EvidenceCandidateId] {
        &self.dependencies
    }
}

/// Candidate validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceCandidateError {
    /// Provider repository differs from the pinned request identity.
    #[error("context evidence candidate repository does not match the request")]
    RepositoryMismatch,
    /// Provider generation differs from the pinned request identity.
    #[error("context evidence candidate generation does not match the request")]
    GenerationMismatch,
    /// Provider identity is empty or unbounded.
    #[error("context evidence candidate identity is invalid")]
    InvalidIdentity,
    /// Relevance or confidence is outside the fixed-point range.
    #[error("context evidence candidate score is invalid")]
    InvalidScore,
    /// Cost, source reference, or dependency bounds are invalid.
    #[error("context evidence candidate exceeds a hard bound")]
    InvalidCostOrBounds,
    /// A source reference is not pinned to the candidate identity.
    #[error("context evidence source reference has mismatched identity")]
    SourceIdentityMismatch,
}

/// Checked result of one bounded provider invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProviderOutput {
    /// Authoritative repository identity observed by the adapter.
    pub repository: RepositoryId,
    /// Authoritative generation identity observed by the adapter.
    pub generation: GenerationId,
    /// Invocation identity echoed by the adapter.
    pub invocation: ProviderInvocationId,
    /// Transport observations returned within the invocation ceiling.
    pub observations: Vec<EvidenceProviderObservation>,
    /// Authoritative completeness and limiting-resource state.
    pub completeness: ResultCompleteness,
    /// Measured provider work charged to the parent ledger.
    pub usage: BudgetCharge,
}

/// One selected candidate whose generation-pinned source may be materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourceTarget {
    /// Stable candidate identity selected by the optimizer.
    pub candidate_id: EvidenceCandidateId,
    /// Exact source range approved by evidence collection.
    pub source_ref: SourceRef,
}

/// Bounded second-stage source request for already selected evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourceRequest {
    /// Authoritative repository identity.
    pub repository: RepositoryId,
    /// Authoritative immutable generation identity.
    pub generation: GenerationId,
    /// Canonical source inclusion mode.
    pub source_policy: SourcePolicy,
    /// Whether response shaping permits repository source bodies.
    pub include_snippets: bool,
    /// Per-snippet UTF-8 byte ceiling.
    pub max_bytes_per_snippet: u32,
    /// Deterministically ordered selected source targets.
    pub targets: Vec<ContextSourceTarget>,
}

/// Repository-derived source body returned strictly as untrusted data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourceSnippet {
    /// Exact UTF-8 source bytes.
    pub content: String,
    /// Bounded language identifier supplied by the source adapter.
    pub language: String,
    /// Whether the approved source range was reduced by a hard bound.
    pub truncated: bool,
}

/// Materialized representation for one selected context candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourceMaterial {
    /// Selected candidate identity echoed by the adapter.
    pub candidate_id: EvidenceCandidateId,
    /// Exact generation-pinned source range that produced this material.
    pub source_ref: SourceRef,
    /// Bounded declaration or type signature when requested.
    pub signature: Option<String>,
    /// Bounded source body when requested.
    pub snippet: Option<ContextSourceSnippet>,
}

/// Checked result of second-stage source materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSourceOutput {
    /// Authoritative repository identity observed by the adapter.
    pub repository: RepositoryId,
    /// Authoritative generation identity observed by the adapter.
    pub generation: GenerationId,
    /// Material returned for a subset of selected targets.
    pub materials: Vec<ContextSourceMaterial>,
    /// Truthful completeness of source materialization.
    pub completeness: ResultCompleteness,
    /// Measured work charged to the shared parent ledger.
    pub usage: BudgetCharge,
}

/// Request-scoped controls supplied to one context-evidence adapter.
#[derive(Debug, Clone)]
pub struct ContextEvidenceCallContext<C> {
    cancellation: C,
    deadline: Instant,
    reservation: BudgetCharge,
}

impl<C> ContextEvidenceCallContext<C>
where
    C: CancellationSignal,
{
    /// Creates a provider call context from an admitted parent reservation.
    #[must_use]
    pub const fn new(cancellation: C, deadline: Instant, reservation: BudgetCharge) -> Self {
        Self {
            cancellation,
            deadline,
            reservation,
        }
    }

    /// Returns the cooperative cancellation signal.
    #[must_use]
    pub const fn cancellation(&self) -> &C {
        &self.cancellation
    }

    /// Returns the mandatory monotonic deadline.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns the complete multi-resource reservation for this call.
    #[must_use]
    pub const fn reservation(&self) -> BudgetCharge {
        self.reservation
    }
}

/// Terminal adapter failure with measured work retained for parent accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextEvidencePortError {
    /// Source-free failure class.
    pub kind: ContextEvidencePortErrorKind,
    /// Work completed before the adapter returned the failure.
    pub usage: BudgetCharge,
}

/// Source-free provider adapter failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextEvidencePortErrorKind {
    /// The selected evidence domain is unavailable for this repository.
    Unsupported,
    /// The provider failed without a trustworthy typed response.
    Unavailable,
    /// Cooperative cancellation won.
    Cancelled,
    /// The request deadline elapsed.
    DeadlineExceeded,
    /// The adapter observed corrupt or contract-invalid provider output.
    InvalidResponse,
}

/// Client-free port implemented by concrete repository evidence adapters.
pub trait ContextEvidencePort<C>: Send + Sync + 'static
where
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    /// Executes one already planned and parent-budget-reserved provider call.
    fn retrieve(
        &self,
        invocation: EvidenceProviderInvocation,
        context: ContextEvidenceCallContext<C>,
    ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>>;

    /// Materializes generation-pinned source only for optimizer-selected items.
    ///
    /// Implementations that have not wired the second-stage source boundary
    /// remain truthful by reporting the capability as unsupported.
    fn materialize_source(
        &self,
        _request: ContextSourceRequest,
        _context: ContextEvidenceCallContext<C>,
    ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
        Box::pin(async {
            Err(ContextEvidencePortError {
                kind: ContextEvidencePortErrorKind::Unsupported,
                usage: BudgetCharge::default(),
            })
        })
    }
}

/// Source-free reason one provider did not contribute a complete candidate set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceProviderOmissionReason {
    /// Provider completed but no supporting evidence exists.
    NoEvidence,
    /// A known provider resource bound truncated the result.
    Truncated,
    /// Provider domain is unavailable for the selected repository.
    Unsupported,
    /// Provider failed without a usable typed result.
    Unavailable,
    /// Shared parent capacity could not reserve this call.
    Budget,
    /// Candidate fell below the canonical confidence threshold.
    LowConfidence,
}

/// Bounded provider-level omission retained for pack completeness accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceProviderOmission {
    /// Provider that could not contribute complete evidence.
    pub provider: EvidenceProvider,
    /// Evidence role affected by the omission.
    pub role: EvidenceRole,
    /// Stable source-free omission reason.
    pub reason: EvidenceProviderOmissionReason,
    /// Number of omitted candidates or provider calls.
    pub count: u32,
    /// Exact limiting resources reported or derived at the agent boundary.
    pub limiting_resources: Vec<LimitingResource>,
}

/// Validated, deterministically ordered provider corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEvidenceCorpus {
    /// Deduplicated candidates in stable optimizer input order.
    pub candidates: Vec<TypedEvidenceCandidate>,
    /// Bounded provider failures, truncation, and confidence omissions.
    pub omissions: Vec<EvidenceProviderOmission>,
    /// Authoritative aggregate provider work charged to the parent ledger.
    pub usage: BudgetCharge,
    budget: BudgetLedger,
}

impl ContextEvidenceCorpus {
    /// Returns the shared ledger after every provider call has reconciled.
    #[must_use]
    pub(crate) const fn budget(&self) -> &BudgetLedger {
        &self.budget
    }
}

/// Failure to collect a safe context-evidence corpus.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextEvidenceCollectionError {
    /// Cooperative cancellation won before a safe corpus was complete.
    #[error("context evidence collection was cancelled")]
    Cancelled,
    /// The mandatory context-pack deadline elapsed.
    #[error("context evidence collection exceeded its deadline")]
    DeadlineExceeded,
    /// A provider returned a mismatched repository, generation, or invocation.
    #[error("context evidence provider returned mismatched identity")]
    IdentityMismatch,
    /// A provider returned an unsafe or internally inconsistent completeness state.
    #[error("context evidence provider returned unsafe completeness")]
    UnsafeCompleteness,
    /// A provider returned corrupt typed evidence.
    #[error("context evidence provider returned invalid evidence")]
    InvalidCandidate(#[from] EvidenceCandidateError),
    /// A provider violated its candidate or measured-usage bounds.
    #[error("context evidence provider violated its admitted bounds")]
    InvalidProviderResponse,
    /// Shared parent accounting rejected measured provider work.
    #[error(transparent)]
    Policy(#[from] ExecutionPolicyError),
}

/// Deterministic collector that validates providers beneath one parent ledger.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextEvidenceCollector;

impl ContextEvidenceCollector {
    /// Executes one provider plan sequentially under a shared parent budget.
    ///
    /// Sequential dispatch deliberately keeps fan-out, cancellation ordering,
    /// and budget admission deterministic. Concrete providers may parallelize
    /// bounded internal reads behind their individual reservation.
    ///
    /// # Errors
    ///
    /// Returns [`ContextEvidenceCollectionError`] for cancellation, deadline,
    /// identity, completeness, candidate, or accounting violations.
    pub async fn collect<P, C>(
        &self,
        port: &P,
        request: &CanonicalContextPackRequest,
        plan: &EvidenceProviderPlan,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ContextEvidenceCorpus, ContextEvidenceCollectionError>
    where
        P: ContextEvidencePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        if plan.request_digest() != request.digest_bytes() {
            return Err(ContextEvidenceCollectionError::IdentityMismatch);
        }

        let mut ledger = BudgetLedger::with_token_limit(u64::from(request.token_budget()));
        let mut candidates = Vec::<TypedEvidenceCandidate>::new();
        let mut omissions = Vec::new();

        for invocation in plan.invocations() {
            collection_checkpoint(&cancellation, deadline)?;
            let reservation = match ledger.reserve(invocation.reservation()) {
                Ok(reservation) => reservation,
                Err(ExecutionPolicyError::BudgetExceeded { resource }) => {
                    record_provider_omission(
                        &mut omissions,
                        invocation.provider(),
                        invocation.role(),
                        EvidenceProviderOmissionReason::Budget,
                        1,
                        vec![LimitingResource::kind(limiting_resource_kind(resource))],
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };

            let response = port
                .retrieve(
                    invocation.clone(),
                    ContextEvidenceCallContext::new(
                        cancellation.clone(),
                        deadline,
                        invocation.reservation(),
                    ),
                )
                .await;
            collection_checkpoint(&cancellation, deadline)?;

            let output = match response {
                Ok(output) => {
                    reservation.commit(output.usage)?;
                    output
                }
                Err(error) => {
                    reservation.commit(error.usage)?;
                    match error.kind {
                        ContextEvidencePortErrorKind::Cancelled => {
                            return Err(ContextEvidenceCollectionError::Cancelled);
                        }
                        ContextEvidencePortErrorKind::DeadlineExceeded => {
                            return Err(ContextEvidenceCollectionError::DeadlineExceeded);
                        }
                        ContextEvidencePortErrorKind::InvalidResponse => {
                            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
                        }
                        ContextEvidencePortErrorKind::Unsupported => {
                            record_provider_omission(
                                &mut omissions,
                                invocation.provider(),
                                invocation.role(),
                                EvidenceProviderOmissionReason::Unsupported,
                                1,
                                vec![LimitingResource::kind(LimitingResourceKind::Capability)],
                            );
                        }
                        ContextEvidencePortErrorKind::Unavailable => {
                            record_provider_omission(
                                &mut omissions,
                                invocation.provider(),
                                invocation.role(),
                                EvidenceProviderOmissionReason::Unavailable,
                                1,
                                vec![LimitingResource::kind(LimitingResourceKind::Coverage)],
                            );
                        }
                    }
                    continue;
                }
            };

            validate_provider_output(request, invocation, &output)?;
            let observations = output
                .observations
                .into_iter()
                .filter(|observation| observation_applies(invocation, observation.kind))
                .collect::<Vec<_>>();
            if observations.is_empty() {
                record_provider_omission(
                    &mut omissions,
                    invocation.provider(),
                    invocation.role(),
                    EvidenceProviderOmissionReason::NoEvidence,
                    1,
                    Vec::new(),
                );
            }
            if output.completeness.state == CompletenessState::Truncated {
                let omitted = u32::from(invocation.max_candidates())
                    .saturating_sub(u32::try_from(observations.len()).unwrap_or(u32::MAX))
                    .max(1);
                record_provider_omission(
                    &mut omissions,
                    invocation.provider(),
                    invocation.role(),
                    EvidenceProviderOmissionReason::Truncated,
                    omitted,
                    output.completeness.limiting_resources.clone(),
                );
            }

            for observation in observations {
                collection_checkpoint(&cancellation, deadline)?;
                let mut draft = shape_provider_observation(
                    request,
                    invocation,
                    output.completeness.state,
                    observation,
                )?;
                if output.completeness.state == CompletenessState::Truncated {
                    // A bounded partial observation remains useful, but it must
                    // not retain the confidence of an exhaustive provider run.
                    draft.confidence = draft.confidence.saturating_sub(100);
                }
                let candidate = TypedEvidenceCandidate::from_draft(
                    request.repository(),
                    request.generation(),
                    draft,
                )?;
                if candidate.confidence() < request.min_confidence() {
                    record_provider_omission(
                        &mut omissions,
                        candidate.provider(),
                        candidate.role(),
                        EvidenceProviderOmissionReason::LowConfidence,
                        1,
                        Vec::new(),
                    );
                    continue;
                }
                candidates.push(candidate);
            }
        }

        let mut candidates = deduplicate_candidates(candidates);
        candidates.sort_by(candidate_order);
        Ok(ContextEvidenceCorpus {
            candidates,
            omissions,
            usage: ledger.consumed(),
            budget: ledger,
        })
    }
}

fn validate_provider_output(
    request: &CanonicalContextPackRequest,
    invocation: &EvidenceProviderInvocation,
    output: &EvidenceProviderOutput,
) -> Result<(), ContextEvidenceCollectionError> {
    if output.repository != request.repository()
        || output.generation != request.generation()
        || output.invocation != *invocation.id()
    {
        return Err(ContextEvidenceCollectionError::IdentityMismatch);
    }
    let applicable_count = output
        .observations
        .iter()
        .filter(|observation| observation_applies(invocation, observation.kind))
        .count();
    let transport_ceiling = usize::from(invocation.max_candidates()).saturating_add(usize::from(
        invocation.provider() == EvidenceProvider::ChangeImpact,
    ));
    if output.observations.len() > transport_ceiling
        || applicable_count > usize::from(invocation.max_candidates())
        || output.usage.results < u64::try_from(applicable_count).unwrap_or(u64::MAX)
    {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
    }
    if !matches!(
        output.completeness.state,
        CompletenessState::Complete | CompletenessState::Truncated
    ) {
        return Err(ContextEvidenceCollectionError::UnsafeCompleteness);
    }
    Ok(())
}

const fn observation_applies(
    invocation: &EvidenceProviderInvocation,
    kind: EvidenceProviderObservationKind,
) -> bool {
    match (invocation.provider(), invocation.role(), kind) {
        (
            EvidenceProvider::ChangeImpact,
            EvidenceRole::Risk,
            EvidenceProviderObservationKind::ChangeRiskSummary,
        ) => true,
        (
            EvidenceProvider::ChangeImpact,
            EvidenceRole::Risk,
            EvidenceProviderObservationKind::Primary,
        )
        | (EvidenceProvider::ChangeImpact, _, EvidenceProviderObservationKind::ChangeRiskSummary) => {
            false
        }
        (_, _, EvidenceProviderObservationKind::Primary) => true,
        (_, _, EvidenceProviderObservationKind::ChangeRiskSummary) => false,
    }
}

fn shape_provider_observation(
    request: &CanonicalContextPackRequest,
    invocation: &EvidenceProviderInvocation,
    completeness: CompletenessState,
    observation: EvidenceProviderObservation,
) -> Result<EvidenceCandidateDraft, ContextEvidenceCollectionError> {
    let confidence = match observation.observed_score {
        Some(score) => score,
        None if invocation.provider() == EvidenceProvider::Planning => match completeness {
            CompletenessState::Complete => 900,
            CompletenessState::Truncated => 700,
            CompletenessState::UnsupportedPartial | CompletenessState::Indeterminate => {
                return Err(ContextEvidenceCollectionError::UnsafeCompleteness);
            }
        },
        None if invocation.provider() == EvidenceProvider::ChangeImpact
            && observation.kind == EvidenceProviderObservationKind::ChangeRiskSummary =>
        {
            700
        }
        None => return Err(ContextEvidenceCollectionError::InvalidProviderResponse),
    };

    let relevance = observation.observed_relevance.unwrap_or(confidence);
    if confidence > 1_000 || relevance > 1_000 {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
    }

    Ok(EvidenceCandidateDraft {
        repository: request.repository(),
        generation: request.generation(),
        invocation: invocation.id().clone(),
        provider: invocation.provider(),
        role: invocation.role(),
        provenance: provider_provenance(invocation.provider()),
        symbol_id: observation.symbol_id,
        identity: observation.identity,
        relevance,
        confidence,
        cost: candidate_cost(observation.estimated_tokens, observation.source_bytes),
        source_refs: observation.source_refs,
        dependencies: Vec::new(),
    })
}

const fn provider_provenance(provider: EvidenceProvider) -> EvidenceProvenance {
    match provider {
        EvidenceProvider::Locate
        | EvidenceProvider::Definition
        | EvidenceProvider::Relationships => EvidenceProvenance::Graph,
        EvidenceProvider::Implementation | EvidenceProvider::Source => EvidenceProvenance::Source,
        EvidenceProvider::Tests => EvidenceProvenance::TestIndex,
        EvidenceProvider::Architecture => EvidenceProvenance::ArchitectureAnalysis,
        EvidenceProvider::ChangeImpact => EvidenceProvenance::ChangeAnalysis,
        EvidenceProvider::History => EvidenceProvenance::HistoryAnalysis,
        EvidenceProvider::Planning => EvidenceProvenance::PlanArtifact,
    }
}

fn candidate_cost(estimated_tokens: u64, source_bytes: u64) -> BudgetCharge {
    let tokens = estimated_tokens.clamp(1, 32_000);
    BudgetCharge {
        results: 1,
        tokens,
        source_bytes,
        memory_bytes: estimated_tokens.saturating_add(source_bytes),
        ..BudgetCharge::default()
    }
}

fn deduplicate_candidates(
    mut candidates: Vec<TypedEvidenceCandidate>,
) -> Vec<TypedEvidenceCandidate> {
    candidates.sort_by(|left, right| {
        candidate_is_better(left, right)
            .cmp(&candidate_is_better(right, left))
            .reverse()
            .then_with(|| left.id().cmp(right.id()))
    });
    let mut retained = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if retained
            .iter()
            .any(|existing| candidates_are_equivalent(existing, &candidate))
        {
            continue;
        }
        retained.push(candidate);
    }
    retained
}

fn candidate_is_better(
    candidate: &TypedEvidenceCandidate,
    current: &TypedEvidenceCandidate,
) -> bool {
    candidate
        .confidence()
        .cmp(&current.confidence())
        .then_with(|| candidate.relevance().cmp(&current.relevance()))
        .then_with(|| current.id().cmp(candidate.id()))
        .is_gt()
}

fn candidates_are_equivalent(
    left: &TypedEvidenceCandidate,
    right: &TypedEvidenceCandidate,
) -> bool {
    if left.role() != right.role() {
        return false;
    }
    if left.symbol_id().is_some() && left.symbol_id() == right.symbol_id() {
        return true;
    }
    if left.identity() == right.identity() {
        return true;
    }
    left.source_refs().iter().any(|left_ref| {
        right.source_refs().iter().any(|right_ref| {
            left_ref.repository() == right_ref.repository()
                && left_ref.generation() == right_ref.generation()
                && left_ref.span().file() == right_ref.span().file()
                && left_ref.span().start_byte() < right_ref.span().end_byte()
                && right_ref.span().start_byte() < left_ref.span().end_byte()
        })
    })
}

fn candidate_order(
    left: &TypedEvidenceCandidate,
    right: &TypedEvidenceCandidate,
) -> std::cmp::Ordering {
    left.role()
        .priority()
        .cmp(&right.role().priority())
        .then_with(|| right.relevance().cmp(&left.relevance()))
        .then_with(|| right.confidence().cmp(&left.confidence()))
        .then_with(|| left.dedup_key().cmp(right.dedup_key()))
        .then_with(|| left.id().cmp(right.id()))
}

fn record_provider_omission(
    omissions: &mut Vec<EvidenceProviderOmission>,
    provider: EvidenceProvider,
    role: EvidenceRole,
    reason: EvidenceProviderOmissionReason,
    count: u32,
    mut limiting_resources: Vec<LimitingResource>,
) {
    limiting_resources.sort_unstable();
    limiting_resources.dedup_by_key(|resource| resource.kind);
    if let Some(existing) = omissions.iter_mut().find(|omission| {
        omission.provider == provider && omission.role == role && omission.reason == reason
    }) {
        existing.count = existing.count.saturating_add(count);
        existing.limiting_resources.append(&mut limiting_resources);
        existing.limiting_resources.sort_unstable();
        existing
            .limiting_resources
            .dedup_by_key(|resource| resource.kind);
    } else if omissions.len() < MAX_PROVIDER_OMISSIONS {
        omissions.push(EvidenceProviderOmission {
            provider,
            role,
            reason,
            count,
            limiting_resources,
        });
    }
}

fn collection_checkpoint<C>(
    cancellation: &C,
    deadline: Instant,
) -> Result<(), ContextEvidenceCollectionError>
where
    C: CancellationSignal,
{
    if cancellation.is_cancelled() {
        Err(ContextEvidenceCollectionError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ContextEvidenceCollectionError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

const fn limiting_resource_kind(resource: BudgetResource) -> LimitingResourceKind {
    match resource {
        BudgetResource::Rows => LimitingResourceKind::Rows,
        BudgetResource::Results => LimitingResourceKind::Results,
        BudgetResource::Tokens | BudgetResource::ActualTokens => {
            LimitingResourceKind::EstimatedTokens
        }
        BudgetResource::SourceBytes => LimitingResourceKind::SourceBytes,
        BudgetResource::TraversalFacts => LimitingResourceKind::Edges,
        BudgetResource::Depth => LimitingResourceKind::Depth,
        BudgetResource::Paths => LimitingResourceKind::Paths,
        BudgetResource::JsonBytes => LimitingResourceKind::ResponseBytes,
        BudgetResource::MemoryBytes => LimitingResourceKind::MemoryBytes,
        BudgetResource::Time => LimitingResourceKind::Deadline,
    }
}

fn grouped_anchors(
    request: &CanonicalContextPackRequest,
) -> Vec<(EvidenceSeedKind, Vec<EvidenceAnchor>)> {
    let seeds = request.seeds();
    let mut groups = Vec::new();
    push_group(
        &mut groups,
        EvidenceSeedKind::Symbol,
        seeds
            .symbols()
            .iter()
            .copied()
            .map(EvidenceAnchor::Symbol)
            .collect(),
    );
    push_group(
        &mut groups,
        EvidenceSeedKind::Path,
        seeds
            .paths()
            .iter()
            .cloned()
            .map(EvidenceAnchor::Path)
            .collect(),
    );
    push_group(
        &mut groups,
        EvidenceSeedKind::Route,
        seeds
            .routes()
            .iter()
            .cloned()
            .map(EvidenceAnchor::Route)
            .collect(),
    );
    push_group(
        &mut groups,
        EvidenceSeedKind::Test,
        seeds
            .tests()
            .iter()
            .copied()
            .map(EvidenceAnchor::Test)
            .collect(),
    );
    if let Some(value) = seeds.located() {
        groups.push((
            EvidenceSeedKind::Located,
            vec![EvidenceAnchor::Located(value.as_str().to_owned())],
        ));
    }
    if let Some(value) = seeds.change() {
        groups.push((
            EvidenceSeedKind::Change,
            vec![EvidenceAnchor::Change(value.to_owned())],
        ));
    }
    if let Some(value) = seeds.plan() {
        groups.push((
            EvidenceSeedKind::Plan,
            vec![EvidenceAnchor::Plan(value.to_owned())],
        ));
    }
    groups
}

fn push_group(
    groups: &mut Vec<(EvidenceSeedKind, Vec<EvidenceAnchor>)>,
    kind: EvidenceSeedKind,
    values: Vec<EvidenceAnchor>,
) {
    if !values.is_empty() {
        groups.push((kind, values));
    }
}

const fn provider_for(kind: EvidenceSeedKind, role: EvidenceRole) -> EvidenceProvider {
    match role {
        EvidenceRole::Definition => match kind {
            EvidenceSeedKind::Symbol | EvidenceSeedKind::Test => EvidenceProvider::Definition,
            EvidenceSeedKind::Path | EvidenceSeedKind::Route | EvidenceSeedKind::Located => {
                EvidenceProvider::Locate
            }
            EvidenceSeedKind::Change | EvidenceSeedKind::Plan => EvidenceProvider::Planning,
        },
        EvidenceRole::Implementation => EvidenceProvider::Implementation,
        EvidenceRole::Caller => EvidenceProvider::Relationships,
        EvidenceRole::Test => EvidenceProvider::Tests,
        EvidenceRole::Risk => EvidenceProvider::ChangeImpact,
        EvidenceRole::Architecture => EvidenceProvider::Architecture,
        EvidenceRole::Change => match kind {
            EvidenceSeedKind::Plan => EvidenceProvider::Planning,
            _ => EvidenceProvider::ChangeImpact,
        },
    }
}

fn push_invocation(
    request: &CanonicalContextPackRequest,
    seed_kind: EvidenceSeedKind,
    provider: EvidenceProvider,
    role: EvidenceRole,
    anchors: &[EvidenceAnchor],
    invocations: &mut Vec<EvidenceProviderInvocation>,
) -> Result<(), EvidenceProviderPlanError> {
    if invocations.len() >= MAX_CONTEXT_PROVIDER_CALLS {
        return Err(EvidenceProviderPlanError::FanoutExceeded);
    }
    if invocations
        .iter()
        .any(|value| value.provider == provider && value.role == role && value.anchors == anchors)
    {
        return Ok(());
    }

    let anchor_count = u16::try_from(anchors.len()).unwrap_or(u16::MAX);
    let max_candidates = anchor_count
        .saturating_mul(provider_results_per_anchor(provider, seed_kind))
        .clamp(1, MAX_CANDIDATES_PER_PROVIDER);
    let reservation = provider_reservation(provider, max_candidates);
    let id = provider_invocation_id(request, provider, role, anchors);
    invocations.push(EvidenceProviderInvocation {
        id,
        repository: request.repository(),
        generation: request.generation(),
        objective: request.objective(),
        task: request.task().to_owned(),
        provider,
        role,
        anchors: anchors.to_vec(),
        max_candidates,
        reservation,
    });
    Ok(())
}

const fn provider_results_per_anchor(
    provider: EvidenceProvider,
    seed_kind: EvidenceSeedKind,
) -> u16 {
    match provider {
        EvidenceProvider::Definition => 1,
        EvidenceProvider::Locate => 2,
        EvidenceProvider::Implementation | EvidenceProvider::Source => match seed_kind {
            EvidenceSeedKind::Path | EvidenceSeedKind::Route => 2,
            EvidenceSeedKind::Symbol
            | EvidenceSeedKind::Test
            | EvidenceSeedKind::Located
            | EvidenceSeedKind::Change
            | EvidenceSeedKind::Plan => 1,
        },
        EvidenceProvider::Relationships | EvidenceProvider::Tests => 8,
        EvidenceProvider::Architecture
        | EvidenceProvider::ChangeImpact
        | EvidenceProvider::History
        | EvidenceProvider::Planning => 4,
    }
}

fn provider_reservation(provider: EvidenceProvider, max_candidates: u16) -> BudgetCharge {
    let results = max_candidates as u64;
    // Symbol explanation accounts the entity, provenance, and bounded related
    // evidence as four internal results in the minimum useful response. The
    // provider still projects one pack candidate.
    let transport_results = match provider {
        EvidenceProvider::Definition
        | EvidenceProvider::Source
        | EvidenceProvider::Implementation => results.saturating_mul(4),
        // Relationship retrieval also explains every bounded target once so
        // task-mentioned symbols can be ranked without inventing hidden seeds.
        EvidenceProvider::Relationships => results.saturating_mul(5),
        _ => results,
    };
    let per_result_tokens = match provider {
        EvidenceProvider::Source | EvidenceProvider::Implementation => 512,
        // Architecture rows include component, responsibility, connection, and
        // derived-view envelopes even though each row yields one pack candidate.
        EvidenceProvider::Architecture => 512,
        EvidenceProvider::Relationships => 512,
        EvidenceProvider::Tests
        | EvidenceProvider::ChangeImpact
        | EvidenceProvider::History
        | EvidenceProvider::Planning => 192,
        EvidenceProvider::Definition => 384,
        EvidenceProvider::Locate => 96,
    };
    let source_bytes = match provider {
        EvidenceProvider::Source | EvidenceProvider::Implementation => results * 2_048,
        EvidenceProvider::Relationships => results * 512,
        _ => results * 256,
    };
    let rows_per_result = match provider {
        // Component discovery scans bounded entity and relation prefixes in
        // addition to the rows represented by returned components.
        EvidenceProvider::Architecture => 64,
        // Definition lookup scans the bounded symbol index before projecting
        // the requested explanations, so returned candidates understate rows.
        EvidenceProvider::Definition => 224,
        // Source evidence resolves definitions before reading their exact
        // spans, so its reservation must cover both bounded operations.
        EvidenceProvider::Source | EvidenceProvider::Implementation => 225,
        // Bidirectional relationship traversal is followed by one bounded
        // symbol-explanation batch for task relevance.
        EvidenceProvider::Relationships => 232,
        _ => 8,
    };
    // Every daemon response may account structural edges even when the
    // adapter ultimately emits a non-relationship role. Reserve the protocol
    // maximum per returned candidate so measured child usage cannot exceed the
    // parent allocation merely because identity resolution traversed edges.
    let traversal_facts_per_result = match provider {
        EvidenceProvider::Definition
        | EvidenceProvider::Source
        | EvidenceProvider::Implementation => 16,
        // Relationship evidence performs one bounded graph traversal and one
        // bounded explanation pass over the resulting symbols.
        EvidenceProvider::Relationships => 48,
        _ => 8,
    };
    let traversal_facts = results * traversal_facts_per_result;
    let envelope_bytes_per_result = match provider {
        EvidenceProvider::Definition
        | EvidenceProvider::Source
        | EvidenceProvider::Implementation => 16_384,
        EvidenceProvider::Relationships => 20_480,
        _ => 4_096,
    };
    let paths_per_result = if provider == EvidenceProvider::Relationships {
        2
    } else {
        1
    };
    BudgetCharge {
        rows: results.saturating_mul(rows_per_result),
        results: transport_results,
        tokens: results.saturating_mul(per_result_tokens),
        source_bytes,
        traversal_facts,
        depth: 4,
        paths: results.saturating_mul(paths_per_result),
        json_bytes: results.saturating_mul(envelope_bytes_per_result),
        memory_bytes: results.saturating_mul(envelope_bytes_per_result),
        time_ms: 2_000,
        ..BudgetCharge::default()
    }
}

fn provider_invocation_id(
    request: &CanonicalContextPackRequest,
    provider: EvidenceProvider,
    role: EvidenceRole,
    anchors: &[EvidenceAnchor],
) -> ProviderInvocationId {
    let mut hasher =
        blake3::Hasher::new_derive_key("rootlight.context-evidence.provider-invocation.v1");
    hasher.update(&request.digest_bytes());
    hasher.update(&[provider.tag(), role.priority()]);
    hash_count(&mut hasher, anchors.len());
    for anchor in anchors {
        anchor.hash_into(&mut hasher);
    }
    ProviderInvocationId(format!("ctxcall1_{}", hasher.finalize().to_hex()))
}

fn candidate_dedup_key(draft: &EvidenceCandidateDraft) -> EvidenceDedupKey {
    let mut hasher = blake3::Hasher::new_derive_key("rootlight.context-evidence.dedup.v1");
    hasher.update(draft.repository.as_bytes());
    hasher.update(draft.generation.as_bytes());
    hasher.update(&[draft.role.priority()]);
    hash_bytes(&mut hasher, draft.identity.as_bytes());
    if let Some(symbol) = draft.symbol_id {
        hasher.update(&[1]);
        hash_bytes(&mut hasher, symbol.as_bytes());
    } else {
        hasher.update(&[0]);
    }
    hash_count(&mut hasher, draft.source_refs.len());
    for source in &draft.source_refs {
        hash_source_ref(&mut hasher, source);
    }
    EvidenceDedupKey(format!("ctxdedup1_{}", hasher.finalize().to_hex()))
}

fn candidate_id(
    draft: &EvidenceCandidateDraft,
    dedup_key: &EvidenceDedupKey,
) -> EvidenceCandidateId {
    let mut hasher = blake3::Hasher::new_derive_key("rootlight.context-evidence.candidate.v1");
    hash_bytes(&mut hasher, dedup_key.as_str().as_bytes());
    hash_bytes(&mut hasher, draft.invocation.as_str().as_bytes());
    hasher.update(&[draft.provider.tag()]);
    hasher.update(&[draft.provenance.tag()]);
    hasher.update(&draft.relevance.to_le_bytes());
    hasher.update(&draft.confidence.to_le_bytes());
    hash_budget(&mut hasher, draft.cost);
    hash_count(&mut hasher, draft.dependencies.len());
    for dependency in &draft.dependencies {
        hash_bytes(&mut hasher, dependency.as_str().as_bytes());
    }
    EvidenceCandidateId(format!("ctxcand1_{}", hasher.finalize().to_hex()))
}

fn hash_source_ref(hasher: &mut blake3::Hasher, source: &SourceRef) {
    hasher.update(source.repository().as_bytes());
    hasher.update(source.generation().as_bytes());
    let span = source.span();
    hasher.update(span.file().as_bytes());
    hasher.update(&span.start_byte().to_le_bytes());
    hasher.update(&span.end_byte().to_le_bytes());
    hasher.update(source.content_hash().as_bytes());
}

fn hash_budget(hasher: &mut blake3::Hasher, budget: BudgetCharge) {
    for value in [
        budget.rows,
        budget.results,
        budget.tokens,
        budget.actual_tokens,
        budget.source_bytes,
        budget.traversal_facts,
        budget.depth,
        budget.paths,
        budget.json_bytes,
        budget.memory_bytes,
        budget.time_ms,
    ] {
        hasher.update(&value.to_le_bytes());
    }
}

fn hash_count(hasher: &mut blake3::Hasher, value: usize) {
    hasher.update(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_count(hasher, value.len());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeSet, VecDeque},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use rootlight_ids::{ContentHash, FileId};
    use rootlight_ir::SourceSpan;
    use rootlight_mcp_contract::{
        RepositorySelector,
        completeness::{ContinuationAvailability, ContinuationGuidance},
        context::{ContextPackInput, ContextSeedSelector},
        vertical::{ContinuationCursor, RepositoryIdSelector},
    };

    use crate::{
        context_pack::DefaultContextPackPlanner,
        policy::{CancellationSignal, NeverCancelled},
    };
    use proptest::prelude::*;

    use super::*;

    const REPOSITORY: RepositoryId = RepositoryId::from_bytes([1; 16]);
    const GENERATION: GenerationId = GenerationId::from_bytes([2; 20]);

    fn request(task: &str) -> CanonicalContextPackRequest {
        request_with_source_policy(task, None)
    }

    fn request_with_source_policy(
        task: &str,
        source_policy: Option<SourcePolicy>,
    ) -> CanonicalContextPackRequest {
        let input = ContextPackInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: REPOSITORY,
            }),
            generation: None,
            task: task.to_owned(),
            seeds: ContextSeedSelector {
                symbols: Some(vec![SymbolId::from_bytes([3; 20])]),
                paths: Some(vec!["src/lib.rs".to_owned()]),
                routes: Some(vec!["GET /health".to_owned()]),
                tests: Some(vec![SymbolId::from_bytes([4; 20])]),
                located: Some(
                    ContinuationCursor::parse("located-1").expect("valid located cursor"),
                ),
                change: Some("change-1".to_owned()),
                plan: Some("plan-1".to_owned()),
            },
            token_budget: 4_500,
            source_policy,
            sections: None,
            diversity: None,
            min_confidence: None,
            response_profile: None,
            continuation: None,
            explain: None,
        };
        CanonicalContextPackRequest::new(&input, REPOSITORY, GENERATION)
            .expect("fixture request canonicalizes")
    }

    fn source_ref(repository: RepositoryId, generation: GenerationId) -> SourceRef {
        ranged_source_ref(repository, generation, 10, 30)
    }

    fn ranged_source_ref(
        repository: RepositoryId,
        generation: GenerationId,
        start: u64,
        end: u64,
    ) -> SourceRef {
        SourceRef::new(
            repository,
            generation,
            SourceSpan::new(FileId::from_bytes([5; 20]), start, end).expect("valid source span"),
            ContentHash::from_bytes([6; 32]),
            None,
        )
    }

    fn symbol_request(symbols: Vec<SymbolId>) -> CanonicalContextPackRequest {
        let input = ContextPackInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: REPOSITORY,
            }),
            generation: None,
            task: "fix crash".to_owned(),
            seeds: ContextSeedSelector {
                symbols: Some(symbols),
                paths: None,
                routes: None,
                tests: None,
                located: None,
                change: None,
                plan: None,
            },
            token_budget: 4_500,
            source_policy: None,
            sections: None,
            diversity: None,
            min_confidence: None,
            response_profile: None,
            continuation: None,
            explain: None,
        };
        CanonicalContextPackRequest::new(&input, REPOSITORY, GENERATION)
            .expect("symbol request canonicalizes")
    }

    #[derive(Debug, Default)]
    struct FakeEvidencePort {
        responses: Mutex<VecDeque<Result<EvidenceProviderOutput, ContextEvidencePortError>>>,
        calls: AtomicUsize,
    }

    impl FakeEvidencePort {
        fn with_responses(
            responses: impl IntoIterator<
                Item = Result<EvidenceProviderOutput, ContextEvidencePortError>,
            >,
        ) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl<C> ContextEvidencePort<C> for FakeEvidencePort
    where
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        fn retrieve(
            &self,
            _invocation: EvidenceProviderInvocation,
            _context: ContextEvidenceCallContext<C>,
        ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .lock()
                .expect("fake response lock")
                .pop_front()
                .expect("fake response exists");
            Box::pin(async move { response })
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct AlwaysCancelled;

    impl CancellationSignal for AlwaysCancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn one_invocation_plan(request: &CanonicalContextPackRequest) -> EvidenceProviderPlan {
        let invocation = ContextEvidenceProviderRegistry
            .plan(request)
            .expect("full provider plan")
            .invocations()[0]
            .clone();
        EvidenceProviderPlan {
            request_digest: request.digest_bytes(),
            invocations: vec![invocation],
        }
    }

    fn complete_output(
        invocation: &EvidenceProviderInvocation,
        identity: &str,
    ) -> EvidenceProviderOutput {
        EvidenceProviderOutput {
            repository: REPOSITORY,
            generation: GENERATION,
            invocation: invocation.id().clone(),
            observations: vec![EvidenceProviderObservation {
                kind: EvidenceProviderObservationKind::Primary,
                symbol_id: Some(SymbolId::from_bytes([3; 20])),
                identity: identity.to_owned(),
                observed_score: Some(900),
                observed_relevance: None,
                estimated_tokens: 64,
                source_bytes: 0,
                source_refs: vec![source_ref(REPOSITORY, GENERATION)],
            }],
            completeness: ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: 1,
                tokens: 64,
                ..BudgetCharge::default()
            },
        }
    }

    #[test]
    fn provider_plan_covers_every_seed_kind_and_required_role() {
        let request = request("fix crash");
        let plan = ContextEvidenceProviderRegistry
            .plan(&request)
            .expect("provider plan is bounded");

        for kind in [
            EvidenceSeedKind::Symbol,
            EvidenceSeedKind::Path,
            EvidenceSeedKind::Route,
            EvidenceSeedKind::Test,
            EvidenceSeedKind::Located,
            EvidenceSeedKind::Change,
            EvidenceSeedKind::Plan,
        ] {
            for role in request.objective().required_roles() {
                assert!(plan.invocations().iter().any(|invocation| {
                    invocation.role() == *role
                        && invocation
                            .anchors()
                            .iter()
                            .all(|anchor| anchor.kind() == kind)
                }));
            }
        }
        assert!(plan.invocations().len() <= MAX_CONTEXT_PROVIDER_CALLS);
    }

    #[test]
    fn architecture_provider_reserves_its_full_bounded_envelope() {
        let reservation = provider_reservation(EvidenceProvider::Architecture, 4);

        assert_eq!(reservation.results, 4);
        assert_eq!(reservation.tokens, 2_048);
        assert_eq!(reservation.rows, 256);
        assert_eq!(reservation.json_bytes, 16_384);
        assert_eq!(reservation.traversal_facts, 32);
    }

    #[test]
    fn relationship_provider_reserves_traversal_and_task_ranking() {
        let reservation = provider_reservation(EvidenceProvider::Relationships, 8);

        assert_eq!(reservation.results, 40);
        assert_eq!(reservation.tokens, 4_096);
        assert_eq!(reservation.rows, 1_856);
        assert_eq!(reservation.source_bytes, 4_096);
        assert_eq!(reservation.traversal_facts, 384);
        assert_eq!(reservation.paths, 16);
        assert_eq!(reservation.json_bytes, 163_840);
        assert_eq!(reservation.memory_bytes, 163_840);
    }

    #[test]
    fn sections_select_roles_while_response_profile_is_representation_only() {
        let make_request = |profile| {
            let public = ContextPackInput {
                repository: RepositorySelector::ById(RepositoryIdSelector {
                    repository_id: REPOSITORY,
                }),
                generation: None,
                task: "explain parser".to_owned(),
                seeds: ContextSeedSelector {
                    symbols: Some(vec![SymbolId::from_bytes([3; 20])]),
                    paths: None,
                    routes: None,
                    tests: None,
                    located: None,
                    change: None,
                    plan: None,
                },
                token_budget: 4_500,
                source_policy: None,
                sections: Some(vec![
                    ContextSection::Definitions,
                    ContextSection::Architecture,
                    ContextSection::Tests,
                ]),
                diversity: None,
                min_confidence: None,
                response_profile: profile,
                continuation: None,
                explain: None,
            };
            CanonicalContextPackRequest::new(&public, REPOSITORY, GENERATION)
                .expect("sectioned request canonicalizes")
        };
        let compact = make_request(Some(
            rootlight_mcp_contract::vertical::ResponseProfile::Compact,
        ));
        let evidence = make_request(Some(
            rootlight_mcp_contract::vertical::ResponseProfile::Evidence,
        ));
        assert_eq!(
            compact.requested_roles(),
            vec![
                EvidenceRole::Definition,
                EvidenceRole::Test,
                EvidenceRole::Architecture,
            ]
        );
        let plan_projection = |request: &CanonicalContextPackRequest| {
            ContextEvidenceProviderRegistry
                .plan(request)
                .expect("profile plan")
                .invocations()
                .iter()
                .map(|invocation| {
                    (
                        invocation.provider(),
                        invocation.role(),
                        invocation.anchors().to_vec(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(plan_projection(&compact), plan_projection(&evidence));
    }

    #[test]
    fn all_objectives_have_deterministic_bounded_plans() {
        let mut observed_providers = BTreeSet::new();
        for task in [
            "fix crash",
            "refactor parser",
            "explain parser",
            "migrate parser",
            "security review",
        ] {
            let request = request(task);
            let left = ContextEvidenceProviderRegistry
                .plan(&request)
                .expect("left provider plan");
            let right = ContextEvidenceProviderRegistry
                .plan(&request)
                .expect("right provider plan");
            assert_eq!(left, right);
            assert!(!left.invocations().is_empty());
            assert!(left.invocations().len() <= MAX_CONTEXT_PROVIDER_CALLS);
            assert!(left.invocations().iter().all(|invocation| {
                invocation.max_candidates() <= MAX_CANDIDATES_PER_PROVIDER
                    && invocation.reservation().results > 0
            }));
            observed_providers.extend(
                left.invocations()
                    .iter()
                    .map(EvidenceProviderInvocation::provider),
            );
            for kind in [
                EvidenceSeedKind::Symbol,
                EvidenceSeedKind::Path,
                EvidenceSeedKind::Route,
                EvidenceSeedKind::Test,
                EvidenceSeedKind::Located,
                EvidenceSeedKind::Change,
                EvidenceSeedKind::Plan,
            ] {
                for role in request.objective().required_roles() {
                    assert!(left.invocations().iter().any(|invocation| {
                        invocation.role() == *role
                            && invocation
                                .anchors()
                                .iter()
                                .all(|anchor| anchor.kind() == kind)
                    }));
                }
            }
        }

        let source_request =
            request_with_source_policy("migrate parser", Some(SourcePolicy::FocusedSnippets));
        let source_plan = ContextEvidenceProviderRegistry
            .plan(&source_request)
            .expect("source-aware provider plan");
        assert!(
            source_plan
                .invocations()
                .iter()
                .all(|invocation| invocation.provider() != EvidenceProvider::Source),
            "raw source is a second-stage operation after evidence selection"
        );
        assert_eq!(
            observed_providers,
            BTreeSet::from([
                EvidenceProvider::Locate,
                EvidenceProvider::Definition,
                EvidenceProvider::Implementation,
                EvidenceProvider::Relationships,
                EvidenceProvider::Tests,
                EvidenceProvider::Architecture,
                EvidenceProvider::ChangeImpact,
                EvidenceProvider::History,
                EvidenceProvider::Planning,
            ])
        );
    }

    #[test]
    fn definition_reservation_covers_bounded_explanation_envelopes() {
        let plan = ContextEvidenceProviderRegistry
            .plan(&request("explain parser"))
            .expect("definition provider plan");
        let definition = plan
            .invocations()
            .iter()
            .find(|invocation| invocation.provider() == EvidenceProvider::Definition)
            .expect("definition invocation");

        assert_eq!(
            definition.reservation().tokens,
            u64::from(definition.max_candidates()).saturating_mul(384)
        );
        assert_eq!(
            definition.reservation().rows,
            u64::from(definition.max_candidates()).saturating_mul(224)
        );
        assert_eq!(
            definition.reservation().traversal_facts,
            u64::from(definition.max_candidates()).saturating_mul(16)
        );
        assert_eq!(
            definition.reservation().results,
            u64::from(definition.max_candidates()).saturating_mul(4)
        );
        assert_eq!(
            definition.reservation().json_bytes,
            u64::from(definition.max_candidates()).saturating_mul(16_384)
        );
    }

    #[test]
    fn explicit_symbols_reserve_one_definition_and_implementation_each() {
        let request = symbol_request(vec![
            SymbolId::from_bytes([3; 20]),
            SymbolId::from_bytes([4; 20]),
        ]);
        let plan = ContextEvidenceProviderRegistry
            .plan(&request)
            .expect("symbol provider plan");

        for provider in [
            EvidenceProvider::Definition,
            EvidenceProvider::Implementation,
        ] {
            let invocation = plan
                .invocations()
                .iter()
                .find(|invocation| invocation.provider() == provider)
                .expect("symbol provider invocation");
            assert_eq!(invocation.max_candidates(), 2);
        }
    }

    #[test]
    fn implementation_reservation_covers_definition_and_source_reads() {
        let plan = ContextEvidenceProviderRegistry
            .plan(&request("inspect parser source"))
            .expect("implementation provider plan");
        let implementation = plan
            .invocations()
            .iter()
            .find(|invocation| invocation.provider() == EvidenceProvider::Implementation)
            .expect("implementation invocation");

        assert_eq!(
            implementation.reservation().rows,
            u64::from(implementation.max_candidates()).saturating_mul(225)
        );
        assert_eq!(
            implementation.reservation().results,
            u64::from(implementation.max_candidates()).saturating_mul(4)
        );
        assert_eq!(
            implementation.reservation().traversal_facts,
            u64::from(implementation.max_candidates()).saturating_mul(16)
        );
    }

    #[test]
    fn candidate_identity_is_stable_and_provider_independent_dedup_is_shared() {
        let invocation = ContextEvidenceProviderRegistry
            .plan(&request("fix crash"))
            .expect("provider plan")
            .invocations()[0]
            .id()
            .clone();
        let draft = EvidenceCandidateDraft {
            repository: REPOSITORY,
            generation: GENERATION,
            invocation,
            provider: EvidenceProvider::Definition,
            role: EvidenceRole::Definition,
            provenance: EvidenceProvenance::Graph,
            symbol_id: Some(SymbolId::from_bytes([3; 20])),
            identity: "parser-definition".to_owned(),
            relevance: 900,
            confidence: 850,
            cost: BudgetCharge {
                results: 1,
                tokens: 64,
                ..BudgetCharge::default()
            },
            source_refs: vec![source_ref(REPOSITORY, GENERATION)],
            dependencies: Vec::new(),
        };

        let left = TypedEvidenceCandidate::from_draft(REPOSITORY, GENERATION, draft.clone())
            .expect("left candidate");
        let right = TypedEvidenceCandidate::from_draft(REPOSITORY, GENERATION, draft)
            .expect("right candidate");
        assert_eq!(left.id(), right.id());
        assert_eq!(left.dedup_key(), right.dedup_key());
        assert_eq!(left.trust(), TrustClassification::UntrustedRepositoryData);
    }

    #[test]
    fn agent_shapes_transport_observations_from_the_authoritative_invocation() {
        let request = request("fix crash");
        let invocation = one_invocation_plan(&request)
            .invocations()
            .first()
            .expect("fixture invocation")
            .clone();
        let source = source_ref(REPOSITORY, GENERATION);
        let draft = shape_provider_observation(
            &request,
            &invocation,
            CompletenessState::Complete,
            EvidenceProviderObservation {
                kind: EvidenceProviderObservationKind::Primary,
                symbol_id: Some(SymbolId::from_bytes([3; 20])),
                identity: "parser".to_owned(),
                observed_score: Some(875),
                observed_relevance: None,
                estimated_tokens: 40_000,
                source_bytes: 128,
                source_refs: vec![source.clone()],
            },
        )
        .expect("scored transport observation is accepted");

        assert_eq!(draft.repository, request.repository());
        assert_eq!(draft.generation, request.generation());
        assert_eq!(draft.invocation, *invocation.id());
        assert_eq!(draft.provider, invocation.provider());
        assert_eq!(draft.role, invocation.role());
        assert_eq!(draft.provenance, provider_provenance(invocation.provider()));
        assert_eq!(draft.relevance, 875);
        assert_eq!(draft.confidence, 875);
        assert_eq!(draft.cost.results, 1);
        assert_eq!(draft.cost.tokens, 32_000);
        assert_eq!(draft.cost.source_bytes, 128);
        assert_eq!(draft.cost.memory_bytes, 40_128);
        assert_eq!(draft.source_refs, [source]);
        assert!(draft.dependencies.is_empty());

        let candidate =
            TypedEvidenceCandidate::from_draft(request.repository(), request.generation(), draft)
                .expect("agent-shaped candidate validates");
        assert_eq!(
            candidate.trust(),
            TrustClassification::UntrustedRepositoryData
        );
    }

    #[test]
    fn provider_provenance_policy_covers_every_transport_adapter() {
        for (provider, expected) in [
            (EvidenceProvider::Locate, EvidenceProvenance::Graph),
            (EvidenceProvider::Definition, EvidenceProvenance::Graph),
            (EvidenceProvider::Implementation, EvidenceProvenance::Source),
            (EvidenceProvider::Relationships, EvidenceProvenance::Graph),
            (EvidenceProvider::Tests, EvidenceProvenance::TestIndex),
            (
                EvidenceProvider::Architecture,
                EvidenceProvenance::ArchitectureAnalysis,
            ),
            (
                EvidenceProvider::ChangeImpact,
                EvidenceProvenance::ChangeAnalysis,
            ),
            (
                EvidenceProvider::History,
                EvidenceProvenance::HistoryAnalysis,
            ),
            (EvidenceProvider::Planning, EvidenceProvenance::PlanArtifact),
            (EvidenceProvider::Source, EvidenceProvenance::Source),
        ] {
            assert_eq!(provider_provenance(provider), expected);
        }
    }

    #[test]
    fn agent_assigns_confidence_when_transport_has_no_native_score() {
        let request = request("migrate parser");
        let plan = ContextEvidenceProviderRegistry
            .plan(&request)
            .expect("provider plan");
        for (provider, expected) in [
            (EvidenceProvider::ChangeImpact, 700),
            (EvidenceProvider::Planning, 900),
        ] {
            let invocation = plan
                .invocations()
                .iter()
                .find(|invocation| invocation.provider() == provider)
                .expect("provider invocation");
            let draft = shape_provider_observation(
                &request,
                invocation,
                CompletenessState::Complete,
                EvidenceProviderObservation {
                    kind: if provider == EvidenceProvider::ChangeImpact {
                        EvidenceProviderObservationKind::ChangeRiskSummary
                    } else {
                        EvidenceProviderObservationKind::Primary
                    },
                    symbol_id: None,
                    identity: provider.name().to_owned(),
                    observed_score: None,
                    observed_relevance: None,
                    estimated_tokens: 1,
                    source_bytes: 0,
                    source_refs: Vec::new(),
                },
            )
            .expect("agent policy assigns confidence");
            assert_eq!(draft.confidence, expected);
            assert_eq!(draft.relevance, expected);
        }

        let definition = plan
            .invocations()
            .iter()
            .find(|invocation| invocation.provider() == EvidenceProvider::Definition)
            .expect("definition invocation");
        assert_eq!(
            shape_provider_observation(
                &request,
                definition,
                CompletenessState::Complete,
                EvidenceProviderObservation {
                    kind: EvidenceProviderObservationKind::Primary,
                    symbol_id: None,
                    identity: "unscored-definition".to_owned(),
                    observed_score: None,
                    observed_relevance: None,
                    estimated_tokens: 1,
                    source_bytes: 0,
                    source_refs: Vec::new(),
                },
            ),
            Err(ContextEvidenceCollectionError::InvalidProviderResponse)
        );
    }

    #[test]
    fn agent_projects_change_transport_records_by_invocation_role() {
        let request = request("fix crash");
        let mut invocation = ContextEvidenceProviderRegistry
            .plan(&request)
            .expect("provider plan")
            .invocations()
            .iter()
            .find(|invocation| {
                invocation.provider() == EvidenceProvider::ChangeImpact
                    && invocation.role() == EvidenceRole::Risk
            })
            .expect("risk invocation")
            .clone();
        assert!(observation_applies(
            &invocation,
            EvidenceProviderObservationKind::ChangeRiskSummary
        ));
        assert!(!observation_applies(
            &invocation,
            EvidenceProviderObservationKind::Primary
        ));

        invocation.role = EvidenceRole::Change;
        assert!(observation_applies(
            &invocation,
            EvidenceProviderObservationKind::Primary
        ));
        assert!(!observation_applies(
            &invocation,
            EvidenceProviderObservationKind::ChangeRiskSummary
        ));
    }

    #[test]
    fn semantic_aliases_and_overlapping_ranges_deduplicate_deterministically() {
        let invocation = one_invocation_plan(&request("fix crash"))
            .invocations()
            .first()
            .expect("fixture invocation")
            .clone();
        let draft = |identity: &str,
                     symbol_id: Option<SymbolId>,
                     source_ref: SourceRef,
                     confidence: u16| {
            TypedEvidenceCandidate::from_draft(
                REPOSITORY,
                GENERATION,
                EvidenceCandidateDraft {
                    repository: REPOSITORY,
                    generation: GENERATION,
                    invocation: invocation.id().clone(),
                    provider: invocation.provider(),
                    role: invocation.role(),
                    provenance: EvidenceProvenance::Graph,
                    symbol_id,
                    identity: identity.to_owned(),
                    relevance: confidence,
                    confidence,
                    cost: BudgetCharge {
                        results: 1,
                        tokens: 16,
                        ..BudgetCharge::default()
                    },
                    source_refs: vec![source_ref],
                    dependencies: Vec::new(),
                },
            )
            .expect("candidate validates")
        };
        let symbol = SymbolId::from_bytes([8; 20]);
        let alias_low = draft(
            "qualified alias",
            Some(symbol),
            ranged_source_ref(REPOSITORY, GENERATION, 0, 8),
            800,
        );
        let alias_high = draft(
            "display alias",
            Some(symbol),
            ranged_source_ref(REPOSITORY, GENERATION, 40, 48),
            900,
        );
        let overlap = draft(
            "overlap",
            None,
            ranged_source_ref(REPOSITORY, GENERATION, 44, 60),
            850,
        );
        let adjacent = draft(
            "adjacent",
            None,
            ranged_source_ref(REPOSITORY, GENERATION, 60, 70),
            700,
        );

        let retained = deduplicate_candidates(vec![
            adjacent.clone(),
            alias_low,
            overlap,
            alias_high.clone(),
        ]);
        assert_eq!(retained.len(), 2);
        assert!(
            retained
                .iter()
                .any(|candidate| candidate.id() == alias_high.id())
        );
        assert!(
            retained
                .iter()
                .any(|candidate| candidate.id() == adjacent.id())
        );
    }

    #[tokio::test]
    async fn confidence_boundary_is_inclusive_and_low_confidence_is_truthful() {
        let mut public = ContextPackInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: REPOSITORY,
            }),
            generation: None,
            task: "fix crash".to_owned(),
            seeds: ContextSeedSelector {
                symbols: Some(vec![SymbolId::from_bytes([3; 20])]),
                paths: None,
                routes: None,
                tests: None,
                located: None,
                change: None,
                plan: None,
            },
            token_budget: 4_500,
            source_policy: None,
            sections: None,
            diversity: None,
            min_confidence: Some(700),
            response_profile: None,
            continuation: None,
            explain: None,
        };
        let request = CanonicalContextPackRequest::new(&public, REPOSITORY, GENERATION)
            .expect("request canonicalizes");
        let plan = one_invocation_plan(&request);
        let invocation = &plan.invocations()[0];
        let mut output = complete_output(invocation, "threshold");
        output.observations[0].observed_score = Some(700);
        let corpus = ContextEvidenceCollector
            .collect(
                &FakeEvidencePort::with_responses([Ok(output)]),
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("boundary collection succeeds");
        assert_eq!(corpus.candidates.len(), 1);
        assert_eq!(corpus.candidates[0].confidence(), 700);

        let mut below = complete_output(invocation, "below");
        below.observations[0].symbol_id = Some(SymbolId::from_bytes([9; 20]));
        below.observations[0].source_refs =
            vec![ranged_source_ref(REPOSITORY, GENERATION, 100, 120)];
        below.observations[0].observed_score = Some(699);
        let below_corpus = ContextEvidenceCollector
            .collect(
                &FakeEvidencePort::with_responses([Ok(below)]),
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("below-boundary collection succeeds");
        assert!(below_corpus.candidates.is_empty());
        assert!(below_corpus.omissions.iter().any(|omission| {
            omission.reason == EvidenceProviderOmissionReason::LowConfidence && omission.count == 1
        }));

        public.min_confidence = Some(701);
        assert!(
            CanonicalContextPackRequest::new(&public, REPOSITORY, GENERATION).is_ok(),
            "confidence threshold remains a canonical request dimension"
        );
    }

    #[test]
    fn candidate_rejects_wrong_identity_and_wrong_source_identity() {
        let invocation = ProviderInvocationId("ctxcall1-test".to_owned());
        let draft = EvidenceCandidateDraft {
            repository: RepositoryId::from_bytes([9; 16]),
            generation: GENERATION,
            invocation: invocation.clone(),
            provider: EvidenceProvider::Definition,
            role: EvidenceRole::Definition,
            provenance: EvidenceProvenance::Graph,
            symbol_id: None,
            identity: "candidate".to_owned(),
            relevance: 800,
            confidence: 800,
            cost: BudgetCharge {
                results: 1,
                tokens: 1,
                ..BudgetCharge::default()
            },
            source_refs: Vec::new(),
            dependencies: Vec::new(),
        };
        assert_eq!(
            TypedEvidenceCandidate::from_draft(REPOSITORY, GENERATION, draft),
            Err(EvidenceCandidateError::RepositoryMismatch)
        );

        let draft = EvidenceCandidateDraft {
            repository: REPOSITORY,
            generation: GENERATION,
            invocation,
            provider: EvidenceProvider::Definition,
            role: EvidenceRole::Definition,
            provenance: EvidenceProvenance::Graph,
            symbol_id: None,
            identity: "candidate".to_owned(),
            relevance: 800,
            confidence: 800,
            cost: BudgetCharge {
                results: 1,
                tokens: 1,
                ..BudgetCharge::default()
            },
            source_refs: vec![source_ref(REPOSITORY, GenerationId::from_bytes([8; 20]))],
            dependencies: Vec::new(),
        };
        assert_eq!(
            TypedEvidenceCandidate::from_draft(REPOSITORY, GENERATION, draft),
            Err(EvidenceCandidateError::SourceIdentityMismatch)
        );
    }

    #[tokio::test]
    async fn fresh_provider_output_is_charged_and_materialized() {
        let request = request("fix crash");
        let plan = one_invocation_plan(&request);
        let output = complete_output(&plan.invocations[0], "definition");
        let port = FakeEvidencePort::with_responses([Ok(output)]);

        let corpus = ContextEvidenceCollector
            .collect(
                &port,
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("fresh provider output is accepted");

        assert_eq!(corpus.candidates.len(), 1);
        assert!(corpus.omissions.is_empty());
        assert_eq!(corpus.usage.results, 1);
        assert_eq!(corpus.usage.tokens, 64);

        let planned = DefaultContextPackPlanner
            .plan_corpus(&request, &corpus, &NeverCancelled)
            .expect("typed corpus materializes under the remaining parent budget");
        assert_eq!(planned.data.items.len(), 1);
        assert_eq!(
            planned.data.items[0].symbol_id,
            Some(SymbolId::from_bytes([3; 20]))
        );
        assert_eq!(
            planned.data.items[0].trust,
            TrustClassification::UntrustedRepositoryData
        );
        assert!(!planned.data.role_coverage.complete());
        assert_eq!(planned.completeness.state, CompletenessState::Indeterminate);
    }

    #[tokio::test]
    async fn stale_and_corrupt_provider_outputs_fail_closed() {
        let request = request("fix crash");
        let plan = one_invocation_plan(&request);
        let mut stale = complete_output(&plan.invocations[0], "stale");
        stale.generation = GenerationId::from_bytes([9; 20]);
        let port = FakeEvidencePort::with_responses([Ok(stale)]);
        assert_eq!(
            ContextEvidenceCollector
                .collect(
                    &port,
                    &request,
                    &plan,
                    NeverCancelled,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(ContextEvidenceCollectionError::IdentityMismatch)
        );

        let mut corrupt = complete_output(&plan.invocations[0], "corrupt");
        corrupt.observations[0].source_refs =
            vec![source_ref(REPOSITORY, GenerationId::from_bytes([8; 20]))];
        let port = FakeEvidencePort::with_responses([Ok(corrupt)]);
        assert_eq!(
            ContextEvidenceCollector
                .collect(
                    &port,
                    &request,
                    &plan,
                    NeverCancelled,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(ContextEvidenceCollectionError::InvalidCandidate(
                EvidenceCandidateError::SourceIdentityMismatch
            ))
        );
    }

    #[tokio::test]
    async fn truncation_is_visible_and_downgrades_observed_confidence() {
        let request = request("fix crash");
        let plan = one_invocation_plan(&request);
        let mut output = complete_output(&plan.invocations[0], "partial");
        output.completeness = ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![LimitingResource::kind(LimitingResourceKind::Results)],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::SplitRequest],
        )
        .expect("valid truncated completeness");
        let port = FakeEvidencePort::with_responses([Ok(output)]);

        let corpus = ContextEvidenceCollector
            .collect(
                &port,
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("bounded partial evidence is accepted");

        assert_eq!(corpus.candidates[0].confidence(), 800);
        assert!(corpus.omissions.iter().any(|omission| {
            omission.reason == EvidenceProviderOmissionReason::Truncated
                && omission
                    .limiting_resources
                    .iter()
                    .any(|resource| resource.kind == LimitingResourceKind::Results)
        }));
        let planned = DefaultContextPackPlanner
            .plan_corpus(&request, &corpus, &NeverCancelled)
            .expect("partial corpus remains materializable");
        assert!(planned.truncated);
        assert_eq!(planned.completeness.state, CompletenessState::Indeterminate);
        assert!(
            planned
                .completeness
                .limiting_resources
                .iter()
                .any(|resource| resource.kind == LimitingResourceKind::Results)
        );
    }

    #[tokio::test]
    async fn unsupported_empty_and_unsafe_states_are_not_hidden() {
        let request = request("fix crash");
        let plan = one_invocation_plan(&request);
        let unsupported = ContextEvidencePortError {
            kind: ContextEvidencePortErrorKind::Unsupported,
            usage: BudgetCharge::default(),
        };
        let port = FakeEvidencePort::with_responses([Err(unsupported)]);
        let corpus = ContextEvidenceCollector
            .collect(
                &port,
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("unsupported provider becomes an omission");
        assert_eq!(
            corpus.omissions[0].reason,
            EvidenceProviderOmissionReason::Unsupported
        );

        let mut empty = complete_output(&plan.invocations[0], "unused");
        empty.observations.clear();
        empty.usage = BudgetCharge::default();
        let port = FakeEvidencePort::with_responses([Ok(empty)]);
        let corpus = ContextEvidenceCollector
            .collect(
                &port,
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("complete empty provider becomes an omission");
        assert_eq!(
            corpus.omissions[0].reason,
            EvidenceProviderOmissionReason::NoEvidence
        );

        let mut unsafe_output = complete_output(&plan.invocations[0], "unsafe");
        unsafe_output.completeness = ResultCompleteness::new(
            CompletenessState::Indeterminate,
            vec![LimitingResource::kind(LimitingResourceKind::Coverage)],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::RefreshCoverage],
        )
        .expect("valid indeterminate completeness");
        let port = FakeEvidencePort::with_responses([Ok(unsafe_output)]);
        assert_eq!(
            ContextEvidenceCollector
                .collect(
                    &port,
                    &request,
                    &plan,
                    NeverCancelled,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(ContextEvidenceCollectionError::UnsafeCompleteness)
        );
    }

    #[tokio::test]
    async fn cancellation_stops_before_provider_dispatch() {
        let request = request("fix crash");
        let plan = one_invocation_plan(&request);
        let port = FakeEvidencePort::with_responses(std::iter::empty());

        assert_eq!(
            ContextEvidenceCollector
                .collect(
                    &port,
                    &request,
                    &plan,
                    AlwaysCancelled,
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(ContextEvidenceCollectionError::Cancelled)
        );
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parent_budget_prevents_unreserved_provider_work() {
        let request = request("fix crash");
        let mut plan = one_invocation_plan(&request);
        plan.invocations[0].reservation.tokens =
            u64::from(request.token_budget()).saturating_add(1);
        let port = FakeEvidencePort::with_responses(std::iter::empty());

        let corpus = ContextEvidenceCollector
            .collect(
                &port,
                &request,
                &plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("unreservable provider becomes a bounded omission");

        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            corpus.omissions[0].reason,
            EvidenceProviderOmissionReason::Budget
        );
        assert_eq!(
            corpus.omissions[0].limiting_resources[0].kind,
            LimitingResourceKind::EstimatedTokens
        );
    }

    proptest! {
        #[test]
        fn provider_plan_is_invariant_to_seed_order_and_duplicates(
            bytes in prop::collection::vec(any::<[u8; 20]>(), 1..=8)
        ) {
            let symbols = bytes
                .iter()
                .copied()
                .map(SymbolId::from_bytes)
                .collect::<Vec<_>>();
            let mut reordered = symbols.iter().rev().copied().collect::<Vec<_>>();
            reordered.extend(symbols.iter().copied());
            let left = ContextEvidenceProviderRegistry
                .plan(&symbol_request(symbols))
                .expect("left plan");
            let right = ContextEvidenceProviderRegistry
                .plan(&symbol_request(reordered))
                .expect("right plan");
            prop_assert_eq!(left, right);
        }

        #[test]
        fn candidate_order_is_invariant_to_provider_return_order(
            scores in prop::collection::vec(700u16..=1_000, 1..=16)
        ) {
            let invocation = ProviderInvocationId("ctxcall1-property".to_owned());
            let build = |index: usize, score: u16| {
                TypedEvidenceCandidate::from_draft(
                    REPOSITORY,
                    GENERATION,
                    EvidenceCandidateDraft {
                        repository: REPOSITORY,
                        generation: GENERATION,
                        invocation: invocation.clone(),
                        provider: EvidenceProvider::Definition,
                        role: EvidenceRole::Definition,
                        provenance: EvidenceProvenance::Graph,
                        symbol_id: None,
                        identity: format!("candidate-{index}"),
                        relevance: score,
                        confidence: score,
                        cost: BudgetCharge {
                            results: 1,
                            tokens: 1,
                            ..BudgetCharge::default()
                        },
                        source_refs: Vec::new(),
                        dependencies: Vec::new(),
                    },
                )
                .expect("property candidate")
            };
            let mut left = scores
                .iter()
                .copied()
                .enumerate()
                .map(|(index, score)| build(index, score))
                .collect::<Vec<_>>();
            let mut right = left.iter().cloned().rev().collect::<Vec<_>>();
            left.sort_by(candidate_order);
            right.sort_by(candidate_order);
            prop_assert_eq!(
                left.iter().map(TypedEvidenceCandidate::id).collect::<Vec<_>>(),
                right.iter().map(TypedEvidenceCandidate::id).collect::<Vec<_>>()
            );
        }
    }
}
