//! Deterministic context-pack optimizer for task-specific evidence assembly.
//!
//! The optimizer accepts typed evidence candidates from bounded providers. The
//! complete planner currently shapes generation-pinned symbol definitions into
//! the public context contract. Selection is deterministic, deduplicated, and
//! constrained by one shared token ledger.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_ids::RepositoryId;
use rootlight_mcp_contract::{
    PublicError, RepositorySelector, SafeLabel, SchemaVersion, SourceFreeMessage,
    TrustClassification,
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    context::{
        ContextItem, ContextPackData, ContextPackId, ContextPackInput,
        ContextPackObjective as ContractContextPackObjective, ContextStructure,
        EvidenceRole as ContractEvidenceRole, MissingRequiredRoleReason, OmissionSummary,
        RoleCoverageEntry, RoleCoverageError, RoleCoverageStatus, RoleCoverageSummary,
        RoleRequirement, TokenAccounting, ToolSuggestion,
    },
    vertical::{GenerationSelector, ReadEnvelope, RequiredNullable, SymbolExplanation},
};

use crate::{
    context_evidence::{
        ContextEvidenceCollectionError, ContextEvidenceCollector, ContextEvidenceCorpus,
        ContextEvidencePort, ContextEvidenceProviderRegistry, EvidenceProviderOmission,
        EvidenceProviderOmissionReason, EvidenceProviderPlanError,
    },
    context_pack_request::{
        CanonicalContextPackRequest, CanonicalContextPackRequestError, normalize_task,
    },
    explain::context_pack_plan,
    policy::{BudgetCharge, BudgetLedger, CancellationSignal, ExecutionPolicyError},
    port::{
        AgentIdentityRequest, AgentPortError, AgentResolutionContext, AgentResolvedIdentity,
        AgentToolPort,
    },
};

/// Maximum evidence items in one context pack.
///
/// Matches the public `context.pack` item ceiling.
pub const MAX_PACK_ITEMS: usize = 200;

/// Maximum source snippet bytes per item.
pub const MAX_SNIPPET_BYTES: usize = 8_192;

/// Maximum omission summary entries.
pub const MAX_OMISSIONS: usize = 32;

/// Token budget hard ceiling for context packs.
///
/// Matches the public `context.pack` token budget ceiling.
pub const MAX_PACK_TOKENS: u32 = 20_000;

/// Token budget minimum for a useful pack.
///
/// Matches the public `context.pack` token budget minimum.
pub const MIN_PACK_TOKENS: u32 = 500;

/// Bounded wall-clock ceiling for context-pack identity and evidence reads.
///
/// This matches the common server-owned analytical deadline.
pub const CONTEXT_PACK_TIMEOUT_MS: u32 = 2_000;

/// Evidence role classification for pack items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceRole {
    /// Primary target definition.
    Definition,
    /// Implementation body or key logic.
    Implementation,
    /// Direct caller or consumer.
    Caller,
    /// Relevant test or test fixture.
    Test,
    /// Risk or uncertainty indicator.
    Risk,
    /// Architecture or module context.
    Architecture,
    /// Recent change or diff context.
    Change,
}

impl EvidenceRole {
    /// All roles in priority order for lexicographic optimization.
    pub const ALL: [Self; 7] = [
        Self::Definition,
        Self::Implementation,
        Self::Caller,
        Self::Test,
        Self::Risk,
        Self::Architecture,
        Self::Change,
    ];

    /// Priority weight for deterministic ranking (lower is higher priority).
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::Definition => 0,
            Self::Implementation => 1,
            Self::Caller => 2,
            Self::Test => 3,
            Self::Risk => 4,
            Self::Architecture => 5,
            Self::Change => 6,
        }
    }
}

/// Task objective for context pack assembly.
pub use crate::context_pack_request::ContextPackObjective as PackObjective;

impl PackObjective {
    /// Minimum required roles for this objective.
    ///
    /// The optimizer guarantees at least one item per required role when
    /// evidence exists and the budget allows.
    #[must_use]
    pub const fn required_roles(self) -> &'static [EvidenceRole] {
        match self {
            Self::BugFix => &[
                EvidenceRole::Definition,
                EvidenceRole::Implementation,
                EvidenceRole::Test,
            ],
            Self::Refactor => &[
                EvidenceRole::Definition,
                EvidenceRole::Caller,
                EvidenceRole::Test,
            ],
            Self::Explanation => &[EvidenceRole::Definition, EvidenceRole::Architecture],
            Self::Migration => &[
                EvidenceRole::Definition,
                EvidenceRole::Caller,
                EvidenceRole::Change,
            ],
            Self::Review => &[
                EvidenceRole::Change,
                EvidenceRole::Definition,
                EvidenceRole::Risk,
            ],
        }
    }
}

/// One scored evidence candidate for pack selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceCandidate {
    /// Stable symbol or file identity.
    pub identity: String,
    /// Evidence role.
    pub role: EvidenceRole,
    /// Relevance score, zero to one thousand.
    pub relevance: u16,
    /// Confidence in the evidence, zero to one thousand.
    pub confidence: u16,
    /// Estimated token cost of including this item.
    pub estimated_tokens: u32,
    /// Source file path for deduplication.
    pub source_path: String,
}

/// A selected evidence item in the final pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackItem {
    /// Zero-based position in deterministic output order.
    pub position: usize,
    /// The selected candidate.
    pub candidate: EvidenceCandidate,
}

/// An omitted evidence entry with continuation handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmissionEntry {
    /// Role of the omitted evidence.
    pub role: EvidenceRole,
    /// Number of items omitted for this role.
    pub count: usize,
    /// Estimated tokens that would be needed to include them.
    pub estimated_tokens: u32,
    /// Opaque continuation handle for follow-up requests.
    pub continuation_handle: String,
}

/// Result of context pack optimization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackResult {
    /// Selected items in deterministic order.
    pub items: Vec<PackItem>,
    /// Omitted evidence summary.
    pub omissions: Vec<OmissionEntry>,
    /// Total estimated tokens used.
    pub total_tokens: u32,
    /// Whether the pack hit the token budget before including all candidates.
    pub truncated: bool,
}

/// Errors returned during pack optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    /// The token budget is outside the valid range.
    #[error("token budget is outside the valid range")]
    InvalidBudget,
    /// No target symbols were provided.
    #[error("no target symbols provided")]
    NoTargets,
    /// Too many target symbols.
    #[error("too many target symbols")]
    TooManyTargets,
    /// Bounded planner allocation could not be reserved.
    #[error("context-pack planner memory is unavailable")]
    MemoryUnavailable,
}

/// Failure returned by the complete context-pack planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContextPackPlanningError {
    /// Evidence selection violated a context-pack invariant.
    #[error(transparent)]
    Pack(#[from] PackError),
    /// Request cancellation or a shared budget stopped planning.
    #[error(transparent)]
    Policy(#[from] ExecutionPolicyError),
    /// Planner completeness could not represent observed provider state.
    #[error("context-pack planner produced invalid completeness")]
    InvalidCompleteness,
    /// Objective-role coverage could not represent provider observations.
    #[error("context-pack planner produced invalid role coverage")]
    InvalidRoleCoverage,
}

/// Transport-neutral input for one complete context-pack planning pass.
#[derive(Debug)]
pub struct ContextPackPlanRequest<'a> {
    /// Canonical request pinned to one exact repository and generation.
    pub request: &'a CanonicalContextPackRequest,
    /// Generation-pinned symbol evidence returned by the injected provider.
    pub symbols: &'a [SymbolExplanation],
}

/// Context-pack data plus envelope-level truncation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedContextPack {
    /// Schema-compatible context-pack data.
    pub data: ContextPackData,
    /// Whether planning omitted evidence due to a bound.
    pub truncated: bool,
    /// Planner-owned completeness merged from evidence providers and selection.
    pub completeness: ResultCompleteness,
    /// Provider retrieval and output materialization charged to one ledger.
    pub usage: BudgetCharge,
}

#[derive(Debug, Clone)]
struct ContextCandidateMetadata {
    symbol_id: Option<rootlight_ids::SymbolId>,
    source_ref: Option<rootlight_ir::SourceRef>,
    trust: TrustClassification,
}

/// Failure returned by the typed multi-provider context-pack path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextEvidencePlanningError {
    /// Canonical provider selection exceeded a hard plan invariant.
    #[error(transparent)]
    ProviderPlan(#[from] EvidenceProviderPlanError),
    /// Provider collection returned unsafe evidence or exceeded policy.
    #[error(transparent)]
    Collection(#[from] ContextEvidenceCollectionError),
    /// Candidate optimization or materialization failed.
    #[error(transparent)]
    Planning(#[from] ContextPackPlanningError),
}

/// Planner boundary for complete context-pack shaping.
pub trait ContextPackPlanner<C>
where
    C: CancellationSignal,
{
    /// Plans and shapes a context pack from generation-pinned evidence.
    ///
    /// # Errors
    ///
    /// Returns [`ContextPackPlanningError`] when planning violates an invariant,
    /// exceeds its budget, or observes cancellation.
    fn plan(
        &self,
        request: ContextPackPlanRequest<'_>,
        cancellation: &C,
    ) -> Result<PlannedContextPack, ContextPackPlanningError>;
}

/// Deterministic production context-pack planner.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultContextPackPlanner;

impl<C> ContextPackPlanner<C> for DefaultContextPackPlanner
where
    C: CancellationSignal,
{
    fn plan(
        &self,
        request: ContextPackPlanRequest<'_>,
        cancellation: &C,
    ) -> Result<PlannedContextPack, ContextPackPlanningError> {
        checkpoint(cancellation)?;

        let mut metadata = BTreeMap::new();
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(request.symbols.len())
            .map_err(|_| PackError::MemoryUnavailable)?;
        for explanation in request.symbols {
            checkpoint(cancellation)?;
            metadata.insert(
                explanation.symbol_id.to_string(),
                ContextCandidateMetadata {
                    symbol_id: Some(explanation.symbol_id),
                    source_ref: Some(explanation.definition.clone()),
                    trust: explanation.trust,
                },
            );
            let signature_bytes = explanation.signature.as_ref().map_or(0, String::len);
            // Exact UTF-8 bytes are a conservative provider-neutral token
            // ceiling. Actual tokenizer counts remain benchmark evidence.
            let estimated_tokens =
                u32::try_from(signature_bytes + explanation.display_name.len()).unwrap_or(u32::MAX);
            candidates.push(EvidenceCandidate {
                identity: explanation.symbol_id.to_string(),
                role: EvidenceRole::Definition,
                relevance: explanation.confidence,
                confidence: explanation.confidence,
                estimated_tokens,
                source_path: explanation.definition.span().file().to_string(),
            });
        }

        let pack = optimize_pack(
            request.request.objective(),
            &mut candidates,
            u32::from(request.request.token_budget()),
        )?;
        checkpoint(cancellation)?;

        let mut budget = BudgetLedger::with_token_limit(u64::from(request.request.token_budget()));
        budget.charge(BudgetCharge {
            results: u64::try_from(pack.items.len()).unwrap_or(u64::MAX),
            tokens: u64::from(pack.total_tokens),
            ..BudgetCharge::default()
        })?;

        let selected_roles = pack
            .items
            .iter()
            .map(|item| item.candidate.role)
            .collect::<Vec<_>>();
        let observed_roles = candidates
            .iter()
            .map(|candidate| candidate.role)
            .collect::<Vec<_>>();
        let role_coverage = evaluate_role_coverage(
            request.request.objective(),
            &selected_roles,
            &observed_roles,
            &[],
        )
        .map_err(|_| ContextPackPlanningError::InvalidRoleCoverage)?;
        let completeness = planner_completeness(pack.truncated)?
            .merge(&role_coverage_completeness(&role_coverage)?)
            .map_err(|_| ContextPackPlanningError::InvalidCompleteness)?;
        let mut data = context_pack_data(request.request, &pack, &metadata, role_coverage.clone());
        append_role_followups(&mut data.followups, &role_coverage);
        Ok(PlannedContextPack {
            data,
            truncated: pack.truncated,
            completeness,
            usage: budget.consumed(),
        })
    }
}

impl DefaultContextPackPlanner {
    /// Executes the deterministic provider registry and plans a typed evidence
    /// corpus without invoking public MCP transport recursively.
    ///
    /// # Errors
    ///
    /// Returns [`ContextEvidencePlanningError`] when provider selection,
    /// collection, validation, accounting, or optimization fails.
    pub async fn collect_and_plan<P, C>(
        &self,
        port: &P,
        request: &CanonicalContextPackRequest,
        cancellation: C,
        deadline: Instant,
    ) -> Result<PlannedContextPack, ContextEvidencePlanningError>
    where
        P: ContextEvidencePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        let provider_plan = ContextEvidenceProviderRegistry.plan(request)?;
        let corpus = ContextEvidenceCollector
            .collect(
                port,
                request,
                &provider_plan,
                cancellation.clone(),
                deadline,
            )
            .await?;
        self.plan_corpus(request, &corpus, &cancellation)
            .map_err(Into::into)
    }

    /// Optimizes one already validated typed provider corpus.
    ///
    /// Provider usage is retained in the same parent ledger before output
    /// materialization is charged, so the final pack cannot exceed the request
    /// token ceiling after retrieval work has already consumed capacity.
    ///
    /// # Errors
    ///
    /// Returns [`ContextPackPlanningError`] when cancellation, the remaining
    /// shared budget, or pack selection prevents safe materialization.
    pub fn plan_corpus<C>(
        &self,
        request: &CanonicalContextPackRequest,
        corpus: &ContextEvidenceCorpus,
        cancellation: &C,
    ) -> Result<PlannedContextPack, ContextPackPlanningError>
    where
        C: CancellationSignal,
    {
        checkpoint(cancellation)?;
        let mut metadata = BTreeMap::new();
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(corpus.candidates.len())
            .map_err(|_| PackError::MemoryUnavailable)?;
        for candidate in &corpus.candidates {
            checkpoint(cancellation)?;
            let identity = candidate.id().as_str().to_owned();
            let source_ref = candidate.source_refs().first().cloned();
            let source_path = source_ref.as_ref().map_or_else(
                || candidate.dedup_key().as_str().to_owned(),
                |source| source.span().file().to_string(),
            );
            metadata.insert(
                identity.clone(),
                ContextCandidateMetadata {
                    symbol_id: candidate.symbol_id(),
                    source_ref,
                    trust: candidate.trust(),
                },
            );
            candidates.push(EvidenceCandidate {
                identity,
                role: candidate.role(),
                relevance: candidate.relevance(),
                confidence: candidate.confidence(),
                estimated_tokens: u32::try_from(candidate.cost().tokens).unwrap_or(u32::MAX),
                source_path,
            });
        }

        let mut budget = corpus.budget().clone();
        let available_tokens = u32::try_from(budget.remaining().tokens).unwrap_or(u32::MAX);
        let pack = optimize_admitted_pack(request.objective(), &mut candidates, available_tokens)?;
        budget.charge(BudgetCharge {
            results: u64::try_from(pack.items.len()).unwrap_or(u64::MAX),
            tokens: u64::from(pack.total_tokens),
            ..BudgetCharge::default()
        })?;
        checkpoint(cancellation)?;

        let selected_roles = pack
            .items
            .iter()
            .map(|item| item.candidate.role)
            .collect::<Vec<_>>();
        let observed_roles = corpus
            .candidates
            .iter()
            .map(|candidate| candidate.role())
            .collect::<Vec<_>>();
        let role_coverage = evaluate_role_coverage(
            request.objective(),
            &selected_roles,
            &observed_roles,
            &corpus.omissions,
        )
        .map_err(|_| ContextPackPlanningError::InvalidRoleCoverage)?;
        let mut data = context_pack_data(request, &pack, &metadata, role_coverage.clone());
        append_provider_omissions(&mut data.omitted, corpus);
        append_role_followups(&mut data.followups, &role_coverage);
        let completeness = corpus_completeness(corpus, pack.truncated, &role_coverage)?;
        let truncated = completeness.state == CompletenessState::Truncated
            || completeness.limiting_resources.iter().any(|resource| {
                !matches!(
                    resource.kind,
                    LimitingResourceKind::Capability | LimitingResourceKind::Coverage
                )
            });
        Ok(PlannedContextPack {
            data,
            truncated,
            completeness,
            usage: budget.consumed(),
        })
    }
}

/// Failure returned by complete context-pack orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextPackServiceError {
    /// A named public input field is not implemented.
    UnsupportedField(&'static str),
    /// No supported seed supplied a symbol identity.
    EmptySeeds,
    /// A checked child call failed.
    Public(Box<PublicError>),
    /// Cooperative cancellation won.
    Cancelled,
    /// The bounded orchestration deadline elapsed.
    DeadlineExceeded,
    /// A child response violated the pinned identity or typed contract.
    InvalidResponse,
    /// The adapter or planner failed internally.
    Unavailable,
}

/// Complete transport-neutral service for `context.pack`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextPackService;

impl ContextPackService {
    /// Resolves identity and evidence, then shapes the complete public envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ContextPackServiceError`] when request admission, identity
    /// resolution, evidence retrieval, or deterministic planning fails.
    pub async fn execute<P, C>(
        &self,
        port: Arc<P>,
        input: ContextPackInput,
        repository: RepositoryId,
        cancellation: C,
    ) -> Result<ReadEnvelope<ContextPackData>, ContextPackServiceError>
    where
        P: AgentToolPort<C> + ContextEvidencePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        validate_supported_fields(&input)?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(u64::from(CONTEXT_PACK_TIMEOUT_MS)))
            .ok_or(ContextPackServiceError::Unavailable)?;
        context_service_checkpoint(&cancellation, deadline)?;
        let identity = port
            .resolve_identity(
                AgentIdentityRequest::new(input.repository.clone(), input.generation.clone()),
                AgentResolutionContext::new(cancellation.clone(), deadline),
            )
            .await
            .map_err(map_port_error)?;
        context_service_checkpoint(&cancellation, deadline)?;

        Self::execute_admitted_with_identity(
            port,
            input,
            repository,
            identity,
            cancellation,
            deadline,
        )
        .await
    }

    /// Resolves evidence under a repository and generation identity pinned by
    /// the caller, without performing another identity lookup.
    ///
    /// # Errors
    ///
    /// Returns [`ContextPackServiceError`] when request admission, pinned
    /// identity validation, evidence retrieval, or deterministic planning
    /// fails.
    pub async fn execute_with_identity<P, C>(
        &self,
        port: Arc<P>,
        input: ContextPackInput,
        repository: RepositoryId,
        identity: AgentResolvedIdentity,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ReadEnvelope<ContextPackData>, ContextPackServiceError>
    where
        P: AgentToolPort<C> + ContextEvidencePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        validate_supported_fields(&input)?;
        context_service_checkpoint(&cancellation, deadline)?;

        Self::execute_admitted_with_identity(
            port,
            input,
            repository,
            identity,
            cancellation,
            deadline,
        )
        .await
    }

    async fn execute_admitted_with_identity<P, C>(
        port: Arc<P>,
        input: ContextPackInput,
        repository: RepositoryId,
        identity: AgentResolvedIdentity,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ReadEnvelope<ContextPackData>, ContextPackServiceError>
    where
        P: AgentToolPort<C> + ContextEvidencePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        if identity.repository.repository_id != repository {
            return Err(ContextPackServiceError::InvalidResponse);
        }
        if matches!(
            &input.repository,
            RepositorySelector::ById(expected)
                if expected.repository_id != identity.repository.repository_id
        ) {
            return Err(ContextPackServiceError::InvalidResponse);
        }
        if matches!(
            input.generation.as_ref(),
            Some(GenerationSelector::Explicit(expected))
                if identity.generation.generation_id != *expected
        ) {
            return Err(ContextPackServiceError::InvalidResponse);
        }
        let canonical = CanonicalContextPackRequest::new(
            &input,
            identity.repository.repository_id,
            identity.generation.generation_id,
        )
        .map_err(map_canonical_request_error)?;
        if input.explain == Some(true) {
            let explanation = context_pack_plan(&canonical);
            let role_coverage = evaluate_role_coverage(canonical.objective(), &[], &[], &[])
                .map_err(|_| ContextPackServiceError::InvalidResponse)?;
            let completeness = role_coverage_completeness(&role_coverage)
                .map_err(|_| ContextPackServiceError::InvalidResponse)?;
            let mut followups = Vec::new();
            append_role_followups(&mut followups, &role_coverage);
            let data = ContextPackData {
                pack_id: context_pack_id(&canonical),
                request_digest: canonical.request_digest(),
                planner_version: rootlight_mcp_contract::context::PLANNER_VERSION,
                items: Vec::new(),
                role_coverage,
                structure: ContextStructure {
                    reading_order: Vec::new(),
                    dependencies: Vec::new(),
                },
                omitted: Vec::new(),
                followups,
                token_accounting: TokenAccounting {
                    estimated_total: 0,
                    by_section: BTreeMap::new(),
                },
                explanation: Some(explanation),
            };
            return Ok(ReadEnvelope {
                schema_version: SchemaVersion::V1_0,
                repository: identity.repository,
                generation: identity.generation,
                coverage: identity.coverage,
                data,
                truncated: false,
                completeness,
                next_cursor: RequiredNullable(None),
                usage: empty_usage("context-pack-explain"),
                warnings: identity.warnings,
                trust: TrustClassification::UntrustedRepositoryData,
            });
        }

        let planned = DefaultContextPackPlanner
            .collect_and_plan(port.as_ref(), &canonical, cancellation.clone(), deadline)
            .await
            .map_err(map_evidence_planning_error)?;
        context_service_checkpoint(&cancellation, deadline)?;
        let usage = usage_summary(planned.usage, "context-pack");
        Ok(ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: identity.repository,
            generation: identity.generation,
            coverage: identity.coverage,
            data: planned.data,
            truncated: planned.truncated,
            completeness: planned.completeness,
            next_cursor: RequiredNullable(None),
            usage,
            warnings: identity.warnings,
            trust: TrustClassification::UntrustedRepositoryData,
        })
    }
}

#[cfg(test)]
fn context_pack_completeness(
    child: &ResultCompleteness,
    planner: &ResultCompleteness,
) -> Result<ResultCompleteness, ContextPackServiceError> {
    child
        .merge(planner)
        .map_err(|_| ContextPackServiceError::InvalidResponse)
}

fn planner_completeness(truncated: bool) -> Result<ResultCompleteness, ContextPackPlanningError> {
    if !truncated {
        return Ok(ResultCompleteness::complete());
    }
    ResultCompleteness::new(
        CompletenessState::Truncated,
        vec![LimitingResource::kind(
            LimitingResourceKind::EstimatedTokens,
        )],
        ContinuationAvailability::Unavailable,
        vec![ContinuationGuidance::SplitRequest],
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
}

fn corpus_completeness(
    corpus: &ContextEvidenceCorpus,
    pack_truncated: bool,
    role_coverage: &RoleCoverageSummary,
) -> Result<ResultCompleteness, ContextPackPlanningError> {
    let mut state = if pack_truncated {
        CompletenessState::Truncated
    } else {
        CompletenessState::Complete
    };
    let mut resources = if pack_truncated {
        vec![LimitingResource::kind(
            LimitingResourceKind::EstimatedTokens,
        )]
    } else {
        Vec::new()
    };
    let mut guidance = if pack_truncated {
        vec![ContinuationGuidance::SplitRequest]
    } else {
        Vec::new()
    };

    for omission in &corpus.omissions {
        match omission.reason {
            EvidenceProviderOmissionReason::NoEvidence
            | EvidenceProviderOmissionReason::LowConfidence => {}
            EvidenceProviderOmissionReason::Truncated | EvidenceProviderOmissionReason::Budget => {
                state = state.max(CompletenessState::Truncated);
                resources.extend(omission.limiting_resources.iter().copied());
                guidance.push(ContinuationGuidance::SplitRequest);
            }
            EvidenceProviderOmissionReason::Unsupported => {
                state = state.max(CompletenessState::UnsupportedPartial);
                resources.extend(omission.limiting_resources.iter().copied());
                guidance.push(ContinuationGuidance::UnsupportedNoContinuation);
            }
            EvidenceProviderOmissionReason::Unavailable => {
                state = CompletenessState::Indeterminate;
                resources.extend(omission.limiting_resources.iter().copied());
                guidance.push(ContinuationGuidance::RefreshCoverage);
            }
        }
    }
    let role_completeness = role_coverage_completeness(role_coverage)?;
    state = state.max(role_completeness.state);
    resources.extend(role_completeness.limiting_resources);
    guidance.extend(role_completeness.guidance);
    if state == CompletenessState::Complete {
        return Ok(ResultCompleteness::complete());
    }
    resources.sort_unstable();
    resources.dedup_by_key(|resource| resource.kind);
    guidance.sort_unstable();
    guidance.dedup();
    ResultCompleteness::new(
        state,
        resources,
        ContinuationAvailability::Unavailable,
        guidance,
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
}

fn role_coverage_completeness(
    coverage: &RoleCoverageSummary,
) -> Result<ResultCompleteness, ContextPackPlanningError> {
    if coverage.complete() {
        return Ok(ResultCompleteness::complete());
    }
    let mut state = CompletenessState::Complete;
    let mut resources = Vec::new();
    let mut guidance = Vec::new();
    for entry in coverage
        .roles()
        .iter()
        .filter(|entry| entry.status == RoleCoverageStatus::MissingRequired)
    {
        match entry.missing_reason {
            Some(MissingRequiredRoleReason::Budget) => {
                state = state.max(CompletenessState::Truncated);
                resources.push(LimitingResource::kind(
                    LimitingResourceKind::EstimatedTokens,
                ));
                guidance.push(ContinuationGuidance::IncreaseBudgetWithinLimit);
            }
            Some(MissingRequiredRoleReason::Truncated) => {
                state = state.max(CompletenessState::Truncated);
                resources.push(LimitingResource::kind(LimitingResourceKind::Results));
                guidance.push(ContinuationGuidance::SplitRequest);
            }
            Some(MissingRequiredRoleReason::Unsupported) => {
                state = state.max(CompletenessState::UnsupportedPartial);
                resources.push(LimitingResource::kind(LimitingResourceKind::Capability));
                guidance.push(ContinuationGuidance::UnsupportedNoContinuation);
            }
            Some(MissingRequiredRoleReason::NoEvidence)
            | Some(MissingRequiredRoleReason::LowConfidence) => {
                state = state.max(CompletenessState::UnsupportedPartial);
                resources.push(LimitingResource::kind(LimitingResourceKind::Coverage));
                guidance.push(ContinuationGuidance::RefreshCoverage);
            }
            Some(MissingRequiredRoleReason::Unavailable)
            | Some(MissingRequiredRoleReason::NotSearched)
            | None => {
                state = CompletenessState::Indeterminate;
                resources.push(LimitingResource::kind(LimitingResourceKind::Coverage));
                guidance.push(ContinuationGuidance::RefreshCoverage);
            }
        }
    }
    resources.sort_unstable();
    resources.dedup_by_key(|resource| resource.kind);
    guidance.sort_unstable();
    guidance.dedup();
    ResultCompleteness::new(
        state,
        resources,
        ContinuationAvailability::Unavailable,
        guidance,
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
}

fn context_service_checkpoint<C>(
    cancellation: &C,
    deadline: Instant,
) -> Result<(), ContextPackServiceError>
where
    C: CancellationSignal,
{
    if cancellation.is_cancelled() {
        Err(ContextPackServiceError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ContextPackServiceError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn validate_supported_fields(input: &ContextPackInput) -> Result<(), ContextPackServiceError> {
    if input.continuation.is_some() {
        return Err(ContextPackServiceError::UnsupportedField("continuation"));
    }
    Ok(())
}

fn map_canonical_request_error(error: CanonicalContextPackRequestError) -> ContextPackServiceError {
    match error {
        CanonicalContextPackRequestError::EmptySeeds => ContextPackServiceError::EmptySeeds,
        CanonicalContextPackRequestError::UnsupportedField(field) => {
            ContextPackServiceError::UnsupportedField(field)
        }
        CanonicalContextPackRequestError::InvalidField(field) => {
            ContextPackServiceError::UnsupportedField(field)
        }
        CanonicalContextPackRequestError::TooManySeeds => {
            ContextPackServiceError::UnsupportedField("seeds")
        }
        CanonicalContextPackRequestError::EmptyTask => {
            ContextPackServiceError::UnsupportedField("task")
        }
        CanonicalContextPackRequestError::RepositoryMismatch
        | CanonicalContextPackRequestError::GenerationMismatch => {
            ContextPackServiceError::InvalidResponse
        }
    }
}

fn map_port_error(error: AgentPortError) -> ContextPackServiceError {
    let (error, _) = error.into_parts();
    match error {
        AgentPortError::Public(error) => ContextPackServiceError::Public(error),
        AgentPortError::Cancelled => ContextPackServiceError::Cancelled,
        AgentPortError::DeadlineExceeded => ContextPackServiceError::DeadlineExceeded,
        AgentPortError::LocalDeadlineExceeded => ContextPackServiceError::InvalidResponse,
        AgentPortError::InvalidResponse => ContextPackServiceError::InvalidResponse,
        AgentPortError::Unavailable => ContextPackServiceError::Unavailable,
        AgentPortError::Measured { .. } => ContextPackServiceError::InvalidResponse,
    }
}

fn map_evidence_planning_error(error: ContextEvidencePlanningError) -> ContextPackServiceError {
    match error {
        ContextEvidencePlanningError::ProviderPlan(_) => ContextPackServiceError::InvalidResponse,
        ContextEvidencePlanningError::Collection(error) => match error {
            ContextEvidenceCollectionError::Cancelled => ContextPackServiceError::Cancelled,
            ContextEvidenceCollectionError::DeadlineExceeded => {
                ContextPackServiceError::DeadlineExceeded
            }
            ContextEvidenceCollectionError::IdentityMismatch
            | ContextEvidenceCollectionError::UnsafeCompleteness
            | ContextEvidenceCollectionError::InvalidCandidate(_)
            | ContextEvidenceCollectionError::InvalidProviderResponse => {
                ContextPackServiceError::InvalidResponse
            }
            ContextEvidenceCollectionError::Policy(ExecutionPolicyError::Cancelled) => {
                ContextPackServiceError::Cancelled
            }
            ContextEvidenceCollectionError::Policy(ExecutionPolicyError::BudgetExceeded {
                ..
            }) => ContextPackServiceError::Unavailable,
        },
        ContextEvidencePlanningError::Planning(error) => match error {
            ContextPackPlanningError::Policy(ExecutionPolicyError::Cancelled) => {
                ContextPackServiceError::Cancelled
            }
            ContextPackPlanningError::Pack(_)
            | ContextPackPlanningError::Policy(ExecutionPolicyError::BudgetExceeded { .. }) => {
                ContextPackServiceError::Unavailable
            }
            ContextPackPlanningError::InvalidCompleteness => {
                ContextPackServiceError::InvalidResponse
            }
            ContextPackPlanningError::InvalidRoleCoverage => {
                ContextPackServiceError::InvalidResponse
            }
        },
    }
}

fn usage_summary(
    usage: BudgetCharge,
    trace_id: &str,
) -> rootlight_mcp_contract::vertical::UsageSummary {
    rootlight_mcp_contract::vertical::UsageSummary {
        rows: usage.rows,
        edges: usage.traversal_facts,
        source_bytes: usage.source_bytes,
        json_bytes: usage.json_bytes,
        estimated_tokens: usage.tokens,
        wall_time_ms: usage.time_ms,
        cache_status: rootlight_mcp_contract::vertical::CacheStatus::NotApplicable,
        trace_id: trace_id.to_owned(),
    }
}

fn empty_usage(trace_id: &str) -> rootlight_mcp_contract::vertical::UsageSummary {
    usage_summary(BudgetCharge::default(), trace_id)
}

/// Optimizes a context pack from scored candidates under a token budget.
///
/// The optimization objective is lexicographic:
/// 1. Include required target definitions and direct evidence.
/// 2. Satisfy minimum representation for objective-relevant roles.
/// 3. Maximize relevance and evidence confidence.
/// 4. Diversify files and components to avoid redundant snippets.
/// 5. Minimize tokens and repeated source.
/// 6. Preserve deterministic ordering.
///
/// # Errors
///
/// Returns [PackError] when the budget or targets are invalid.
pub fn optimize_pack(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    token_budget: u32,
) -> Result<PackResult, PackError> {
    if !(MIN_PACK_TOKENS..=MAX_PACK_TOKENS).contains(&token_budget) {
        return Err(PackError::InvalidBudget);
    }
    optimize_admitted_pack(objective, candidates, token_budget)
}

fn optimize_admitted_pack(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    token_budget: u32,
) -> Result<PackResult, PackError> {
    if token_budget > MAX_PACK_TOKENS {
        return Err(PackError::InvalidBudget);
    }
    if candidates.is_empty() {
        return Err(PackError::NoTargets);
    }

    // Sort candidates by deterministic ranking:
    // 1. Role priority (required roles first)
    // 2. Relevance descending
    // 3. Confidence descending
    // 4. Identity ascending (stable tie-break)
    let required = objective.required_roles();
    // Deterministic global ranking: required roles first, then role priority,
    // relevance, confidence, and a stable identity tie-break. Within one role
    // this orders candidates best-first, so the first unreserved candidate of
    // a role is also its best representative.
    candidates.sort_by(|a, b| {
        let a_required = required.contains(&a.role);
        let b_required = required.contains(&b.role);
        b_required
            .cmp(&a_required)
            .then_with(|| a.role.priority().cmp(&b.role.priority()))
            .then_with(|| b.relevance.cmp(&a.relevance))
            .then_with(|| b.confidence.cmp(&a.confidence))
            .then_with(|| a.identity.cmp(&b.identity))
    });

    // Minimum representation: reserve one fitting candidate per required role
    // before the remaining budget is handed to greedy filling. Without this
    // reservation a run of high-relevance items from the first required role
    // can consume the whole budget and starve the other required roles even
    // though one item per role would have fit. Candidates are visited in ranked
    // order, so roles are reserved in role-priority order and each role keeps
    // its best candidate that still fits.
    let mut reserved = vec![false; candidates.len()];
    let mut reserved_tokens = 0u32;
    let mut reserved_paths: Vec<&str> = Vec::new();
    let mut represented: Vec<EvidenceRole> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if represented.len() == required.len() {
            break;
        }
        if !required.contains(&candidate.role) || represented.contains(&candidate.role) {
            continue;
        }
        if reserved_tokens.saturating_add(candidate.estimated_tokens) <= token_budget
            && path_count(&reserved_paths, &candidate.source_path) < 2
        {
            reserved[index] = true;
            reserved_tokens = reserved_tokens.saturating_add(candidate.estimated_tokens);
            reserved_paths.push(candidate.source_path.as_str());
            represented.push(candidate.role);
        }
    }

    // Emit in deterministic ranked order. Reserved candidates are always
    // included so every represented required role stays present; the remaining
    // budget is filled greedily under the per-source diversity bound. Greedy
    // spending is capped at the budget left over after reservation so greedy
    // items can never displace a reserved required-role representative.
    let greedy_budget = token_budget.saturating_sub(reserved_tokens);
    let mut greedy_spent = 0u32;
    let mut items: Vec<PackItem> = Vec::new();
    let mut omissions = Vec::new();
    let mut total_tokens = 0u32;
    let mut truncated = false;
    let mut seen_paths: Vec<&str> = Vec::new();

    for (index, candidate) in candidates.iter().enumerate().take(MAX_PACK_ITEMS) {
        if !reserved[index] {
            // Deduplication: skip items from the same source path if we already
            // have two items from it (diversity constraint).
            if path_count(&seen_paths, &candidate.source_path) >= 2
                || greedy_spent.saturating_add(candidate.estimated_tokens) > greedy_budget
            {
                record_omission(&mut omissions, candidate);
                truncated = true;
                continue;
            }
            greedy_spent = greedy_spent.saturating_add(candidate.estimated_tokens);
        }

        let position = items.len();
        items.push(PackItem {
            position,
            candidate: candidate.clone(),
        });
        total_tokens = total_tokens.saturating_add(candidate.estimated_tokens);
        seen_paths.push(candidate.source_path.as_str());
    }

    if candidates.len() > MAX_PACK_ITEMS {
        truncated = true;
    }

    // Trim omissions to bounded count
    omissions.truncate(MAX_OMISSIONS);

    Ok(PackResult {
        items,
        omissions,
        total_tokens,
        truncated,
    })
}

/// Counts how many selected items came from one source path.
fn path_count(seen_paths: &[&str], path: &str) -> usize {
    seen_paths.iter().filter(|p| **p == path).count()
}

fn record_omission(omissions: &mut Vec<OmissionEntry>, candidate: &EvidenceCandidate) {
    if let Some(existing) = omissions.iter_mut().find(|o| o.role == candidate.role) {
        existing.count += 1;
        existing.estimated_tokens = existing
            .estimated_tokens
            .saturating_add(candidate.estimated_tokens);
    } else if omissions.len() < MAX_OMISSIONS {
        omissions.push(OmissionEntry {
            role: candidate.role,
            count: 1,
            estimated_tokens: candidate.estimated_tokens,
            continuation_handle: format!("pack-cont-{}", candidate.role.priority()),
        });
    }
}

/// Classifies source-free task guidance into the planner's objective set.
#[must_use]
pub fn objective_for_task(task: &str) -> PackObjective {
    PackObjective::from_normalized_task(&normalize_task(task))
}

/// Derives a deterministic identity for one generation-pinned context pack.
#[must_use]
pub fn context_pack_id(request: &CanonicalContextPackRequest) -> ContextPackId {
    request.pack_id()
}

/// Evaluates required-role truth independently of response presentation.
///
/// Selected and observed roles may contain duplicates; counts are retained in
/// the public summary. Provider omissions supply the observed reason for a
/// required role that has no selected candidate.
///
/// # Errors
///
/// Returns [`RoleCoverageError`] only if the derived entries violate the
/// versioned public role policy.
pub fn evaluate_role_coverage(
    objective: PackObjective,
    selected_roles: &[EvidenceRole],
    observed_roles: &[EvidenceRole],
    omissions: &[EvidenceProviderOmission],
) -> Result<RoleCoverageSummary, RoleCoverageError> {
    const ALL_ROLES: [EvidenceRole; 7] = [
        EvidenceRole::Definition,
        EvidenceRole::Implementation,
        EvidenceRole::Caller,
        EvidenceRole::Test,
        EvidenceRole::Risk,
        EvidenceRole::Architecture,
        EvidenceRole::Change,
    ];
    let mut entries = Vec::with_capacity(ALL_ROLES.len());
    for role in ALL_ROLES {
        let selected_items = u16::try_from(
            selected_roles
                .iter()
                .filter(|selected| **selected == role)
                .count(),
        )
        .unwrap_or(u16::MAX);
        let observed_candidates = u32::try_from(
            observed_roles
                .iter()
                .filter(|observed| **observed == role)
                .count(),
        )
        .unwrap_or(u32::MAX);
        let requirement = if objective.required_roles().contains(&role) {
            RoleRequirement::Required
        } else {
            RoleRequirement::Optional
        };
        let (status, missing_reason) = if selected_items > 0 {
            (RoleCoverageStatus::Satisfied, None)
        } else if requirement == RoleRequirement::Optional {
            (RoleCoverageStatus::OptionalAbsent, None)
        } else {
            (
                RoleCoverageStatus::MissingRequired,
                Some(missing_required_role_reason(
                    role,
                    observed_candidates,
                    omissions,
                )),
            )
        };
        entries.push(RoleCoverageEntry {
            role: contract_role(role),
            requirement,
            status,
            observed_candidates,
            selected_items,
            missing_reason,
        });
    }
    RoleCoverageSummary::new(contract_objective(objective), entries)
}

fn missing_required_role_reason(
    role: EvidenceRole,
    observed_candidates: u32,
    omissions: &[EvidenceProviderOmission],
) -> MissingRequiredRoleReason {
    if observed_candidates > 0 {
        return MissingRequiredRoleReason::Budget;
    }
    omissions
        .iter()
        .filter(|omission| omission.role == role)
        .map(|omission| match omission.reason {
            EvidenceProviderOmissionReason::Budget => MissingRequiredRoleReason::Budget,
            EvidenceProviderOmissionReason::Truncated => MissingRequiredRoleReason::Truncated,
            EvidenceProviderOmissionReason::Unavailable => MissingRequiredRoleReason::Unavailable,
            EvidenceProviderOmissionReason::Unsupported => MissingRequiredRoleReason::Unsupported,
            EvidenceProviderOmissionReason::LowConfidence => {
                MissingRequiredRoleReason::LowConfidence
            }
            EvidenceProviderOmissionReason::NoEvidence => MissingRequiredRoleReason::NoEvidence,
        })
        .max_by_key(|reason| missing_reason_priority(*reason))
        .unwrap_or(MissingRequiredRoleReason::NotSearched)
}

const fn missing_reason_priority(reason: MissingRequiredRoleReason) -> u8 {
    match reason {
        MissingRequiredRoleReason::NotSearched => 0,
        MissingRequiredRoleReason::NoEvidence => 1,
        MissingRequiredRoleReason::LowConfidence => 2,
        MissingRequiredRoleReason::Unsupported => 3,
        MissingRequiredRoleReason::Unavailable => 4,
        MissingRequiredRoleReason::Truncated => 5,
        MissingRequiredRoleReason::Budget => 6,
    }
}

fn context_pack_data(
    request: &CanonicalContextPackRequest,
    pack: &PackResult,
    metadata: &BTreeMap<String, ContextCandidateMetadata>,
    role_coverage: RoleCoverageSummary,
) -> ContextPackData {
    let items = pack
        .items
        .iter()
        .map(|item| {
            let metadata = metadata.get(&item.candidate.identity);
            ContextItem {
                role: contract_role(item.candidate.role),
                symbol_id: metadata
                    .and_then(|value| value.symbol_id)
                    .or_else(|| item.candidate.identity.parse().ok()),
                source_ref: metadata.and_then(|value| value.source_ref.clone()),
                score: item.candidate.relevance,
                tokens: item.candidate.estimated_tokens,
                trust: metadata.map_or(TrustClassification::UntrustedRepositoryData, |value| {
                    value.trust
                }),
                snippet: None,
            }
        })
        .collect();

    let reading_order = pack
        .items
        .iter()
        .filter_map(|item| {
            SourceFreeMessage::parse(&format!(
                "review {}",
                role_label(contract_role(item.candidate.role))
            ))
            .ok()
        })
        .collect();
    let omitted = pack
        .omissions
        .iter()
        .filter_map(|omission| {
            Some(OmissionSummary {
                reason: SafeLabel::parse(role_label(contract_role(omission.role))).ok()?,
                count: u32::try_from(omission.count).unwrap_or(u32::MAX),
                continuation: None,
            })
        })
        .collect();

    let mut followups = Vec::new();
    if let Ok(reason) =
        SourceFreeMessage::parse("expand callers and callees for the target symbols")
    {
        followups.push(ToolSuggestion {
            tool: "symbol.relationships".to_owned(),
            reason,
            continuation: None,
        });
    }
    if !pack.items.is_empty()
        && let Ok(reason) =
            SourceFreeMessage::parse("read full definitions for the included evidence")
    {
        followups.push(ToolSuggestion {
            tool: "source.read".to_owned(),
            reason,
            continuation: None,
        });
    }

    ContextPackData {
        pack_id: context_pack_id(request),
        request_digest: request.request_digest(),
        planner_version: rootlight_mcp_contract::context::PLANNER_VERSION,
        items,
        role_coverage,
        structure: ContextStructure {
            reading_order,
            dependencies: Vec::new(),
        },
        omitted,
        followups,
        token_accounting: TokenAccounting {
            estimated_total: pack.total_tokens,
            by_section: BTreeMap::new(),
        },
        explanation: None,
    }
}

fn append_provider_omissions(omitted: &mut Vec<OmissionSummary>, corpus: &ContextEvidenceCorpus) {
    for provider_omission in &corpus.omissions {
        let label = match provider_omission.reason {
            EvidenceProviderOmissionReason::NoEvidence => "provider_no_evidence",
            EvidenceProviderOmissionReason::Truncated => "provider_truncated",
            EvidenceProviderOmissionReason::Unsupported => "provider_unsupported",
            EvidenceProviderOmissionReason::Unavailable => "provider_unavailable",
            EvidenceProviderOmissionReason::Budget => "provider_budget",
            EvidenceProviderOmissionReason::LowConfidence => "provider_low_confidence",
        };
        let Ok(reason) = SafeLabel::parse(label) else {
            continue;
        };
        if let Some(existing) = omitted.iter_mut().find(|value| value.reason == reason) {
            existing.count = existing.count.saturating_add(provider_omission.count);
        } else if omitted.len() < MAX_OMISSIONS {
            omitted.push(OmissionSummary {
                reason,
                count: provider_omission.count,
                continuation: None,
            });
        }
    }
}

fn append_role_followups(followups: &mut Vec<ToolSuggestion>, coverage: &RoleCoverageSummary) {
    for entry in coverage
        .roles()
        .iter()
        .filter(|entry| entry.status == RoleCoverageStatus::MissingRequired)
    {
        let Some(reason) = entry.missing_reason else {
            continue;
        };
        let message = format!(
            "retrieve missing {} evidence after {}",
            role_label(entry.role),
            missing_reason_label(reason)
        );
        let Ok(reason) = SourceFreeMessage::parse(&message) else {
            continue;
        };
        followups.push(ToolSuggestion {
            tool: role_followup_tool(entry.role).to_owned(),
            reason,
            continuation: None,
        });
    }
}

const fn role_followup_tool(role: ContractEvidenceRole) -> &'static str {
    match role {
        ContractEvidenceRole::Definition => "symbol.explain",
        ContractEvidenceRole::Implementation => "source.read",
        ContractEvidenceRole::Caller => "symbol.relationships",
        ContractEvidenceRole::Test => "tests.select",
        ContractEvidenceRole::Risk => "change.impact",
        ContractEvidenceRole::Architecture => "architecture.overview",
        ContractEvidenceRole::Change => "history.compare",
    }
}

const fn missing_reason_label(reason: MissingRequiredRoleReason) -> &'static str {
    match reason {
        MissingRequiredRoleReason::NotSearched => "not searched",
        MissingRequiredRoleReason::NoEvidence => "no evidence",
        MissingRequiredRoleReason::Unsupported => "unsupported provider",
        MissingRequiredRoleReason::Unavailable => "unavailable provider",
        MissingRequiredRoleReason::Truncated => "truncated search",
        MissingRequiredRoleReason::LowConfidence => "low confidence",
        MissingRequiredRoleReason::Budget => "budget limit",
    }
}

const fn contract_objective(objective: PackObjective) -> ContractContextPackObjective {
    match objective {
        PackObjective::BugFix => ContractContextPackObjective::BugFix,
        PackObjective::Refactor => ContractContextPackObjective::Refactor,
        PackObjective::Explanation => ContractContextPackObjective::Explanation,
        PackObjective::Migration => ContractContextPackObjective::Migration,
        PackObjective::Review => ContractContextPackObjective::Review,
    }
}

const fn contract_role(role: EvidenceRole) -> ContractEvidenceRole {
    match role {
        EvidenceRole::Definition => ContractEvidenceRole::Definition,
        EvidenceRole::Implementation => ContractEvidenceRole::Implementation,
        EvidenceRole::Caller => ContractEvidenceRole::Caller,
        EvidenceRole::Test => ContractEvidenceRole::Test,
        EvidenceRole::Risk => ContractEvidenceRole::Risk,
        EvidenceRole::Architecture => ContractEvidenceRole::Architecture,
        EvidenceRole::Change => ContractEvidenceRole::Change,
    }
}

const fn role_label(role: ContractEvidenceRole) -> &'static str {
    match role {
        ContractEvidenceRole::Definition => "definition",
        ContractEvidenceRole::Implementation => "implementation",
        ContractEvidenceRole::Caller => "caller",
        ContractEvidenceRole::Test => "test",
        ContractEvidenceRole::Risk => "risk",
        ContractEvidenceRole::Architecture => "architecture",
        ContractEvidenceRole::Change => "change",
    }
}

fn checkpoint<C>(cancellation: &C) -> Result<(), ExecutionPolicyError>
where
    C: CancellationSignal,
{
    if cancellation.is_cancelled() {
        Err(ExecutionPolicyError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContextPackPlanRequest, ContextPackPlanner, DefaultContextPackPlanner, EvidenceCandidate,
        EvidenceRole, MAX_PACK_TOKENS, MIN_PACK_TOKENS, PackError, PackObjective,
        append_role_followups, context_pack_completeness, context_pack_id, contract_role,
        evaluate_role_coverage, objective_for_task, optimize_pack, role_coverage_completeness,
    };
    use crate::{
        context_evidence::{
            EvidenceProvider, EvidenceProviderOmission, EvidenceProviderOmissionReason,
        },
        context_pack_request::CanonicalContextPackRequest,
        policy::NeverCancelled,
    };
    use proptest::prelude::*;
    use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_ir::{LineRange, SourceRef, SourceSpan};
    use rootlight_mcp_contract::{
        RepositorySelector, TrustClassification,
        completeness::{CompletenessState, LimitingResourceKind, ResultCompleteness},
        context::{
            ContextPackInput, ContextSeedSelector, MissingRequiredRoleReason, RoleCoverageStatus,
        },
        vertical::{
            EntityKind, RelationSummary, RepositoryIdSelector, ResponseProfile, SymbolExplanation,
        },
    };

    fn candidate(id: &str, role: EvidenceRole, relevance: u16, tokens: u32) -> EvidenceCandidate {
        EvidenceCandidate {
            identity: id.to_owned(),
            role,
            relevance,
            confidence: 800,
            estimated_tokens: tokens,
            source_path: format!("src/{id}.rs"),
        }
    }

    fn omission(
        role: EvidenceRole,
        reason: EvidenceProviderOmissionReason,
    ) -> EvidenceProviderOmission {
        EvidenceProviderOmission {
            provider: EvidenceProvider::Definition,
            role,
            reason,
            count: 1,
            limiting_resources: Vec::new(),
        }
    }

    fn objectives() -> [PackObjective; 5] {
        [
            PackObjective::BugFix,
            PackObjective::Refactor,
            PackObjective::Explanation,
            PackObjective::Migration,
            PackObjective::Review,
        ]
    }

    fn context_input(symbol: SymbolId) -> ContextPackInput {
        ContextPackInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: RepositoryId::from_bytes([1; 16]),
            }),
            generation: None,
            task: "fix parser crash".to_owned(),
            seeds: ContextSeedSelector {
                symbols: Some(vec![symbol]),
                paths: None,
                routes: None,
                tests: None,
                located: None,
                change: None,
                plan: None,
            },
            token_budget: 500,
            source_policy: None,
            sections: None,
            diversity: None,
            min_confidence: None,
            continuation: None,
            explain: None,
        }
    }

    fn explanation(symbol: SymbolId, generation: GenerationId) -> SymbolExplanation {
        SymbolExplanation {
            symbol_id: symbol,
            kind: EntityKind::Function,
            display_name: "parse_request".to_owned(),
            signature: Some("fn parse_request(input: &str)".to_owned()),
            definition: SourceRef::new(
                RepositoryId::from_bytes([1; 16]),
                generation,
                SourceSpan::new(FileId::from_bytes([3; 20]), 10, 40).expect("source span is valid"),
                ContentHash::from_bytes([4; 32]),
                Some(LineRange::new(2, 4).expect("line range is valid")),
            ),
            relations: RelationSummary {
                outbound_exact: 0,
                outbound_candidates: 0,
                inbound_exact: 0,
                inbound_candidates: 0,
                references_exact: 0,
            },
            provenance: Vec::new(),
            confidence: 900,
            uncertainty: Vec::new(),
            trust: TrustClassification::UntrustedRepositoryData,
        }
    }

    #[test]
    fn invalid_budget_is_rejected() {
        let mut candidates = vec![candidate("a", EvidenceRole::Definition, 900, 100)];
        assert_eq!(
            optimize_pack(PackObjective::BugFix, &mut candidates, MIN_PACK_TOKENS - 1),
            Err(PackError::InvalidBudget)
        );
        assert_eq!(
            optimize_pack(PackObjective::BugFix, &mut candidates, MAX_PACK_TOKENS + 1),
            Err(PackError::InvalidBudget)
        );
    }

    #[test]
    fn empty_evidence_is_rejected() {
        assert_eq!(
            optimize_pack(PackObjective::BugFix, &mut [], MIN_PACK_TOKENS),
            Err(PackError::NoTargets)
        );
    }

    #[test]
    fn required_roles_are_prioritized() {
        let mut candidates = vec![
            candidate("arch", EvidenceRole::Architecture, 950, 100),
            candidate("def", EvidenceRole::Definition, 800, 100),
            candidate("impl", EvidenceRole::Implementation, 700, 100),
            candidate("test", EvidenceRole::Test, 600, 100),
        ];
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 1000).expect("valid pack");
        // BugFix requires Definition, Implementation, Test
        let roles: Vec<EvidenceRole> = result.items.iter().map(|i| i.candidate.role).collect();
        let def_pos = roles
            .iter()
            .position(|r| *r == EvidenceRole::Definition)
            .unwrap();
        let arch_pos = roles
            .iter()
            .position(|r| *r == EvidenceRole::Architecture)
            .unwrap();
        assert!(
            def_pos < arch_pos,
            "required Definition must come before non-required Architecture"
        );
    }

    #[test]
    fn required_roles_get_minimum_representation_under_tight_budget() {
        // A run of high-relevance Definition candidates could greedily consume
        // the whole budget and starve the other required roles. The optimizer
        // must reserve one item per required role first, so every required role
        // stays represented whenever one item per role fits the budget.
        let mut candidates = vec![
            candidate("def1", EvidenceRole::Definition, 950, 300),
            candidate("def2", EvidenceRole::Definition, 940, 300),
            candidate("def3", EvidenceRole::Definition, 930, 300),
            candidate("impl1", EvidenceRole::Implementation, 500, 300),
            candidate("test1", EvidenceRole::Test, 400, 300),
        ];
        // Budget fits exactly one of each required role (3 * 300) but not all
        // five candidates.
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 900).expect("valid pack");
        let roles: Vec<EvidenceRole> = result.items.iter().map(|i| i.candidate.role).collect();
        assert!(
            roles.contains(&EvidenceRole::Definition),
            "definition represented"
        );
        assert!(
            roles.contains(&EvidenceRole::Implementation),
            "implementation represented"
        );
        assert!(roles.contains(&EvidenceRole::Test), "test represented");
        assert!(result.total_tokens <= 900, "budget respected");
    }

    #[test]
    fn token_budget_is_respected() {
        let mut candidates = vec![
            candidate("a", EvidenceRole::Definition, 900, 500),
            candidate("b", EvidenceRole::Implementation, 800, 500),
            candidate("c", EvidenceRole::Test, 700, 500),
        ];
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 1000).expect("valid pack");
        assert!(result.total_tokens <= 1000);
        assert_eq!(result.items.len(), 2);
        assert!(result.truncated);
    }

    #[test]
    fn token_truncation_reports_the_token_resource() {
        let planner = super::planner_completeness(true)
            .expect("planner truncation remains a valid completeness state");
        let completeness = context_pack_completeness(&ResultCompleteness::complete(), &planner)
            .expect("child and planner completeness merge");
        assert!(
            completeness
                .limiting_resources
                .iter()
                .any(|resource| { resource.kind == LimitingResourceKind::EstimatedTokens })
        );
        assert!(
            !completeness
                .limiting_resources
                .iter()
                .any(|resource| resource.kind == LimitingResourceKind::Results)
        );
    }

    #[test]
    fn deduplication_limits_same_path_items() {
        let mut candidates = vec![
            EvidenceCandidate {
                identity: "a".to_owned(),
                role: EvidenceRole::Definition,
                relevance: 900,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
            },
            EvidenceCandidate {
                identity: "b".to_owned(),
                role: EvidenceRole::Implementation,
                relevance: 850,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
            },
            EvidenceCandidate {
                identity: "c".to_owned(),
                role: EvidenceRole::Caller,
                relevance: 800,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
            },
            EvidenceCandidate {
                identity: "d".to_owned(),
                role: EvidenceRole::Test,
                relevance: 750,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/other.rs".to_owned(),
            },
        ];
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 5000).expect("valid pack");
        let shared_count = result
            .items
            .iter()
            .filter(|i| i.candidate.source_path == "src/shared.rs")
            .count();
        assert!(shared_count <= 2, "at most 2 items from same path");
    }

    #[test]
    fn deterministic_ordering_for_same_input() {
        let make_candidates = || {
            vec![
                candidate("x", EvidenceRole::Definition, 900, 100),
                candidate("y", EvidenceRole::Definition, 900, 100),
                candidate("z", EvidenceRole::Caller, 800, 100),
            ]
        };
        let mut c1 = make_candidates();
        let mut c2 = make_candidates();
        let r1 = optimize_pack(PackObjective::BugFix, &mut c1, 5000).expect("valid");
        let r2 = optimize_pack(PackObjective::BugFix, &mut c2, 5000).expect("valid");
        assert_eq!(r1, r2, "same input must produce same output");
    }

    #[test]
    fn omissions_are_reported_with_continuation_handles() {
        let mut candidates = vec![
            candidate("a", EvidenceRole::Definition, 900, 900),
            candidate("b", EvidenceRole::Implementation, 800, 900),
        ];
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 1000).expect("valid pack");
        assert!(result.truncated);
        assert!(!result.omissions.is_empty());
        assert!(!result.omissions[0].continuation_handle.is_empty());
    }

    #[test]
    fn all_objectives_have_required_roles() {
        for objective in objectives() {
            assert!(
                !objective.required_roles().is_empty(),
                "{objective:?} must have required roles"
            );
            assert!(
                objective
                    .required_roles()
                    .contains(&EvidenceRole::Definition)
                    || objective.required_roles().contains(&EvidenceRole::Change),
                "{objective:?} must require Definition or Change"
            );
        }
    }

    #[test]
    fn objective_role_coverage_goldens_cover_complete_and_incomplete_packs() {
        for objective in objectives() {
            let selected = objective.required_roles().to_vec();
            let complete = evaluate_role_coverage(objective, &selected, &selected, &[])
                .expect("required roles form a valid complete golden");
            assert!(complete.complete(), "{objective:?} complete golden");

            let missing_role = objective.required_roles()[0];
            let selected = objective.required_roles()[1..].to_vec();
            let incomplete = evaluate_role_coverage(
                objective,
                &selected,
                &selected,
                &[omission(
                    missing_role,
                    EvidenceProviderOmissionReason::NoEvidence,
                )],
            )
            .expect("missing-role golden remains representable");
            assert!(!incomplete.complete(), "{objective:?} incomplete golden");
            let entry = incomplete
                .roles()
                .iter()
                .find(|entry| entry.role == contract_role(missing_role))
                .expect("every accepted role has one coverage entry");
            assert_eq!(entry.status, RoleCoverageStatus::MissingRequired);
            assert_eq!(
                entry.missing_reason,
                Some(MissingRequiredRoleReason::NoEvidence)
            );
        }
    }

    #[test]
    fn missing_required_role_reason_matrix_preserves_provider_observations() {
        let objective = PackObjective::BugFix;
        let role = EvidenceRole::Implementation;
        let cases = [
            (None, Vec::new(), MissingRequiredRoleReason::NotSearched),
            (
                Some(EvidenceProviderOmissionReason::NoEvidence),
                Vec::new(),
                MissingRequiredRoleReason::NoEvidence,
            ),
            (
                Some(EvidenceProviderOmissionReason::Unsupported),
                Vec::new(),
                MissingRequiredRoleReason::Unsupported,
            ),
            (
                Some(EvidenceProviderOmissionReason::Unavailable),
                Vec::new(),
                MissingRequiredRoleReason::Unavailable,
            ),
            (
                Some(EvidenceProviderOmissionReason::Truncated),
                Vec::new(),
                MissingRequiredRoleReason::Truncated,
            ),
            (
                Some(EvidenceProviderOmissionReason::LowConfidence),
                Vec::new(),
                MissingRequiredRoleReason::LowConfidence,
            ),
            (None, vec![role], MissingRequiredRoleReason::Budget),
        ];
        for (omission_reason, observed, expected) in cases {
            let omissions = omission_reason
                .map(|reason| vec![omission(role, reason)])
                .unwrap_or_default();
            let coverage = evaluate_role_coverage(objective, &[], &observed, &omissions)
                .expect("reason matrix is representable");
            let entry = coverage
                .roles()
                .iter()
                .find(|entry| entry.role == contract_role(role))
                .expect("implementation role is present");
            assert_eq!(entry.missing_reason, Some(expected));
            assert!(!coverage.complete());
        }
    }

    #[test]
    fn truncated_observation_takes_precedence_over_empty_search() {
        let role = EvidenceRole::Implementation;
        let coverage = evaluate_role_coverage(
            PackObjective::BugFix,
            &[],
            &[],
            &[
                omission(role, EvidenceProviderOmissionReason::NoEvidence),
                omission(role, EvidenceProviderOmissionReason::Truncated),
            ],
        )
        .expect("combined provider observations are representable");
        let entry = coverage
            .roles()
            .iter()
            .find(|entry| entry.role == contract_role(role))
            .expect("implementation role is present");
        assert_eq!(
            entry.missing_reason,
            Some(MissingRequiredRoleReason::Truncated)
        );
    }

    #[test]
    fn missing_required_roles_drive_completeness_and_followups() {
        let coverage = evaluate_role_coverage(
            PackObjective::BugFix,
            &[EvidenceRole::Definition],
            &[EvidenceRole::Definition],
            &[
                omission(
                    EvidenceRole::Implementation,
                    EvidenceProviderOmissionReason::Unsupported,
                ),
                omission(
                    EvidenceRole::Test,
                    EvidenceProviderOmissionReason::LowConfidence,
                ),
            ],
        )
        .expect("missing required roles remain representable");
        let completeness =
            role_coverage_completeness(&coverage).expect("coverage completeness is valid");
        assert_eq!(completeness.state, CompletenessState::UnsupportedPartial);
        assert!(
            completeness
                .limiting_resources
                .iter()
                .any(|resource| { resource.kind == LimitingResourceKind::Capability })
        );
        assert!(
            completeness
                .limiting_resources
                .iter()
                .any(|resource| { resource.kind == LimitingResourceKind::Coverage })
        );

        let mut followups = Vec::new();
        append_role_followups(&mut followups, &coverage);
        assert_eq!(followups.len(), 2);
        assert!(
            followups
                .iter()
                .any(|followup| followup.tool == "source.read")
        );
        assert!(
            followups
                .iter()
                .any(|followup| followup.tool == "tests.select")
        );
    }

    #[test]
    fn role_coverage_truth_is_profile_invariant() {
        let selected = PackObjective::Review.required_roles().to_vec();
        let coverage = evaluate_role_coverage(PackObjective::Review, &selected, &selected, &[])
            .expect("review coverage is representable");
        let expected = serde_json::to_value(&coverage).expect("coverage serializes");

        for profile in [
            ResponseProfile::Compact,
            ResponseProfile::Standard,
            ResponseProfile::Evidence,
        ] {
            let projected = match profile {
                ResponseProfile::Compact
                | ResponseProfile::Standard
                | ResponseProfile::Evidence => coverage.clone(),
            };
            assert_eq!(
                serde_json::to_value(projected).expect("profile coverage serializes"),
                expected
            );
        }
    }

    proptest! {
        #[test]
        fn removing_any_required_selected_role_cannot_leave_complete(
            objective_index in 0usize..5,
            required_index in 0usize..3,
        ) {
            let objective = objectives()[objective_index];
            let required = objective.required_roles();
            let removed = required[required_index % required.len()];
            let selected = required
                .iter()
                .copied()
                .filter(|role| *role != removed)
                .collect::<Vec<_>>();
            let coverage = evaluate_role_coverage(
                objective,
                &selected,
                &selected,
                &[omission(removed, EvidenceProviderOmissionReason::NoEvidence)],
            )
            .expect("property input follows the objective policy");

            prop_assert!(!coverage.complete());
            let missing_is_visible = coverage.roles().iter().any(|entry| {
                entry.role == contract_role(removed)
                    && entry.status == RoleCoverageStatus::MissingRequired
            });
            prop_assert!(missing_is_visible);
        }
    }

    #[test]
    fn complete_planner_shapes_schema_compatible_context_data() {
        let symbol = SymbolId::from_bytes([2; 20]);
        let generation = GenerationId::from_bytes([5; 20]);
        let input = context_input(symbol);
        let request =
            CanonicalContextPackRequest::new(&input, RepositoryId::from_bytes([1; 16]), generation)
                .expect("fixture request canonicalizes");
        let symbols = [explanation(symbol, generation)];

        let planned = DefaultContextPackPlanner
            .plan(
                ContextPackPlanRequest {
                    request: &request,
                    symbols: &symbols,
                },
                &NeverCancelled,
            )
            .expect("context pack is planned");

        assert_eq!(planned.data.pack_id, context_pack_id(&request));
        assert_eq!(planned.data.request_digest, request.request_digest());
        assert_eq!(
            planned.data.planner_version,
            rootlight_mcp_contract::context::PLANNER_VERSION
        );
        assert_eq!(planned.data.items.len(), 1);
        assert_eq!(planned.data.items[0].symbol_id, Some(symbol));
        assert!(!planned.data.role_coverage.complete());
        assert_eq!(
            planned.data.role_coverage.objective_rule_version(),
            rootlight_mcp_contract::context::OBJECTIVE_ROLE_POLICY_VERSION
        );
        assert_ne!(planned.completeness.state, CompletenessState::Complete);
        let expected_token_ceiling = symbols[0]
            .signature
            .as_ref()
            .map_or(0, String::len)
            .saturating_add(symbols[0].display_name.len());
        assert_eq!(
            planned.data.token_accounting.estimated_total,
            u32::try_from(expected_token_ceiling).expect("fixture ceiling fits u32")
        );
        assert!(!planned.truncated);

        let encoded = serde_json::to_value(&planned.data).expect("planned data serializes");
        let decoded =
            serde_json::from_value(encoded).expect("planned data matches the public schema type");
        assert_eq!(planned.data, decoded);
    }

    #[test]
    fn task_objective_classification_stays_source_free_and_deterministic() {
        assert_eq!(
            objective_for_task("fix parser crash"),
            PackObjective::BugFix
        );
        assert_eq!(
            objective_for_task("perform a security audit"),
            PackObjective::Review
        );
        assert_eq!(
            objective_for_task("describe the parser"),
            PackObjective::Explanation
        );
    }
}
