//! Deterministic context-pack optimizer for task-specific evidence assembly.
//!
//! The optimizer accepts typed evidence candidates from bounded providers. The
//! complete planner currently shapes generation-pinned symbol definitions into
//! the public context contract. Selection is deterministic, deduplicated, and
//! constrained by one shared token ledger.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_ids::RepositoryId;

mod source_metadata;

use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, SafeLabel, SchemaVersion, SourceFreeMessage,
    ToolResponse, TrustClassification,
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    context::{
        ContextItem, ContextPackData, ContextPackId, ContextPackInput,
        ContextPackObjective as ContractContextPackObjective, ContextSection, ContextStructure,
        Diversity, EvidenceRole as ContractEvidenceRole, MissingRequiredRoleReason,
        OmissionSummary, RepositorySnippet, RoleCoverageEntry, RoleCoverageError,
        RoleCoverageStatus, RoleCoverageSummary, RoleRequirement, SnippetProvenance, SourcePolicy,
        TokenAccounting, ToolSuggestion,
    },
    vertical::{
        GenerationSelector, ReadEnvelope, RequiredNullable, ResponseProfile, SymbolExplanation,
    },
};

use crate::{
    context_continuation::{
        ContextContinuationBinding, ContextContinuationCodec, ContextContinuationError,
        ContextContinuationState, ContextContinuationStateParts, extend_identity_digest,
    },
    context_evidence::{
        ContextEvidenceCollectionError, ContextEvidenceCollector, ContextEvidenceCorpus,
        ContextEvidencePort, ContextEvidencePortErrorKind, ContextEvidenceProviderRegistry,
        ContextSourceMaterial, ContextSourceMetadataQuoteRequest, ContextSourceOutput,
        ContextSourceRequest, ContextSourceTarget, EvidenceProviderOmission,
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

const FOCUSED_SNIPPET_BYTES: u32 = 2_048;
const STANDARD_EVIDENCE_HEAVY_BYTES: u32 = 4_096;
const EVIDENCE_SNIPPET_BYTES: u32 = 8_192;
const SIGNATURE_BYTES: u32 = 1_024;
const SOURCE_LANGUAGE_BYTES: u64 = 64;
const JSON_ESCAPE_EXPANSION: u64 = 6;
const MIN_SOURCE_MATERIAL_BYTES: u32 = 64;

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
/// This orchestration work limit is independent of the inclusive response allowance.
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
                EvidenceRole::Caller,
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
    /// Stable evidence-provider domain used for diversity.
    pub provider_key: String,
    /// Stable source-start coordinate used for within-file diversity.
    pub source_region: u32,
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
    /// Stable provider domain affected by the omission.
    pub provider_key: String,
    /// Source-free optimizer reason.
    pub reason: PackOmissionReason,
    /// Whether a fresh page can retrieve the omitted candidate.
    pub resumable: bool,
    /// Opaque continuation handle for follow-up requests.
    pub continuation_handle: String,
}

/// Optimizer-level reason an admitted candidate was not emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackOmissionReason {
    /// The remaining page token capacity was insufficient.
    Budget,
    /// Per-page file, provider, or source-region diversity rejected the item.
    Diversity,
    /// The public item ceiling ended the page.
    ItemLimit,
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
    /// A continuation did not reproduce its authenticated deterministic prefix.
    #[error("context-pack continuation does not match the deterministic frontier")]
    InvalidContinuation,
    /// The minimum truthful final representation exceeds the requested budget.
    #[error("context-pack final representation exceeds the requested token budget")]
    FinalRepresentationExceeded,
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
    /// Source-free next-page frontier awaiting authenticated transport sealing.
    pub continuation: Option<ContextContinuationState>,
    /// Ranked private identities for the current page's reconciliation pass.
    page_identities: Vec<String>,
    /// Unsealed current-page proof retained for late final-envelope eviction.
    continuation_frontier: Option<ContextContinuationFrontier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextContinuationFrontier {
    next_page: u16,
    output_budget: u32,
    corpus_digest: [u8; 32],
    page_start_digest: [u8; 32],
    page_start_count: u16,
    emitted_digest: [u8; 32],
    emitted_count: u16,
    remaining_candidates: u32,
    page_item_counts: Vec<u8>,
}

impl ContextContinuationFrontier {
    fn state(&self) -> Result<Option<ContextContinuationState>, ContextContinuationError> {
        if self.remaining_candidates == 0 {
            return Ok(None);
        }
        ContextContinuationState::new(ContextContinuationStateParts {
            next_page: self.next_page,
            output_budget: self.output_budget,
            corpus_digest: self.corpus_digest,
            page_start_digest: self.page_start_digest,
            page_start_count: self.page_start_count,
            emitted_digest: self.emitted_digest,
            emitted_count: self.emitted_count,
            remaining_candidates: self.remaining_candidates,
            page_item_counts: self.page_item_counts.clone(),
        })
        .map(Some)
    }

    fn retain_current_page(
        &mut self,
        retained_identities: &[String],
    ) -> Result<ContextContinuationState, ContextContinuationError> {
        let current_count = self
            .page_item_counts
            .last_mut()
            .ok_or(ContextContinuationError::Invalid)?;
        if retained_identities.is_empty() || retained_identities.len() > usize::from(*current_count)
        {
            return Err(ContextContinuationError::Invalid);
        }
        let retained_count = u8::try_from(retained_identities.len())
            .map_err(|_| ContextContinuationError::Invalid)?;
        let removed = current_count.saturating_sub(retained_count);
        self.emitted_count = self
            .page_start_count
            .checked_add(u16::from(retained_count))
            .ok_or(ContextContinuationError::Invalid)?;
        self.remaining_candidates = self
            .remaining_candidates
            .checked_add(u32::from(removed))
            .ok_or(ContextContinuationError::Invalid)?;
        self.emitted_digest = extend_identity_digest(self.page_start_digest, retained_identities);
        *current_count = retained_count;
        self.state()?.ok_or(ContextContinuationError::Invalid)
    }
}

#[derive(Debug, Clone)]
struct ContextCandidateMetadata {
    symbol_id: Option<rootlight_ids::SymbolId>,
    source_ref: Option<rootlight_ir::SourceRef>,
    trust: TrustClassification,
    signature: Option<String>,
    snippet: Option<RepositorySnippet>,
}

#[derive(Debug, Clone)]
struct SelectedSourceTarget {
    item_index: usize,
    target: ContextSourceTarget,
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
                    signature: if request.request.source_policy() == SourcePolicy::ReferencesOnly {
                        None
                    } else {
                        explanation.signature.clone()
                    },
                    snippet: None,
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
                provider_key: "symbol.explain".to_owned(),
                source_region: u32::try_from(explanation.definition.span().start_byte() / 4_096)
                    .unwrap_or(u32::MAX),
            });
        }

        let pack = optimize_pack_with_diversity(
            request.request.objective(),
            &mut candidates,
            u32::from(request.request.token_budget()),
            request.request.diversity(),
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
            continuation: None,
            page_identities: pack
                .items
                .iter()
                .map(|item| item.candidate.identity.clone())
                .collect(),
            continuation_frontier: None,
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
        self.collect_and_plan_page(port, request, None, cancellation, deadline)
            .await
    }

    async fn collect_and_plan_page<P, C>(
        &self,
        port: &P,
        request: &CanonicalContextPackRequest,
        continuation: Option<ContextContinuationState>,
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
        let mut planned = self.plan_corpus_page(request, &corpus, continuation, &cancellation)?;
        if request.source_policy() == SourcePolicy::ReferencesOnly {
            return Ok(planned);
        }

        let (include_snippets, configured_max_bytes_per_snippet) =
            source_materialization_limits(request.source_policy(), request.response_profile());
        let mut selected = selected_source_targets(&planned, &corpus);
        if selected.is_empty() {
            return Ok(planned);
        }

        let mut budget = BudgetLedger::with_token_limit(u64::from(request.token_budget()));
        budget
            .charge(planned.usage)
            .map_err(ContextPackPlanningError::from)?;
        // A conservative quote can fit fewer targets than their eventual
        // receipts. Requote the remaining prefix after each charged batch;
        // every successful iteration removes at least one selected target.
        loop {
            let quote_request = ContextSourceMetadataQuoteRequest {
                repository: request.repository(),
                generation: request.generation(),
                source_policy: request.source_policy(),
                include_snippets,
                targets: selected
                    .iter()
                    .map(|selected| selected.target.clone())
                    .collect(),
            };
            let quoted_bounds = match source_metadata::quote_bounds(
                port,
                quote_request,
                &mut budget,
                cancellation.clone(),
                deadline,
            )
            .await?
            {
                source_metadata::QuoteBoundsOutcome::Complete(bounds) => bounds,
                source_metadata::QuoteBoundsOutcome::Omitted(label, completeness) => {
                    mark_source_omission(&mut planned, label, selected.len(), completeness)?;
                    planned.usage = budget.consumed();
                    return Ok(planned);
                }
            };
            let materialization = quoted_bounds.affordable(
                budget.remaining(),
                configured_max_bytes_per_snippet,
                include_snippets,
            );
            if materialization.target_count == 0 {
                mark_source_omission(
                    &mut planned,
                    "source_budget",
                    selected.len(),
                    source_completeness(ContextEvidencePortErrorKind::Unavailable, true)?,
                )?;
                break;
            }
            let remaining = selected.split_off(materialization.target_count);
            let provider_reservation = quoted_bounds
                .provider(selected.len(), materialization.max_bytes_per_snippet)
                .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
            let combined_reservation = quoted_bounds
                .combined(
                    selected.len(),
                    materialization.max_bytes_per_snippet,
                    include_snippets,
                )
                .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
            let reservation = budget
                .reserve(combined_reservation)
                .map_err(ContextPackPlanningError::from)?;
            let source_request = ContextSourceRequest {
                repository: request.repository(),
                generation: request.generation(),
                source_policy: request.source_policy(),
                include_snippets,
                max_bytes_per_snippet: materialization.max_bytes_per_snippet,
                targets: selected
                    .iter()
                    .map(|target| target.target.clone())
                    .collect(),
            };
            let output = port
                .materialize_source(
                    source_request.clone(),
                    crate::context_evidence::ContextEvidenceCallContext::new(
                        cancellation.clone(),
                        deadline,
                        provider_reservation,
                    ),
                )
                .await;
            // Receipt admission does not authorize using unvalidated source material.
            let usage = match &output {
                Ok(output) => output.usage,
                Err(error) => error.usage,
            };
            reservation
                .commit(usage)
                .map_err(ContextPackPlanningError::from)?;
            if cancellation.is_cancelled() {
                return Err(ContextEvidenceCollectionError::Cancelled.into());
            }
            if Instant::now() >= deadline {
                return Err(ContextEvidenceCollectionError::DeadlineExceeded.into());
            }

            match output {
                Ok(output) => {
                    validate_source_output(&source_request, &output)?;
                    let shaping =
                        apply_source_materials(&mut planned, &selected, &output.materials)?;
                    budget
                        .charge(shaping)
                        .map_err(ContextPackPlanningError::from)?;
                    merge_source_completeness(&mut planned, &output.completeness)?;
                    let truncated_sources = output
                        .materials
                        .iter()
                        .filter(|material| {
                            material
                                .snippet
                                .as_ref()
                                .is_some_and(|snippet| snippet.truncated)
                        })
                        .count();
                    if truncated_sources != 0 {
                        mark_source_omission(
                            &mut planned,
                            "source_truncated",
                            truncated_sources,
                            snippet_truncation_completeness()?,
                        )?;
                    }
                }
                Err(error) => match error.kind {
                    ContextEvidencePortErrorKind::Cancelled => {
                        return Err(ContextEvidenceCollectionError::Cancelled.into());
                    }
                    ContextEvidencePortErrorKind::DeadlineExceeded => {
                        return Err(ContextEvidenceCollectionError::DeadlineExceeded.into());
                    }
                    ContextEvidencePortErrorKind::InvalidResponse => {
                        return Err(ContextEvidenceCollectionError::InvalidProviderResponse.into());
                    }
                    ContextEvidencePortErrorKind::Unsupported
                    | ContextEvidencePortErrorKind::Unavailable => {
                        let label = if error.kind == ContextEvidencePortErrorKind::Unsupported {
                            "source_unsupported"
                        } else {
                            "source_unavailable"
                        };
                        mark_source_omission(
                            &mut planned,
                            label,
                            selected.len().saturating_add(remaining.len()),
                            source_completeness(error.kind, false)?,
                        )?;
                        break;
                    }
                },
            }
            selected = remaining;
            if selected.is_empty() {
                break;
            }
        }
        planned.usage = budget.consumed();
        Ok(planned)
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
        self.plan_corpus_page(request, corpus, None, cancellation)
    }

    fn plan_corpus_page<C>(
        &self,
        request: &CanonicalContextPackRequest,
        corpus: &ContextEvidenceCorpus,
        continuation: Option<ContextContinuationState>,
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
            let source_region = source_ref.as_ref().map_or(0, |source| {
                u32::try_from(source.span().start_byte()).unwrap_or(u32::MAX)
            });
            let signature = (candidate.symbol_id().is_none() && source_ref.is_none())
                .then(|| candidate.identity().to_owned());
            metadata.insert(
                identity.clone(),
                ContextCandidateMetadata {
                    symbol_id: candidate.symbol_id(),
                    source_ref,
                    trust: candidate.trust(),
                    signature,
                    snippet: None,
                },
            );
            candidates.push(EvidenceCandidate {
                identity,
                role: candidate.role(),
                relevance: candidate.relevance(),
                confidence: candidate.confidence(),
                estimated_tokens: u32::try_from(candidate.cost().tokens).unwrap_or(u32::MAX),
                source_path,
                provider_key: candidate.provider().name().to_owned(),
                source_region,
            });
        }

        let mut budget = corpus.budget().clone();
        let available_tokens = u32::try_from(budget.remaining().tokens).unwrap_or(u32::MAX);
        let explicit_seed_anchors = explicit_symbol_anchor_ids(request, corpus);
        let (pack, next_continuation, continuation_frontier, cumulative_roles) =
            if candidates.is_empty() {
                if continuation.is_some() {
                    return Err(ContextPackPlanningError::InvalidContinuation);
                }
                (
                    PackResult {
                        items: Vec::new(),
                        omissions: Vec::new(),
                        total_tokens: 0,
                        truncated: false,
                    },
                    None,
                    None,
                    Vec::new(),
                )
            } else {
                let ContextPageSelection {
                    pack,
                    continuation,
                    frontier,
                    cumulative_roles,
                } = optimize_context_page(
                    request.objective(),
                    &mut candidates,
                    available_tokens,
                    request.diversity(),
                    &explicit_seed_anchors,
                    continuation,
                )?;
                (pack, continuation, Some(frontier), cumulative_roles)
            };
        budget.charge(BudgetCharge {
            results: u64::try_from(pack.items.len()).unwrap_or(u64::MAX),
            tokens: u64::from(pack.total_tokens),
            ..BudgetCharge::default()
        })?;
        checkpoint(cancellation)?;

        let observed_roles = corpus
            .candidates
            .iter()
            .map(|candidate| candidate.role())
            .collect::<Vec<_>>();
        let role_coverage = evaluate_role_coverage(
            request.objective(),
            &cumulative_roles,
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
            continuation: next_continuation,
            page_identities: pack
                .items
                .iter()
                .map(|item| item.candidate.identity.clone())
                .collect(),
            continuation_frontier,
        })
    }
}

const fn source_materialization_limits(
    source_policy: SourcePolicy,
    response_profile: ResponseProfile,
) -> (bool, u32) {
    match (source_policy, response_profile) {
        (SourcePolicy::ReferencesOnly, _) => (false, 0),
        (SourcePolicy::Signatures, _) => (false, SIGNATURE_BYTES),
        (SourcePolicy::FocusedSnippets | SourcePolicy::EvidenceHeavy, ResponseProfile::Compact) => {
            (false, SIGNATURE_BYTES)
        }
        (SourcePolicy::FocusedSnippets, ResponseProfile::Standard | ResponseProfile::Evidence) => {
            (true, FOCUSED_SNIPPET_BYTES)
        }
        (SourcePolicy::EvidenceHeavy, ResponseProfile::Standard) => {
            (true, STANDARD_EVIDENCE_HEAVY_BYTES)
        }
        (SourcePolicy::EvidenceHeavy, ResponseProfile::Evidence) => (true, EVIDENCE_SNIPPET_BYTES),
    }
}

fn selected_source_targets(
    planned: &PlannedContextPack,
    corpus: &ContextEvidenceCorpus,
) -> Vec<SelectedSourceTarget> {
    let selected = planned
        .data
        .items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            let source_ref = item.source_ref.as_ref()?;
            let candidate = corpus.candidates.iter().find(|candidate| {
                contract_role(candidate.role()) == item.role
                    && candidate.source_refs().contains(source_ref)
            })?;
            Some(SelectedSourceTarget {
                item_index,
                target: ContextSourceTarget {
                    candidate_id: candidate.id().clone(),
                    source_ref: source_ref.clone(),
                },
            })
        })
        .collect::<Vec<_>>();
    // Spend a bounded source prefix on distinct evidence before repeating the
    // same source for another role. Item order and presentation charges stay
    // unchanged; later duplicates still receive material when capacity permits.
    let mut distinct = Vec::with_capacity(selected.len());
    let mut repeated = Vec::new();
    for target in selected {
        if distinct.iter().any(|previous: &SelectedSourceTarget| {
            previous.target.source_ref == target.target.source_ref
        }) {
            repeated.push(target);
        } else {
            distinct.push(target);
        }
    }
    distinct.extend(repeated);
    distinct
}

fn explicit_symbol_anchor_ids(
    request: &CanonicalContextPackRequest,
    corpus: &ContextEvidenceCorpus,
) -> BTreeSet<String> {
    let mut seeds = request
        .seeds()
        .symbols()
        .iter()
        .chain(request.seeds().tests())
        .copied()
        .collect::<Vec<_>>();
    seeds.sort_unstable();
    seeds.dedup();

    seeds
        .into_iter()
        .filter_map(|seed| {
            corpus
                .candidates
                .iter()
                .filter(|candidate| {
                    candidate.symbol_id() == Some(seed) && !candidate.source_refs().is_empty()
                })
                .min_by(|left, right| {
                    explicit_anchor_role_rank(left.role())
                        .cmp(&explicit_anchor_role_rank(right.role()))
                        .then_with(|| right.relevance().cmp(&left.relevance()))
                        .then_with(|| right.confidence().cmp(&left.confidence()))
                        .then_with(|| left.cost().tokens.cmp(&right.cost().tokens))
                        .then_with(|| left.id().cmp(right.id()))
                })
                .map(|candidate| candidate.id().as_str().to_owned())
        })
        .collect()
}

const fn explicit_anchor_role_rank(role: EvidenceRole) -> u8 {
    match role {
        EvidenceRole::Definition => 0,
        EvidenceRole::Implementation => 1,
        EvidenceRole::Caller => 2,
        EvidenceRole::Test => 3,
        EvidenceRole::Architecture => 4,
        EvidenceRole::Change => 5,
        EvidenceRole::Risk => 6,
    }
}

fn add_budget_charge(left: BudgetCharge, right: BudgetCharge) -> BudgetCharge {
    BudgetCharge {
        rows: left.rows.saturating_add(right.rows),
        results: left.results.saturating_add(right.results),
        tokens: left.tokens.saturating_add(right.tokens),
        actual_tokens: left.actual_tokens.saturating_add(right.actual_tokens),
        source_bytes: left.source_bytes.saturating_add(right.source_bytes),
        traversal_facts: left.traversal_facts.saturating_add(right.traversal_facts),
        depth: left.depth.max(right.depth),
        paths: left.paths.saturating_add(right.paths),
        json_bytes: left.json_bytes.saturating_add(right.json_bytes),
        memory_bytes: left.memory_bytes.saturating_add(right.memory_bytes),
        time_ms: left.time_ms.max(right.time_ms),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceMaterializationPlan {
    target_count: usize,
    max_bytes_per_snippet: u32,
}

fn budget_charge_fits(charge: BudgetCharge, remaining: BudgetCharge) -> bool {
    charge.rows <= remaining.rows
        && charge.results <= remaining.results
        && charge.tokens <= remaining.tokens
        && charge.actual_tokens <= remaining.actual_tokens
        && charge.source_bytes <= remaining.source_bytes
        && charge.traversal_facts <= remaining.traversal_facts
        && charge.depth <= remaining.depth
        && charge.paths <= remaining.paths
        && charge.json_bytes <= remaining.json_bytes
        && charge.memory_bytes <= remaining.memory_bytes
        && charge.time_ms <= remaining.time_ms
}

fn validate_source_output(
    request: &ContextSourceRequest,
    output: &ContextSourceOutput,
) -> Result<(), ContextEvidenceCollectionError> {
    let mut material_sources = Vec::new();
    for material in &output.materials {
        if !material_sources.contains(&material.source_ref) {
            material_sources.push(material.source_ref.clone());
        }
    }
    if output.repository != request.repository
        || output.generation != request.generation
        || output.materials.len() > request.targets.len()
        || output.usage.results < u64::try_from(material_sources.len()).unwrap_or(u64::MAX)
        || !matches!(
            output.completeness.state,
            CompletenessState::Complete | CompletenessState::Truncated
        )
        || (output.completeness.state == CompletenessState::Complete
            && output.materials.len() != request.targets.len())
    {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
    }

    let mut observed = std::collections::BTreeSet::new();
    let mut observed_sources: Vec<&ContextSourceMaterial> = Vec::new();
    let mut returned_source_bytes = 0u64;
    for material in &output.materials {
        if !observed.insert(material.candidate_id.clone()) {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
        let Some(target) = request.targets.iter().find(|target| {
            target.candidate_id == material.candidate_id && target.source_ref == material.source_ref
        }) else {
            return Err(ContextEvidenceCollectionError::IdentityMismatch);
        };
        if target.source_ref.repository() != request.repository
            || target.source_ref.generation() != request.generation
        {
            return Err(ContextEvidenceCollectionError::IdentityMismatch);
        }
        let valid_signature = material.signature.as_ref().is_none_or(|signature| {
            !signature.is_empty()
                && signature.len()
                    <= usize::try_from(SIGNATURE_BYTES.min(request.max_bytes_per_snippet))
                        .unwrap_or(usize::MAX)
        });
        if !valid_signature {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
        let snippet_bytes = match (&material.snippet, request.include_snippets) {
            (Some(snippet), true)
                if !snippet.content.is_empty()
                    && snippet.content.len()
                        <= usize::try_from(request.max_bytes_per_snippet).unwrap_or(usize::MAX)
                    && !snippet.language.is_empty()
                    && snippet.language.len() <= 64
                    && !snippet.language.chars().any(char::is_control) =>
            {
                u64::try_from(snippet.content.len()).unwrap_or(u64::MAX)
            }
            (None, false) => 0,
            _ => return Err(ContextEvidenceCollectionError::InvalidProviderResponse),
        };
        let signature_bytes = material.signature.as_ref().map_or(0, |signature| {
            u64::try_from(signature.len()).unwrap_or(u64::MAX)
        });
        let physical_bytes =
            validate_source_projection(request, material, snippet_bytes, signature_bytes)?;
        if let Some(previous) = observed_sources
            .iter()
            .find(|previous| previous.source_ref == material.source_ref)
        {
            if previous.projection != material.projection
                || previous.signature != material.signature
                || previous.snippet != material.snippet
            {
                return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
            }
        } else {
            observed_sources.push(material);
            returned_source_bytes = returned_source_bytes
                .checked_add(physical_bytes)
                .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
        }
        if request.source_policy == SourcePolicy::Signatures && material.signature.is_none() {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
        if material.signature.is_none() && material.snippet.is_none() {
            return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
        }
    }
    if output.usage.source_bytes < returned_source_bytes {
        return Err(ContextEvidenceCollectionError::InvalidProviderResponse);
    }
    Ok(())
}

fn validate_source_projection(
    request: &ContextSourceRequest,
    material: &ContextSourceMaterial,
    snippet_bytes: u64,
    signature_bytes: u64,
) -> Result<u64, ContextEvidenceCollectionError> {
    let projection = &material.projection;
    let approved = material.source_ref.span();
    let physical_bytes = projection
        .end_byte
        .checked_sub(projection.start_byte)
        .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
    let invalid = || ContextEvidenceCollectionError::InvalidProviderResponse;
    if projection.start_byte > approved.start_byte()
        || projection.end_byte <= approved.start_byte()
        || projection.text_bytes == 0
        || projection.text_bytes > u64::from(request.max_bytes_per_snippet)
        || signature_bytes > projection.text_bytes
        || material.snippet.is_some() && snippet_bytes != projection.text_bytes
    {
        return Err(invalid());
    }
    let utf16 = projection.encoding.is_some();
    if !utf16 && physical_bytes != projection.text_bytes
        || utf16
            && ([
                projection.start_byte,
                projection.end_byte,
                approved.start_byte(),
                approved.end_byte(),
            ]
            .into_iter()
            .any(|byte| byte % 2 != 0)
                || physical_bytes > projection.text_bytes.saturating_mul(2)
                || physical_bytes < projection.text_bytes.saturating_mul(2).div_ceil(3))
    {
        return Err(invalid());
    }
    if let Some(signature) = &material.signature {
        let signature_width = source_text_width(signature, utf16)?;
        if signature_width > physical_bytes || utf16 && signature.contains('\0') {
            return Err(invalid());
        }
        if signature.contains(['\r', '\n']) || signature.trim_start() != signature.as_str() {
            return Err(invalid());
        }
        if let Some(snippet) = &material.snippet {
            let visible = visible_approved_source(&snippet.content, material, utf16)?;
            let declaration = visible
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(str::trim)
                .ok_or_else(invalid)?;
            let end = declaration
                .find(['{', ';'])
                .map_or(declaration.len(), |index| index.saturating_add(1));
            if !declaration
                .get(..end)
                .is_some_and(|authored| authored.starts_with(signature))
            {
                return Err(invalid());
            }
        }
    }
    if let Some(snippet) = &material.snippet {
        validate_visible_source_endpoints(&snippet.content, material, utf16)?;
        if projection.end_byte < approved.end_byte() && !snippet.truncated {
            return Err(invalid());
        }
    }
    Ok(physical_bytes)
}

fn source_text_width(text: &str, utf16: bool) -> Result<u64, ContextEvidenceCollectionError> {
    text.chars().try_fold(0_u64, |width, character| {
        let size = if utf16 {
            character.len_utf16() * 2
        } else {
            character.len_utf8()
        };
        width
            .checked_add(
                u64::try_from(size)
                    .map_err(|_| ContextEvidenceCollectionError::InvalidProviderResponse)?,
            )
            .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)
    })
}

fn validate_visible_source_endpoints(
    text: &str,
    material: &ContextSourceMaterial,
    utf16: bool,
) -> Result<(), ContextEvidenceCollectionError> {
    let projection = &material.projection;
    let approved = material.source_ref.span();
    let invalid = || ContextEvidenceCollectionError::InvalidProviderResponse;
    if utf16 && (text.contains('\0') || projection.start_byte == 0 && !text.starts_with('\u{feff}'))
    {
        return Err(invalid());
    }
    let mut cursor = projection.start_byte;
    // A prefix may be exhausted in preceding context before either approved endpoint.
    let mut start_visible =
        approved.start_byte() > projection.end_byte || approved.start_byte() == cursor;
    let mut end_visible =
        approved.end_byte() > projection.end_byte || approved.end_byte() == cursor;
    for character in text.chars() {
        let size = if utf16 {
            character.len_utf16() * 2
        } else {
            character.len_utf8()
        };
        cursor = cursor
            .checked_add(u64::try_from(size).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        if cursor > projection.end_byte {
            return Err(invalid());
        }
        start_visible |= approved.start_byte() == cursor;
        end_visible |= approved.end_byte() == cursor;
    }
    if cursor != projection.end_byte || !start_visible || !end_visible {
        return Err(invalid());
    }
    Ok(())
}

fn visible_approved_source<'a>(
    text: &'a str,
    material: &ContextSourceMaterial,
    utf16: bool,
) -> Result<&'a str, ContextEvidenceCollectionError> {
    let approved = material.source_ref.span();
    let mut physical = material.projection.start_byte;
    let visible_end = approved.end_byte().min(material.projection.end_byte);
    let mut start = (physical == approved.start_byte()).then_some(0);
    let mut end = (physical == visible_end).then_some(0);
    for (offset, scalar) in text.char_indices() {
        let width = if utf16 {
            scalar.len_utf16() * 2
        } else {
            scalar.len_utf8()
        };
        physical = physical
            .checked_add(u64::try_from(width).unwrap_or(u64::MAX))
            .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)?;
        let offset = offset + scalar.len_utf8();
        if physical == approved.start_byte() {
            start = Some(offset);
        }
        if physical == visible_end {
            end = Some(offset);
        }
    }
    start
        .zip(end)
        .and_then(|(start, end)| text.get(start..end))
        .filter(|visible| !visible.is_empty())
        .ok_or(ContextEvidenceCollectionError::InvalidProviderResponse)
}

fn apply_source_materials(
    planned: &mut PlannedContextPack,
    selected: &[SelectedSourceTarget],
    materials: &[ContextSourceMaterial],
) -> Result<BudgetCharge, ContextEvidenceCollectionError> {
    let mut shaping = BudgetCharge::default();
    for material in materials {
        let Some(selected) = selected.iter().find(|selected| {
            selected.target.candidate_id == material.candidate_id
                && selected.target.source_ref == material.source_ref
        }) else {
            continue;
        };
        let item = &mut planned.data.items[selected.item_index];
        // Validated snippets already contain the selected declaration. Keep
        // the signature only when it is the sole source presentation.
        item.signature = if material.snippet.is_none() {
            material.signature.clone()
        } else {
            None
        };
        item.snippet = material.snippet.as_ref().map(|snippet| RepositorySnippet {
            source_ref: material.source_ref.clone(),
            content: snippet.content.clone(),
            language: snippet.language.clone(),
            provenance: SnippetProvenance::SourceRead,
            truncated: snippet.truncated,
            trust: TrustClassification::UntrustedRepositoryData,
        });
        let represented_bytes = item
            .signature
            .as_ref()
            .map_or(0, |text| json_string_content_bytes(text))
            .saturating_add(material.snippet.as_ref().map_or(0, |snippet| {
                json_string_content_bytes(&snippet.content)
                    .saturating_add(json_string_content_bytes(&snippet.language))
            }));
        let metadata =
            source_metadata::shaping_fixed(&material.source_ref, material.snippet.is_some())?;
        let represented_bytes = represented_bytes.saturating_add(metadata.json_bytes);
        shaping.tokens = shaping.tokens.saturating_add(represented_bytes);
        shaping.json_bytes = shaping.json_bytes.saturating_add(represented_bytes);
        let owned_bytes = item
            .signature
            .as_ref()
            .map_or(0, String::capacity)
            .saturating_add(item.snippet.as_ref().map_or(0, |snippet| {
                snippet
                    .content
                    .capacity()
                    .saturating_add(snippet.language.capacity())
            }));
        shaping.memory_bytes = shaping.memory_bytes.saturating_add(
            u64::try_from(owned_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(metadata.memory_bytes),
        );
        let item_tokens = u32::try_from(represented_bytes).unwrap_or(u32::MAX);
        item.tokens = item.tokens.saturating_add(item_tokens);
        planned.data.token_accounting.estimated_total = planned
            .data
            .token_accounting
            .estimated_total
            .saturating_add(item_tokens);
    }
    Ok(shaping)
}

fn json_string_content_bytes(text: &str) -> u64 {
    text.bytes().fold(0_u64, |bytes, byte| {
        bytes.saturating_add(match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
            0..=31 => 6,
            _ => 1,
        })
    })
}

fn source_completeness(
    kind: ContextEvidencePortErrorKind,
    budget_limited: bool,
) -> Result<ResultCompleteness, ContextPackPlanningError> {
    let (state, resource, guidance) = if budget_limited {
        (
            CompletenessState::Truncated,
            LimitingResourceKind::EstimatedTokens,
            ContinuationGuidance::IncreaseBudgetWithinLimit,
        )
    } else if kind == ContextEvidencePortErrorKind::Unsupported {
        (
            CompletenessState::UnsupportedPartial,
            LimitingResourceKind::Capability,
            ContinuationGuidance::UnsupportedNoContinuation,
        )
    } else {
        (
            CompletenessState::Indeterminate,
            LimitingResourceKind::Coverage,
            ContinuationGuidance::RefreshCoverage,
        )
    };
    ResultCompleteness::new(
        state,
        vec![LimitingResource::kind(resource)],
        ContinuationAvailability::Unavailable,
        vec![guidance],
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
}

fn snippet_truncation_completeness() -> Result<ResultCompleteness, ContextPackPlanningError> {
    ResultCompleteness::new(
        CompletenessState::Truncated,
        vec![LimitingResource::kind(LimitingResourceKind::SourceBytes)],
        ContinuationAvailability::Unavailable,
        vec![ContinuationGuidance::RequestSource],
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
}

fn merge_source_completeness(
    planned: &mut PlannedContextPack,
    source: &ResultCompleteness,
) -> Result<(), ContextPackPlanningError> {
    planned.completeness = planned
        .completeness
        .merge(source)
        .map_err(|_| ContextPackPlanningError::InvalidCompleteness)?;
    planned.truncated |= source.state != CompletenessState::Complete;
    Ok(())
}

fn mark_source_omission(
    planned: &mut PlannedContextPack,
    label: &str,
    count: usize,
    completeness: ResultCompleteness,
) -> Result<(), ContextPackPlanningError> {
    if let Ok(reason) = SafeLabel::parse(label) {
        let provider = SafeLabel::parse("source_read").ok();
        if let Some(previous) = planned.data.omitted.iter_mut().find(|omission| {
            omission.reason == reason
                && omission.role.is_none()
                && omission.provider == provider
                && omission.limiting_resources == completeness.limiting_resources
                && !omission.resumable
        }) {
            previous.count = previous
                .count
                .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
        } else {
            planned.data.omitted.push(OmissionSummary {
                role: None,
                reason,
                provider,
                count: u32::try_from(count).unwrap_or(u32::MAX),
                limiting_resources: completeness.limiting_resources.clone(),
                resumable: false,
                continuation: None,
            });
            planned.data.omitted.truncate(MAX_OMISSIONS);
        }
    }
    merge_source_completeness(planned, &completeness)
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
    /// A shared planning, evidence, or minimum publication budget was exhausted.
    BudgetExceeded,
    /// A child response violated the pinned identity or typed contract.
    InvalidResponse,
    /// A continuation is malformed, expired, or bound to another request.
    InvalidContinuation,
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
        P: AgentToolPort<C> + ContextEvidencePort<C> + ContextContinuationCodec,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        validate_supported_fields(&input)?;
        let started_at = Instant::now();
        let deadline = started_at
            .checked_add(Duration::from_millis(u64::from(CONTEXT_PACK_TIMEOUT_MS)))
            .ok_or(ContextPackServiceError::Unavailable)?;
        context_service_checkpoint(&cancellation, deadline)?;
        let continuation_request = input.continuation.is_some();
        let identity = port
            .resolve_identity(
                AgentIdentityRequest::new(input.repository.clone(), input.generation.clone()),
                AgentResolutionContext::new(cancellation.clone(), deadline),
            )
            .await
            .map_err(|error| map_identity_port_error(error, continuation_request))?;
        context_service_checkpoint(&cancellation, deadline)?;

        Self::execute_admitted_with_identity(
            port,
            input,
            repository,
            identity,
            cancellation,
            deadline,
            started_at,
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
        P: AgentToolPort<C> + ContextEvidencePort<C> + ContextContinuationCodec,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        validate_supported_fields(&input)?;
        let started_at = Instant::now();
        context_service_checkpoint(&cancellation, deadline)?;

        Self::execute_admitted_with_identity(
            port,
            input,
            repository,
            identity,
            cancellation,
            deadline,
            started_at,
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
        started_at: Instant,
    ) -> Result<ReadEnvelope<ContextPackData>, ContextPackServiceError>
    where
        P: AgentToolPort<C> + ContextEvidencePort<C> + ContextContinuationCodec,
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
        let continuation_binding = ContextContinuationBinding {
            repository: canonical.repository(),
            generation: canonical.generation(),
            request_digest: canonical.digest_bytes(),
            response_profile: canonical.response_profile(),
            token_budget: canonical.token_budget(),
            planner_version: rootlight_mcp_contract::context::PLANNER_VERSION,
            role_policy_version: rootlight_mcp_contract::context::OBJECTIVE_ROLE_POLICY_VERSION,
        };
        let continuation = input
            .continuation
            .as_ref()
            .map(|cursor| {
                port.open_context_continuation(cursor, continuation_binding)
                    .map_err(map_continuation_error)
            })
            .transpose()?;
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
            let mut envelope = ReadEnvelope {
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
            };
            envelope.usage.wall_time_ms = elapsed_millis(started_at);
            reconcile_final_envelope(&canonical, &mut envelope)
                .map_err(|_| ContextPackServiceError::BudgetExceeded)?;
            return Ok(envelope);
        }

        let mut planned = DefaultContextPackPlanner
            .collect_and_plan_page(
                port.as_ref(),
                &canonical,
                continuation,
                cancellation.clone(),
                deadline,
            )
            .await
            .map_err(map_evidence_planning_error)?;
        context_service_checkpoint(&cancellation, deadline)?;
        loop {
            let next_cursor = planned
                .continuation
                .as_ref()
                .map(|state| {
                    port.seal_context_continuation(state.clone(), continuation_binding)
                        .map_err(map_continuation_error)
                })
                .transpose()?;
            let mut data = planned.data.clone();
            if let Some(cursor) = &next_cursor {
                attach_context_cursor(&mut data, cursor, canonical.response_profile());
            }
            let completeness = if next_cursor.is_some() {
                resumable_completeness(&planned.completeness)
                    .map_err(|_| ContextPackServiceError::InvalidResponse)?
            } else {
                planned.completeness.clone()
            };
            let mut envelope = ReadEnvelope {
                schema_version: SchemaVersion::V1_0,
                repository: identity.repository.clone(),
                generation: identity.generation.clone(),
                coverage: identity.coverage.clone(),
                data,
                truncated: planned.truncated || next_cursor.is_some(),
                completeness,
                next_cursor: RequiredNullable(next_cursor),
                usage: usage_summary(planned.usage, "context-pack"),
                warnings: identity.warnings.clone(),
                trust: TrustClassification::UntrustedRepositoryData,
            };
            envelope.usage.wall_time_ms = elapsed_millis(started_at);
            match reconcile_final_envelope(&canonical, &mut envelope) {
                Ok(()) => return Ok(envelope),
                Err(ContextPackPlanningError::FinalRepresentationExceeded)
                    if evict_lowest_ranked_optional(&mut planned)? =>
                {
                    continue;
                }
                Err(ContextPackPlanningError::FinalRepresentationExceeded) => {
                    return Err(ContextPackServiceError::BudgetExceeded);
                }
                Err(_) => return Err(ContextPackServiceError::Unavailable),
            }
        }
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
    if input.continuation.is_some() && input.explain == Some(true) {
        return Err(ContextPackServiceError::InvalidContinuation);
    }
    Ok(())
}

const fn map_continuation_error(error: ContextContinuationError) -> ContextPackServiceError {
    match error {
        ContextContinuationError::Invalid => ContextPackServiceError::InvalidContinuation,
        ContextContinuationError::Unavailable => ContextPackServiceError::Unavailable,
    }
}

fn resumable_completeness(
    current: &ResultCompleteness,
) -> Result<ResultCompleteness, ContextPackPlanningError> {
    let mut resources = current.limiting_resources.clone();
    resources.push(LimitingResource::kind(
        LimitingResourceKind::EstimatedTokens,
    ));
    resources.sort_unstable();
    resources.dedup_by_key(|resource| resource.kind);
    let mut guidance = current.guidance.clone();
    guidance.push(ContinuationGuidance::UseCursor);
    guidance.sort_unstable();
    guidance.dedup();
    ResultCompleteness::new(
        if current.state == CompletenessState::Complete {
            CompletenessState::Truncated
        } else {
            current.state
        },
        resources,
        ContinuationAvailability::Available,
        guidance,
    )
    .map_err(|_| ContextPackPlanningError::InvalidCompleteness)
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

fn map_identity_port_error(
    error: AgentPortError,
    continuation_request: bool,
) -> ContextPackServiceError {
    let (error, _) = error.into_parts();
    if continuation_request
        && matches!(
            &error,
            AgentPortError::Public(public) if public.code() == ErrorCode::StaleGeneration
        )
    {
        return ContextPackServiceError::InvalidContinuation;
    }
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
            }) => ContextPackServiceError::BudgetExceeded,
        },
        ContextEvidencePlanningError::Planning(error) => match error {
            ContextPackPlanningError::Policy(ExecutionPolicyError::Cancelled) => {
                ContextPackServiceError::Cancelled
            }
            ContextPackPlanningError::Pack(_)
            | ContextPackPlanningError::Policy(ExecutionPolicyError::BudgetExceeded { .. }) => {
                ContextPackServiceError::BudgetExceeded
            }
            ContextPackPlanningError::InvalidCompleteness => {
                ContextPackServiceError::InvalidResponse
            }
            ContextPackPlanningError::InvalidRoleCoverage => {
                ContextPackServiceError::InvalidResponse
            }
            ContextPackPlanningError::InvalidContinuation => {
                ContextPackServiceError::InvalidContinuation
            }
            ContextPackPlanningError::FinalRepresentationExceeded => {
                ContextPackServiceError::BudgetExceeded
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

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros())
        .unwrap_or(u64::MAX)
        .div_ceil(1_000)
}

#[derive(Debug)]
struct ContextPageSelection {
    pack: PackResult,
    continuation: Option<ContextContinuationState>,
    frontier: ContextContinuationFrontier,
    cumulative_roles: Vec<EvidenceRole>,
}

fn optimize_context_page(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    token_budget: u32,
    diversity: Diversity,
    explicit_seed_anchors: &BTreeSet<String>,
    continuation: Option<ContextContinuationState>,
) -> Result<ContextPageSelection, ContextPackPlanningError> {
    if candidates.is_empty() {
        return Err(PackError::NoTargets.into());
    }
    rank_candidates(objective, candidates, diversity, explicit_seed_anchors);
    let corpus_digest = candidate_corpus_digest(candidates);
    let target_page = continuation
        .as_ref()
        .map_or(0, ContextContinuationState::next_page);
    if continuation.as_ref().is_some_and(|state| {
        state.output_budget() != token_budget || state.corpus_digest() != corpus_digest
    }) {
        return Err(ContextPackPlanningError::InvalidContinuation);
    }
    let mut page_item_counts = continuation
        .as_ref()
        .map_or_else(Vec::new, |state| state.page_item_counts().to_vec());

    let mut emitted = BTreeSet::new();
    let mut emitted_order = Vec::new();
    let mut cumulative_roles = Vec::new();
    for page_index in 0..target_page {
        let mut prior = select_remaining_page(
            objective,
            candidates,
            &emitted,
            token_budget,
            diversity,
            explicit_seed_anchors,
        )?;
        let authenticated_count = page_item_counts
            .get(usize::from(page_index))
            .copied()
            .ok_or(ContextPackPlanningError::InvalidContinuation)?;
        if usize::from(authenticated_count) > prior.items.len() {
            return Err(ContextPackPlanningError::InvalidContinuation);
        }
        prior.items.truncate(usize::from(authenticated_count));
        prior.total_tokens = prior.items.iter().fold(0_u32, |total, item| {
            total.saturating_add(item.candidate.estimated_tokens)
        });
        if prior.items.is_empty() {
            return Err(ContextPackPlanningError::InvalidContinuation);
        }
        append_emitted(
            &prior,
            &mut emitted,
            &mut emitted_order,
            &mut cumulative_roles,
        );
        if page_index + 1 < target_page
            && !has_resumable_candidate(candidates, &emitted, token_budget)
        {
            return Err(ContextPackPlanningError::InvalidContinuation);
        }
    }

    if let Some(state) = continuation.as_ref() {
        let emitted_count = u16::try_from(emitted_order.len())
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?;
        let remaining = u32::try_from(candidates.len().saturating_sub(emitted.len()))
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?;
        if state.emitted_count() != emitted_count
            || state.emitted_digest() != emitted_identity_digest(&emitted_order)
            || state.remaining_candidates() != remaining
        {
            return Err(ContextPackPlanningError::InvalidContinuation);
        }
    }

    let page_start_digest = emitted_identity_digest(&emitted_order);
    let page_start_count = u16::try_from(emitted_order.len())
        .map_err(|_| ContextPackPlanningError::InvalidContinuation)?;
    let mut pack = select_remaining_page(
        objective,
        candidates,
        &emitted,
        token_budget,
        diversity,
        explicit_seed_anchors,
    )?;
    append_emitted(
        &pack,
        &mut emitted,
        &mut emitted_order,
        &mut cumulative_roles,
    );
    let has_next = has_resumable_candidate(candidates, &emitted, token_budget);
    pack.truncated |= has_next;
    let next_page = target_page
        .checked_add(1)
        .ok_or(ContextPackPlanningError::InvalidContinuation)?;
    page_item_counts.push(
        u8::try_from(pack.items.len())
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?,
    );
    let frontier = ContextContinuationFrontier {
        next_page,
        output_budget: token_budget,
        corpus_digest,
        page_start_digest,
        page_start_count,
        emitted_digest: emitted_identity_digest(&emitted_order),
        emitted_count: u16::try_from(emitted_order.len())
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?,
        remaining_candidates: u32::try_from(candidates.len().saturating_sub(emitted.len()))
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?,
        page_item_counts,
    };
    let next = if has_next && !pack.items.is_empty() {
        frontier
            .state()
            .map_err(|_| ContextPackPlanningError::InvalidContinuation)?
    } else {
        None
    };

    Ok(ContextPageSelection {
        pack,
        continuation: next,
        frontier,
        cumulative_roles,
    })
}

fn select_remaining_page(
    objective: PackObjective,
    candidates: &[EvidenceCandidate],
    emitted: &BTreeSet<String>,
    token_budget: u32,
    diversity: Diversity,
    explicit_seed_anchors: &BTreeSet<String>,
) -> Result<PackResult, PackError> {
    let mut remaining = candidates
        .iter()
        .filter(|candidate| !emitted.contains(&candidate.identity))
        .cloned()
        .collect::<Vec<_>>();
    if remaining.is_empty() {
        return Ok(PackResult {
            items: Vec::new(),
            omissions: Vec::new(),
            total_tokens: 0,
            truncated: false,
        });
    }
    optimize_admitted_pack(
        objective,
        &mut remaining,
        token_budget,
        diversity,
        explicit_seed_anchors,
    )
}

fn append_emitted(
    page: &PackResult,
    emitted: &mut BTreeSet<String>,
    emitted_order: &mut Vec<String>,
    roles: &mut Vec<EvidenceRole>,
) {
    for item in &page.items {
        if emitted.insert(item.candidate.identity.clone()) {
            emitted_order.push(item.candidate.identity.clone());
            roles.push(item.candidate.role);
        }
    }
}

fn has_resumable_candidate(
    candidates: &[EvidenceCandidate],
    emitted: &BTreeSet<String>,
    token_budget: u32,
) -> bool {
    candidates.iter().any(|candidate| {
        !emitted.contains(&candidate.identity) && candidate.estimated_tokens <= token_budget
    })
}

fn candidate_corpus_digest(candidates: &[EvidenceCandidate]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("rootlight.context-pack.candidate-corpus.v1");
    hasher.update(
        &u64::try_from(candidates.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    for candidate in candidates {
        hash_context_bytes(&mut hasher, candidate.identity.as_bytes());
        hasher.update(&[candidate.role.priority()]);
        hasher.update(&candidate.relevance.to_le_bytes());
        hasher.update(&candidate.confidence.to_le_bytes());
        hasher.update(&candidate.estimated_tokens.to_le_bytes());
        hash_context_bytes(&mut hasher, candidate.source_path.as_bytes());
        hash_context_bytes(&mut hasher, candidate.provider_key.as_bytes());
        hasher.update(&candidate.source_region.to_le_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn emitted_identity_digest(identities: &[String]) -> [u8; 32] {
    extend_identity_digest([0; 32], identities)
}

fn hash_context_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
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
    optimize_admitted_pack(
        objective,
        candidates,
        token_budget,
        Diversity::Balanced,
        &BTreeSet::new(),
    )
}

/// Optimizes a context pack with an explicit deterministic diversity bias.
///
/// # Errors
///
/// Returns [`PackError`] when the budget or target set is invalid.
pub fn optimize_pack_with_diversity(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    token_budget: u32,
    diversity: Diversity,
) -> Result<PackResult, PackError> {
    if !(MIN_PACK_TOKENS..=MAX_PACK_TOKENS).contains(&token_budget) {
        return Err(PackError::InvalidBudget);
    }
    optimize_admitted_pack(
        objective,
        candidates,
        token_budget,
        diversity,
        &BTreeSet::new(),
    )
}

fn optimize_admitted_pack(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    token_budget: u32,
    diversity: Diversity,
    explicit_seed_anchors: &BTreeSet<String>,
) -> Result<PackResult, PackError> {
    if token_budget > MAX_PACK_TOKENS {
        return Err(PackError::InvalidBudget);
    }
    if candidates.is_empty() {
        return Err(PackError::NoTargets);
    }

    rank_candidates(objective, candidates, diversity, explicit_seed_anchors);
    let required = objective.required_roles();

    // Explicit symbol and test seeds are user-selected subjects, so each
    // source-bearing anchor gets first claim on the page budget. This prevents
    // role diversity from preserving the requested identities while dropping
    // the semantic material for every subject except the highest-ranked one.
    let mut reserved = vec![false; candidates.len()];
    let mut reserved_tokens = 0u32;
    let mut reserved_items = 0usize;
    let mut anchor_indices = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| explicit_seed_anchors.contains(&candidate.identity))
        .collect::<Vec<_>>();
    anchor_indices.sort_by(|(_, left), (_, right)| {
        left.estimated_tokens
            .cmp(&right.estimated_tokens)
            .then_with(|| Reverse(left.relevance).cmp(&Reverse(right.relevance)))
            .then_with(|| Reverse(left.confidence).cmp(&Reverse(right.confidence)))
            .then_with(|| left.identity.cmp(&right.identity))
    });
    for (index, candidate) in anchor_indices {
        if reserved_items == MAX_PACK_ITEMS {
            break;
        }
        if reserved_tokens.saturating_add(candidate.estimated_tokens) <= token_budget {
            reserved[index] = true;
            reserved_tokens = reserved_tokens.saturating_add(candidate.estimated_tokens);
            reserved_items = reserved_items.saturating_add(1);
        }
    }

    // Reserve one fitting candidate per remaining required role. Required
    // coverage may legitimately place several distinct symbols in one file,
    // so the per-source diversity cap applies only to optional greedy fill.
    // Candidates are already ranked by task relevance and confidence; source
    // diversity must not displace an explicitly task-relevant required item.
    for required_role in required {
        if reserved_items == MAX_PACK_ITEMS {
            break;
        }
        if candidates
            .iter()
            .enumerate()
            .any(|(index, candidate)| reserved[index] && candidate.role == *required_role)
        {
            continue;
        }
        let fits = |candidate: &EvidenceCandidate| {
            candidate.role == *required_role
                && reserved_tokens.saturating_add(candidate.estimated_tokens) <= token_budget
        };
        let selected = candidates
            .iter()
            .enumerate()
            .find(|(index, candidate)| !reserved[*index] && fits(candidate));
        if let Some((index, candidate)) = selected {
            reserved[index] = true;
            reserved_tokens = reserved_tokens.saturating_add(candidate.estimated_tokens);
            reserved_items = reserved_items.saturating_add(1);
        }
    }

    // Emit in deterministic ranked order. Reserved candidates are always
    // included so every represented required role stays present; the remaining
    // budget is filled greedily under the per-source diversity bound. Greedy
    // spending is capped at the budget left over after reservation so greedy
    // items can never displace a reserved required-role representative. The
    // item cap likewise leaves one slot for every reservation not yet emitted.
    let greedy_budget = token_budget.saturating_sub(reserved_tokens);
    let mut greedy_spent = 0u32;
    let mut items: Vec<PackItem> = Vec::new();
    let mut omissions = Vec::new();
    let mut total_tokens = 0u32;
    let mut truncated = false;
    let mut seen_paths: Vec<&str> = Vec::new();
    let mut seen_providers: Vec<&str> = Vec::new();
    let mut seen_regions: Vec<(&str, u32)> = Vec::new();
    let mut remaining_reserved = reserved_items;

    for (index, candidate) in candidates.iter().enumerate() {
        if reserved[index] {
            remaining_reserved = remaining_reserved.saturating_sub(1);
        } else {
            if items.len().saturating_add(remaining_reserved) >= MAX_PACK_ITEMS {
                record_omission(
                    &mut omissions,
                    candidate,
                    PackOmissionReason::ItemLimit,
                    candidate.estimated_tokens <= token_budget,
                );
                truncated = true;
                continue;
            }
            // Deduplication: skip items from the same source path if we already
            // have two items from it (diversity constraint).
            let diversity_limited = path_count(&seen_paths, &candidate.source_path) >= 2
                || path_count(&seen_providers, &candidate.provider_key) >= 4
                || seen_regions.iter().any(|(path, region)| {
                    *path == candidate.source_path && *region == candidate.source_region
                });
            let budget_limited =
                greedy_spent.saturating_add(candidate.estimated_tokens) > greedy_budget;
            if diversity_limited || budget_limited {
                record_omission(
                    &mut omissions,
                    candidate,
                    if diversity_limited {
                        PackOmissionReason::Diversity
                    } else {
                        PackOmissionReason::Budget
                    },
                    candidate.estimated_tokens <= token_budget,
                );
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
        seen_providers.push(candidate.provider_key.as_str());
        seen_regions.push((candidate.source_path.as_str(), candidate.source_region));
    }

    omissions.truncate(MAX_OMISSIONS);

    Ok(PackResult {
        items,
        omissions,
        total_tokens,
        truncated,
    })
}

fn rank_candidates(
    objective: PackObjective,
    candidates: &mut [EvidenceCandidate],
    diversity: Diversity,
    explicit_seed_anchors: &BTreeSet<String>,
) {
    let required = objective.required_roles();
    candidates.sort_by(|a, b| {
        let a_anchor = explicit_seed_anchors.contains(&a.identity);
        let b_anchor = explicit_seed_anchors.contains(&b.identity);
        let a_required = required.contains(&a.role);
        let b_required = required.contains(&b.role);
        b_anchor
            .cmp(&a_anchor)
            .then_with(|| b_required.cmp(&a_required))
            .then_with(|| diversity_rank(diversity, a.role).cmp(&diversity_rank(diversity, b.role)))
            .then_with(|| a.role.priority().cmp(&b.role.priority()))
            .then_with(|| b.relevance.cmp(&a.relevance))
            .then_with(|| b.confidence.cmp(&a.confidence))
            .then_with(|| a.identity.cmp(&b.identity))
    });
}

const fn diversity_rank(diversity: Diversity, role: EvidenceRole) -> u8 {
    match diversity {
        Diversity::Balanced => role.priority(),
        Diversity::Implementation => {
            if matches!(role, EvidenceRole::Implementation) {
                0
            } else {
                role.priority() + 1
            }
        }
        Diversity::Tests => {
            if matches!(role, EvidenceRole::Test) {
                0
            } else {
                role.priority() + 1
            }
        }
        Diversity::Impact => match role {
            EvidenceRole::Risk => 0,
            EvidenceRole::Change => 1,
            EvidenceRole::Caller => 2,
            _ => role.priority() + 3,
        },
        Diversity::Architecture => match role {
            EvidenceRole::Architecture => 0,
            EvidenceRole::Caller => 1,
            _ => role.priority() + 2,
        },
    }
}

/// Counts how many selected items came from one source path.
fn path_count(seen_paths: &[&str], path: &str) -> usize {
    seen_paths.iter().filter(|p| **p == path).count()
}

fn record_omission(
    omissions: &mut Vec<OmissionEntry>,
    candidate: &EvidenceCandidate,
    reason: PackOmissionReason,
    resumable: bool,
) {
    if let Some(existing) = omissions.iter_mut().find(|omission| {
        omission.role == candidate.role
            && omission.provider_key == candidate.provider_key
            && omission.reason == reason
            && omission.resumable == resumable
    }) {
        existing.count += 1;
        existing.estimated_tokens = existing
            .estimated_tokens
            .saturating_add(candidate.estimated_tokens);
    } else if omissions.len() < MAX_OMISSIONS {
        omissions.push(OmissionEntry {
            role: candidate.role,
            count: 1,
            estimated_tokens: candidate.estimated_tokens,
            provider_key: candidate.provider_key.clone(),
            reason,
            resumable,
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
                signature: metadata.and_then(|value| value.signature.clone()),
                score: item.candidate.relevance,
                tokens: item.candidate.estimated_tokens,
                trust: metadata.map_or(TrustClassification::UntrustedRepositoryData, |value| {
                    value.trust
                }),
                snippet: metadata.and_then(|value| value.snippet.clone()),
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
    let selected_roles = pack
        .items
        .iter()
        .map(|item| item.candidate.role)
        .collect::<Vec<_>>();
    let dependencies = [
        (
            EvidenceRole::Definition,
            EvidenceRole::Implementation,
            "definition before implementation",
        ),
        (
            EvidenceRole::Definition,
            EvidenceRole::Architecture,
            "definition before architecture",
        ),
        (
            EvidenceRole::Implementation,
            EvidenceRole::Test,
            "implementation before tests",
        ),
        (
            EvidenceRole::Implementation,
            EvidenceRole::Risk,
            "implementation before risks",
        ),
        (
            EvidenceRole::Change,
            EvidenceRole::Risk,
            "changes before risks",
        ),
    ]
    .into_iter()
    .filter(|(prerequisite, dependent, _)| {
        selected_roles.contains(prerequisite) && selected_roles.contains(dependent)
    })
    .filter_map(|(_, _, message)| SourceFreeMessage::parse(message).ok())
    .collect();
    let omitted = pack
        .omissions
        .iter()
        .filter_map(|omission| {
            let reason = match omission.reason {
                PackOmissionReason::Budget => "selection_budget",
                PackOmissionReason::Diversity => "selection_diversity",
                PackOmissionReason::ItemLimit => "selection_item_limit",
            };
            Some(OmissionSummary {
                role: Some(contract_role(omission.role)),
                reason: SafeLabel::parse(reason).ok()?,
                provider: SafeLabel::parse(&omission.provider_key).ok(),
                count: u32::try_from(omission.count).unwrap_or(u32::MAX),
                limiting_resources: vec![LimitingResource::kind(match omission.reason {
                    PackOmissionReason::Budget => LimitingResourceKind::EstimatedTokens,
                    PackOmissionReason::Diversity | PackOmissionReason::ItemLimit => {
                        LimitingResourceKind::Results
                    }
                })],
                resumable: omission.resumable,
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
            dependencies,
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

fn attach_context_cursor(
    data: &mut ContextPackData,
    cursor: &rootlight_mcp_contract::vertical::ContinuationCursor,
    profile: ResponseProfile,
) {
    // Compact pages retain the actionable follow-up and envelope cursor without
    // repeating the same authenticated handle in every omission category.
    if profile != ResponseProfile::Compact {
        for omission in &mut data.omitted {
            if omission.resumable {
                omission.continuation = Some(cursor.clone());
            }
        }
    }
    if let Ok(reason) =
        SourceFreeMessage::parse("continue the authenticated context evidence frontier")
    {
        data.followups.push(ToolSuggestion {
            tool: "context.pack".to_owned(),
            reason,
            continuation: Some(cursor.clone()),
        });
        data.followups.truncate(MAX_OMISSIONS);
    }
}

fn reconcile_final_envelope(
    request: &CanonicalContextPackRequest,
    envelope: &mut ReadEnvelope<ContextPackData>,
) -> Result<(), ContextPackPlanningError> {
    reconcile_final_envelope_with_budget(request, envelope, request.token_budget())
}

fn reconcile_final_envelope_with_budget(
    request: &CanonicalContextPackRequest,
    envelope: &mut ReadEnvelope<ContextPackData>,
    token_budget: u16,
) -> Result<(), ContextPackPlanningError> {
    let mut by_section = context_section_accounting(request, &envelope.data)?;
    by_section.insert("envelope".to_owned(), 0);
    for _ in 0..12 {
        let accounted_without_envelope = by_section
            .iter()
            .filter(|(section, _)| section.as_str() != "envelope")
            .map(|(_, tokens)| *tokens)
            .fold(0_u32, u32::saturating_add);
        let estimated_total = by_section
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add);
        envelope.data.token_accounting = TokenAccounting {
            estimated_total,
            by_section: by_section.clone(),
        };
        let serialized = serde_json::to_vec(&ToolResponse::Success(&*envelope))
            .map_err(|_| ContextPackPlanningError::FinalRepresentationExceeded)?;
        let json_bytes = u64::try_from(serialized.len()).unwrap_or(u64::MAX);
        let measured_tokens = rootlight_mcp_contract::accounting::estimate_tokens(serialized.len());
        let measured_tokens_u32 = u32::try_from(measured_tokens).unwrap_or(u32::MAX);
        let envelope_tokens = measured_tokens_u32.saturating_sub(accounted_without_envelope);
        let stable = envelope.usage.json_bytes == json_bytes
            && envelope.usage.estimated_tokens == measured_tokens
            && by_section.get("envelope").copied() == Some(envelope_tokens)
            && envelope.data.token_accounting.estimated_total == measured_tokens_u32;
        envelope.usage.json_bytes = json_bytes;
        envelope.usage.estimated_tokens = measured_tokens;
        by_section.insert("envelope".to_owned(), envelope_tokens);
        if stable {
            if measured_tokens > u64::from(token_budget) {
                return Err(ContextPackPlanningError::FinalRepresentationExceeded);
            }
            return Ok(());
        }
    }
    Err(ContextPackPlanningError::FinalRepresentationExceeded)
}

fn evict_lowest_ranked_optional(
    planned: &mut PlannedContextPack,
) -> Result<bool, ContextPackServiceError> {
    if planned.continuation.is_none() && planned.continuation_frontier.is_none() {
        return Ok(false);
    }
    let Some(last) = planned.data.items.last() else {
        return Ok(false);
    };
    let mut coverage_entries = planned.data.role_coverage.roles().to_vec();
    let Some(coverage) = coverage_entries
        .iter_mut()
        .find(|entry| entry.role == last.role)
    else {
        return Err(ContextPackServiceError::InvalidResponse);
    };
    if coverage.requirement == RoleRequirement::Required && coverage.selected_items <= 1 {
        return Ok(false);
    }
    if planned.data.items.len() <= 1 || planned.page_identities.len() <= 1 {
        return Ok(false);
    }
    let role = last.role;
    let removed_tokens = last.tokens;
    planned.data.items.pop();
    planned.page_identities.pop();
    if let Some(state) = planned.continuation.as_mut() {
        state
            .retain_current_page(&planned.page_identities)
            .map_err(map_continuation_error)?;
    } else {
        let state = planned
            .continuation_frontier
            .as_mut()
            .ok_or(ContextPackServiceError::InvalidResponse)?
            .retain_current_page(&planned.page_identities)
            .map_err(map_continuation_error)?;
        planned.continuation = Some(state);
    }
    coverage.selected_items = coverage.selected_items.saturating_sub(1);
    if coverage.selected_items == 0 {
        coverage.status = RoleCoverageStatus::OptionalAbsent;
        coverage.missing_reason = None;
    }
    planned.data.role_coverage =
        RoleCoverageSummary::new(planned.data.role_coverage.objective(), coverage_entries)
            .map_err(|_| ContextPackServiceError::InvalidResponse)?;
    planned
        .data
        .structure
        .reading_order
        .truncate(planned.data.items.len());
    planned.usage.results = planned.usage.results.saturating_sub(1);
    planned.usage.tokens = planned
        .usage
        .tokens
        .saturating_sub(u64::from(removed_tokens));
    let reason = SafeLabel::parse("final_representation_budget")
        .map_err(|_| ContextPackServiceError::InvalidResponse)?;
    if let Some(omission) = planned
        .data
        .omitted
        .iter_mut()
        .find(|omission| omission.reason == reason && omission.role == Some(role))
    {
        omission.count = omission.count.saturating_add(1);
    } else {
        planned.data.omitted.push(OmissionSummary {
            role: Some(role),
            reason,
            provider: None,
            count: 1,
            limiting_resources: vec![LimitingResource::kind(
                LimitingResourceKind::EstimatedTokens,
            )],
            resumable: true,
            continuation: None,
        });
        planned.data.omitted.truncate(MAX_OMISSIONS);
    }
    planned.truncated = true;
    planned.completeness = resumable_completeness(&planned.completeness)
        .map_err(|_| ContextPackServiceError::InvalidResponse)?;
    Ok(true)
}

fn context_section_accounting(
    request: &CanonicalContextPackRequest,
    data: &ContextPackData,
) -> Result<BTreeMap<String, u32>, ContextPackPlanningError> {
    let mut by_section_bytes = BTreeMap::new();
    for section in request.sections() {
        by_section_bytes.insert(section_label(*section).to_owned(), 0_usize);
    }
    for item in &data.items {
        let item_bytes = serde_json::to_vec(item)
            .map_err(|_| ContextPackPlanningError::FinalRepresentationExceeded)?
            .len();
        let sections = item_sections(request.sections(), item.role);
        if sections.is_empty() {
            add_section_bytes(&mut by_section_bytes, "unclassified_items", item_bytes);
            continue;
        }
        let divisor = sections.len().max(1);
        let share = item_bytes / divisor;
        let remainder = item_bytes % divisor;
        for (index, section) in sections.into_iter().enumerate() {
            add_section_bytes(
                &mut by_section_bytes,
                section_label(section),
                share.saturating_add(usize::from(index == 0).saturating_mul(remainder)),
            );
        }
    }

    add_serialized_section_bytes(
        &mut by_section_bytes,
        "role_coverage",
        serde_json::to_vec(&data.role_coverage).map_or(usize::MAX, |encoded| encoded.len()),
    );
    add_serialized_section_bytes(
        &mut by_section_bytes,
        "structure",
        serde_json::to_vec(&data.structure).map_or(usize::MAX, |encoded| encoded.len()),
    );
    add_serialized_section_bytes(
        &mut by_section_bytes,
        "omissions",
        serde_json::to_vec(&data.omitted).map_or(usize::MAX, |encoded| encoded.len()),
    );
    add_serialized_section_bytes(
        &mut by_section_bytes,
        "followups",
        serde_json::to_vec(&data.followups).map_or(usize::MAX, |encoded| encoded.len()),
    );
    if let Some(explanation) = &data.explanation {
        add_serialized_section_bytes(
            &mut by_section_bytes,
            "explanation",
            serde_json::to_vec(explanation).map_or(usize::MAX, |encoded| encoded.len()),
        );
    }
    let mut prefix_bytes = 0_usize;
    let mut prefix_tokens = 0_u64;
    let mut by_section = BTreeMap::new();
    for (section, bytes) in by_section_bytes {
        prefix_bytes = prefix_bytes.saturating_add(bytes);
        let next_tokens = rootlight_mcp_contract::accounting::estimate_tokens(prefix_bytes);
        by_section.insert(
            section,
            u32::try_from(next_tokens.saturating_sub(prefix_tokens)).unwrap_or(u32::MAX),
        );
        prefix_tokens = next_tokens;
    }
    Ok(by_section)
}

fn add_serialized_section_bytes(
    accounting: &mut BTreeMap<String, usize>,
    section: &str,
    bytes: usize,
) {
    add_section_bytes(accounting, section, bytes);
}

fn add_section_bytes(accounting: &mut BTreeMap<String, usize>, section: &str, bytes: usize) {
    let entry = accounting.entry(section.to_owned()).or_default();
    *entry = entry.saturating_add(bytes);
}

fn item_sections(requested: &[ContextSection], role: ContractEvidenceRole) -> Vec<ContextSection> {
    requested
        .iter()
        .copied()
        .filter(|section| match role {
            ContractEvidenceRole::Definition => {
                matches!(section, ContextSection::Definitions | ContextSection::Types)
            }
            ContractEvidenceRole::Implementation => *section == ContextSection::Source,
            ContractEvidenceRole::Caller => {
                matches!(section, ContextSection::Callers | ContextSection::Callees)
            }
            ContractEvidenceRole::Test => *section == ContextSection::Tests,
            ContractEvidenceRole::Risk => *section == ContextSection::Risks,
            ContractEvidenceRole::Architecture => *section == ContextSection::Architecture,
            ContractEvidenceRole::Change => *section == ContextSection::History,
        })
        .collect()
}

const fn section_label(section: ContextSection) -> &'static str {
    match section {
        ContextSection::Architecture => "architecture",
        ContextSection::Definitions => "definitions",
        ContextSection::Callers => "callers",
        ContextSection::Callees => "callees",
        ContextSection::Types => "types",
        ContextSection::Tests => "tests",
        ContextSection::History => "history",
        ContextSection::Source => "source",
        ContextSection::Risks => "risks",
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
        let provider = SafeLabel::parse(provider_omission.provider.name()).ok();
        let role = Some(contract_role(provider_omission.role));
        if let Some(existing) = omitted.iter_mut().find(|value| {
            value.reason == reason
                && value.provider == provider
                && value.role == role
                && value.limiting_resources == provider_omission.limiting_resources
                && !value.resumable
        }) {
            existing.count = existing.count.saturating_add(provider_omission.count);
        } else if omitted.len() < MAX_OMISSIONS {
            omitted.push(OmissionSummary {
                role,
                reason,
                provider,
                count: provider_omission.count,
                limiting_resources: provider_omission.limiting_resources.clone(),
                resumable: false,
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
    use std::{
        collections::BTreeSet,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use super::{
        ContextEvidencePlanningError, ContextPackPlanRequest, ContextPackPlanner,
        ContextPackPlanningError, DefaultContextPackPlanner, EvidenceCandidate, EvidenceRole,
        MAX_PACK_ITEMS, MAX_PACK_TOKENS, MIN_PACK_TOKENS, PackError, PackObjective,
        SourceMaterializationPlan, append_role_followups, context_pack_completeness,
        context_pack_id, contract_role, evaluate_role_coverage, objective_for_task,
        optimize_admitted_pack, optimize_context_page, optimize_pack, optimize_pack_with_diversity,
        reconcile_final_envelope, reconcile_final_envelope_with_budget, resumable_completeness,
        role_coverage_completeness, selected_source_targets, source_materialization_limits,
        usage_summary, validate_source_output,
    };
    use crate::{
        context_evidence::{
            ContextEvidenceCallContext, ContextEvidenceCollectionError, ContextEvidenceCollector,
            ContextEvidencePort, ContextEvidencePortError, ContextEvidencePortErrorKind,
            ContextEvidenceProviderRegistry, ContextSourceMaterial,
            ContextSourceMetadataQuoteOutput, ContextSourceMetadataQuoteRequest,
            ContextSourceOutput, ContextSourceProjection, ContextSourceRequest,
            ContextSourceSnippet, ContextSourceTarget, EvidenceCandidateDraft, EvidenceProvenance,
            EvidenceProvider, EvidenceProviderInvocation, EvidenceProviderObservation,
            EvidenceProviderOmission, EvidenceProviderOmissionReason, EvidenceProviderOutput,
            TypedEvidenceCandidate,
        },
        context_pack_request::CanonicalContextPackRequest,
        policy::{
            BudgetCharge, BudgetResource, CancellationSignal, ExecutionPolicyError, NeverCancelled,
        },
        port::AgentPortFuture,
    };
    use proptest::prelude::*;
    use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_ir::{CoverageStatus, LineRange, SourceRef, SourceSpan};
    use rootlight_mcp_contract::{
        RepositorySelector, SchemaVersion, TrustClassification,
        completeness::{CompletenessState, LimitingResourceKind, ResultCompleteness},
        context::{
            ContextPackInput, ContextSection, ContextSeedSelector, Diversity,
            MissingRequiredRoleReason, RoleCoverageStatus, SourcePolicy,
        },
        vertical::{
            CoverageSummary, EntityKind, Freshness, GenerationSummary, ReadEnvelope,
            RelationSummary, RepositoryIdSelector, RequiredNullable, ResolvedRepository,
            ResponseProfile, SymbolExplanation,
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
            provider_key: "fixture".to_owned(),
            source_region: 0,
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct ProfileSourcePort;

    impl<C> ContextEvidencePort<C> for ProfileSourcePort
    where
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        fn retrieve(
            &self,
            invocation: EvidenceProviderInvocation,
            _context: ContextEvidenceCallContext<C>,
        ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
            let file_byte = invocation.role().priority().saturating_add(20);
            let source_ref = SourceRef::new(
                invocation.repository(),
                invocation.generation(),
                SourceSpan::new(FileId::from_bytes([file_byte; 20]), 0, 8_192)
                    .expect("fixture source span"),
                ContentHash::from_bytes([file_byte; 32]),
                Some(LineRange::new(1, 256).expect("fixture line hint")),
            );
            // A single materializable item isolates response-profile shaping
            // from the independent target-count tradeoff in the shared budget.
            let source_refs = if invocation.role() == EvidenceRole::Definition {
                vec![source_ref]
            } else {
                Vec::new()
            };
            let output = EvidenceProviderOutput {
                repository: invocation.repository(),
                generation: invocation.generation(),
                invocation: invocation.id().clone(),
                observations: vec![EvidenceProviderObservation {
                    kind: crate::context_evidence::EvidenceProviderObservationKind::Primary,
                    symbol_id: None,
                    identity: format!("profile-role-{}", invocation.role().priority()),
                    observed_score: Some(900),
                    observed_relevance: None,
                    estimated_tokens: 1,
                    source_bytes: 0,
                    source_refs,
                }],
                completeness: ResultCompleteness::complete(),
                usage: BudgetCharge {
                    results: 1,
                    tokens: 1,
                    ..BudgetCharge::default()
                },
            };
            Box::pin(async move { Ok(output) })
        }

        fn quote_source_metadata(
            &self,
            request: ContextSourceMetadataQuoteRequest,
            _context: ContextEvidenceCallContext<C>,
        ) -> AgentPortFuture<Result<ContextSourceMetadataQuoteOutput, ContextEvidencePortError>>
        {
            let output = super::source_metadata::tests::fixture_quote(&request);
            Box::pin(async move { Ok(output) })
        }

        fn materialize_source(
            &self,
            request: ContextSourceRequest,
            _context: ContextEvidenceCallContext<C>,
        ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
            let bytes = usize::try_from(request.max_bytes_per_snippet)
                .expect("fixture source cap fits usize");
            let materials = request
                .targets
                .iter()
                .map(|target| ContextSourceMaterial {
                    candidate_id: target.candidate_id.clone(),
                    source_ref: target.source_ref.clone(),
                    projection: ContextSourceProjection {
                        start_byte: target.source_ref.span().start_byte(),
                        end_byte: target.source_ref.span().start_byte()
                            + if request.include_snippets {
                                u64::from(request.max_bytes_per_snippet)
                            } else {
                                20
                            },
                        encoding: None,
                        text_bytes: if request.include_snippets {
                            u64::from(request.max_bytes_per_snippet)
                        } else {
                            20
                        },
                    },
                    signature: Some("fn profile_fixture()".to_owned()),
                    snippet: request.include_snippets.then(|| ContextSourceSnippet {
                        content: format!("fn profile_fixture()\n{}", "x".repeat(bytes - 21)),
                        language: "rust".to_owned(),
                        truncated: true,
                    }),
                })
                .collect::<Vec<_>>();
            let total_bytes = if request.include_snippets {
                u64::from(request.max_bytes_per_snippet)
            } else {
                20
            }
            .saturating_mul(u64::try_from(materials.len()).unwrap_or(u64::MAX));
            let output = ContextSourceOutput {
                repository: request.repository,
                generation: request.generation,
                materials,
                completeness: ResultCompleteness::complete(),
                usage: BudgetCharge {
                    results: u64::try_from(request.targets.len()).unwrap_or(u64::MAX),
                    tokens: total_bytes,
                    source_bytes: total_bytes,
                    memory_bytes: total_bytes,
                    ..BudgetCharge::default()
                },
            };
            Box::pin(async move { Ok(output) })
        }
    }

    #[derive(Debug, Clone, Default)]
    struct SourceReceiptCancellation(Arc<AtomicBool>);

    impl CancellationSignal for SourceReceiptCancellation {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Debug)]
    struct InterruptedSourcePort {
        cancellation: SourceReceiptCancellation,
        source_calls: Arc<AtomicUsize>,
        failed_response: bool,
        overage: bool,
        interrupt_after: usize,
    }

    impl ContextEvidencePort<SourceReceiptCancellation> for InterruptedSourcePort {
        fn quote_source_metadata(
            &self,
            request: ContextSourceMetadataQuoteRequest,
            context: ContextEvidenceCallContext<SourceReceiptCancellation>,
        ) -> AgentPortFuture<Result<ContextSourceMetadataQuoteOutput, ContextEvidencePortError>>
        {
            ProfileSourcePort.quote_source_metadata(request, context)
        }

        fn retrieve(
            &self,
            invocation: EvidenceProviderInvocation,
            context: ContextEvidenceCallContext<SourceReceiptCancellation>,
        ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
            if self.interrupt_after == 1 {
                return ProfileSourcePort.retrieve(invocation, context);
            }
            let role = invocation.role();
            let file_byte = role.priority().saturating_add(20);
            let source = SourceRef::new(
                invocation.repository(),
                invocation.generation(),
                SourceSpan::new(FileId::from_bytes([file_byte; 20]), 0, 8_192).unwrap(),
                ContentHash::from_bytes([file_byte; 32]),
                None,
            );
            let response = ProfileSourcePort.retrieve(invocation, context);
            Box::pin(async move {
                let mut output = response.await?;
                for observation in &mut output.observations {
                    observation.source_refs = vec![source.clone()];
                }
                Ok(output)
            })
        }

        fn materialize_source(
            &self,
            request: ContextSourceRequest,
            context: ContextEvidenceCallContext<SourceReceiptCancellation>,
        ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
            let interrupt =
                self.source_calls.fetch_add(1, Ordering::SeqCst) + 1 == self.interrupt_after;
            let reserved_rows = context.reservation().rows;
            let response = ProfileSourcePort.materialize_source(request, context);
            let cancellation = self.cancellation.clone();
            let failed_response = self.failed_response;
            let overage = self.overage;
            Box::pin(async move {
                let mut output = response.await?;
                if !interrupt {
                    return Ok(output);
                }
                if overage {
                    output.usage.rows = reserved_rows + 1;
                }
                cancellation.0.store(true, Ordering::SeqCst);
                if failed_response {
                    Err(ContextEvidencePortError {
                        kind: ContextEvidencePortErrorKind::Unavailable,
                        usage: output.usage,
                    })
                } else {
                    Ok(output)
                }
            })
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct UnsupportedEvidencePort;

    #[derive(Debug)]
    struct DefaultQuoteSourcePort(Arc<AtomicUsize>);

    #[tokio::test]
    async fn source_metadata_short_signature_does_not_retain_previous_capacity() {
        let mut input = context_input(SymbolId::from_bytes([3; 20]));
        input.token_budget = 20_000;
        input.source_policy = Some(SourcePolicy::Signatures);
        let request = CanonicalContextPackRequest::new(
            &input,
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let registry = ContextEvidenceProviderRegistry.plan(&request).unwrap();
        let corpus = ContextEvidenceCollector
            .collect(
                &ProfileSourcePort,
                &request,
                &registry,
                NeverCancelled,
                deadline,
            )
            .await
            .unwrap();
        let mut planned = DefaultContextPackPlanner
            .plan_corpus(&request, &corpus, &NeverCancelled)
            .unwrap();
        let selected = selected_source_targets(&planned, &corpus);
        assert_eq!(selected.len(), 1);
        let target = selected[0].target.clone();
        let mut old = String::with_capacity(2048);
        old.push_str(&"s".repeat(1024));
        planned.data.items[selected[0].item_index].signature = Some(old);
        let source_request = ContextSourceRequest {
            repository: request.repository(),
            generation: request.generation(),
            source_policy: SourcePolicy::Signatures,
            include_snippets: false,
            max_bytes_per_snippet: 64,
            targets: vec![target.clone()],
        };
        let output = ProfileSourcePort
            .materialize_source(
                source_request.clone(),
                ContextEvidenceCallContext::new(NeverCancelled, deadline, BudgetCharge::default()),
            )
            .await
            .unwrap();
        validate_source_output(&source_request, &output).unwrap();
        let fixed = super::source_metadata::shaping_fixed(&target.source_ref, false).unwrap();
        let shaping =
            super::apply_source_materials(&mut planned, &selected, &output.materials).unwrap();
        let signature = planned.data.items[selected[0].item_index]
            .signature
            .as_ref()
            .unwrap();
        assert_eq!(signature, "fn profile_fixture()");
        assert_eq!(
            shaping.memory_bytes,
            fixed.memory_bytes + u64::try_from(signature.capacity()).unwrap()
        );
        assert!(shaping.memory_bytes <= fixed.memory_bytes + 2 * 64);
        assert_eq!(
            planned.data.items[selected[0].item_index]
                .source_ref
                .as_ref(),
            Some(&target.source_ref)
        );
    }

    #[tokio::test]
    async fn source_metadata_default_unsupported_preserves_selected_full_refs_without_read() {
        let mut input = context_input(SymbolId::from_bytes([3; 20]));
        input.token_budget = 20_000;
        input.source_policy = Some(SourcePolicy::FocusedSnippets);
        let request = CanonicalContextPackRequest::new(
            &input,
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
        )
        .unwrap();
        let provider_plan = ContextEvidenceProviderRegistry.plan(&request).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let corpus = ContextEvidenceCollector
            .collect(
                &ProfileSourcePort,
                &request,
                &provider_plan,
                NeverCancelled,
                deadline,
            )
            .await
            .unwrap();
        let before = DefaultContextPackPlanner
            .plan_corpus(&request, &corpus, &NeverCancelled)
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let planned = DefaultContextPackPlanner
            .collect_and_plan(
                &DefaultQuoteSourcePort(Arc::clone(&calls)),
                &request,
                NeverCancelled,
                deadline,
            )
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(planned.data.items, before.data.items);
        assert_eq!(planned.usage, before.usage);
        assert!(
            planned
                .data
                .items
                .iter()
                .any(|item| item.source_ref.is_some())
        );
        assert!(
            before
                .data
                .items
                .iter()
                .any(|item| { item.source_ref.is_none() && item.signature.is_some() }),
            "the fixture retains first-stage identity presentation"
        );
        assert!(planned.data.items.iter().all(|item| item.snippet.is_none()));
        let before_targets = selected_source_targets(&before, &corpus)
            .into_iter()
            .map(|selected| selected.target)
            .collect::<Vec<_>>();
        let after_targets = selected_source_targets(&planned, &corpus)
            .into_iter()
            .map(|selected| selected.target)
            .collect::<Vec<_>>();
        assert!(!before_targets.is_empty());
        assert_eq!(
            after_targets, before_targets,
            "unsupported quotes preserve candidate identity and full approved references"
        );
        assert_eq!(
            planned.data.structure.reading_order,
            before.data.structure.reading_order
        );
        assert!(
            planned
                .data
                .omitted
                .iter()
                .any(|omission| omission.reason.as_str() == "source_metadata_unsupported")
        );
        assert_ne!(planned.completeness.state, CompletenessState::Complete);
    }

    impl ContextEvidencePort<NeverCancelled> for DefaultQuoteSourcePort {
        fn retrieve(
            &self,
            invocation: EvidenceProviderInvocation,
            context: ContextEvidenceCallContext<NeverCancelled>,
        ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
            ProfileSourcePort.retrieve(invocation, context)
        }

        fn materialize_source(
            &self,
            _request: ContextSourceRequest,
            _context: ContextEvidenceCallContext<NeverCancelled>,
        ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(ContextEvidencePortError {
                    kind: ContextEvidencePortErrorKind::Unavailable,
                    usage: BudgetCharge::default(),
                })
            })
        }
    }

    impl<C> ContextEvidencePort<C> for UnsupportedEvidencePort
    where
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        fn retrieve(
            &self,
            _invocation: EvidenceProviderInvocation,
            _context: ContextEvidenceCallContext<C>,
        ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
            Box::pin(async {
                Err(ContextEvidencePortError {
                    kind: ContextEvidencePortErrorKind::Unsupported,
                    usage: BudgetCharge::default(),
                })
            })
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
            response_profile: None,
            continuation: None,
            explain: None,
        }
    }

    fn explanation(symbol: SymbolId, generation: GenerationId) -> SymbolExplanation {
        SymbolExplanation {
            symbol_id: symbol,
            kind: EntityKind::Function,
            display_name: "parse_request".to_owned(),
            qualified_name: Some("crate::parse_request".to_owned()),
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
            container: None,
            relation_samples: Vec::new(),
            source_preview: None,
            provenance: Vec::new(),
            confidence: 900,
            uncertainty: Vec::new(),
            section_gaps: Vec::new(),
            trust: TrustClassification::UntrustedRepositoryData,
        }
    }

    #[tokio::test]
    async fn source_materialization_interruption_admits_received_work_before_cancellation() {
        for failed_response in [false, true] {
            let mut input = context_input(SymbolId::from_bytes([3; 20]));
            input.token_budget = 20_000;
            input.source_policy = Some(SourcePolicy::Signatures);
            let request = CanonicalContextPackRequest::new(
                &input,
                RepositoryId::from_bytes([1; 16]),
                GenerationId::from_bytes([2; 20]),
            )
            .unwrap();
            let cancellation = SourceReceiptCancellation::default();
            let source_calls = Arc::new(AtomicUsize::new(0));
            let port = InterruptedSourcePort {
                cancellation: cancellation.clone(),
                source_calls: Arc::clone(&source_calls),
                failed_response,
                overage: true,
                interrupt_after: 1,
            };

            let result = DefaultContextPackPlanner
                .collect_and_plan(
                    &port,
                    &request,
                    cancellation,
                    Instant::now() + Duration::from_secs(1),
                )
                .await;

            assert_eq!(
                result,
                Err(ContextEvidencePlanningError::Planning(
                    ContextPackPlanningError::Policy(ExecutionPolicyError::BudgetExceeded {
                        resource: BudgetResource::Rows,
                    }),
                )),
            );
            assert_eq!(source_calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn source_materialization_interruption_preserves_typed_stop_after_receipt() {
        for failed_response in [false, true] {
            let mut input = context_input(SymbolId::from_bytes([3; 20]));
            input.token_budget = 20_000;
            input.source_policy = Some(SourcePolicy::Signatures);
            let request = CanonicalContextPackRequest::new(
                &input,
                RepositoryId::from_bytes([1; 16]),
                GenerationId::from_bytes([2; 20]),
            )
            .unwrap();
            let cancellation = SourceReceiptCancellation::default();
            let source_calls = Arc::new(AtomicUsize::new(0));
            let port = InterruptedSourcePort {
                cancellation: cancellation.clone(),
                source_calls: Arc::clone(&source_calls),
                failed_response,
                overage: false,
                interrupt_after: 1,
            };

            let result = DefaultContextPackPlanner
                .collect_and_plan(
                    &port,
                    &request,
                    cancellation,
                    Instant::now() + Duration::from_secs(1),
                )
                .await;

            assert_eq!(
                result,
                Err(ContextEvidencePlanningError::Collection(
                    ContextEvidenceCollectionError::Cancelled,
                )),
            );
            assert_eq!(source_calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn second_source_batch_admits_receipt_before_preserving_cancellation() {
        for (failed_response, overage) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut input = context_input(SymbolId::from_bytes([3; 20]));
            input.token_budget = 8_000;
            input.source_policy = Some(SourcePolicy::Signatures);
            let request = CanonicalContextPackRequest::new(
                &input,
                RepositoryId::from_bytes([1; 16]),
                GenerationId::from_bytes([2; 20]),
            )
            .unwrap();
            let cancellation = SourceReceiptCancellation::default();
            let source_calls = Arc::new(AtomicUsize::new(0));
            let port = InterruptedSourcePort {
                cancellation: cancellation.clone(),
                source_calls: Arc::clone(&source_calls),
                failed_response,
                overage,
                interrupt_after: 2,
            };
            let result = DefaultContextPackPlanner
                .collect_and_plan(
                    &port,
                    &request,
                    cancellation,
                    Instant::now() + Duration::from_secs(1),
                )
                .await;
            let expected = if overage {
                ContextEvidencePlanningError::Planning(ContextPackPlanningError::Policy(
                    ExecutionPolicyError::BudgetExceeded {
                        resource: BudgetResource::Rows,
                    },
                ))
            } else {
                ContextEvidencePlanningError::Collection(ContextEvidenceCollectionError::Cancelled)
            };
            assert_eq!(result, Err(expected));
            assert_eq!(source_calls.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn source_materialization_refs_preserve_full_approved_span_and_line_hint() {
        let generation = GenerationId::from_bytes([2; 20]);
        let input = context_input(SymbolId::from_bytes([3; 20]));
        let request =
            CanonicalContextPackRequest::new(&input, RepositoryId::from_bytes([1; 16]), generation)
                .unwrap();
        let provider_plan = ContextEvidenceProviderRegistry.plan(&request).unwrap();
        let corpus = ContextEvidenceCollector
            .collect(
                &ProfileSourcePort,
                &request,
                &provider_plan,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .unwrap();
        let planned = DefaultContextPackPlanner
            .plan_corpus(&request, &corpus, &NeverCancelled)
            .unwrap();
        let selected = selected_source_targets(&planned, &corpus);
        assert_eq!(selected.len(), 1);
        let approved = corpus
            .candidates
            .iter()
            .flat_map(|candidate| candidate.source_refs())
            .find(|source| source.span().end_byte() == 8192)
            .unwrap();
        assert_eq!(&selected[0].target.source_ref, approved);
        assert_eq!(
            selected[0].target.source_ref.line_hint(),
            Some(LineRange::new(1, 256).unwrap())
        );
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

    #[tokio::test]
    async fn unsupported_providers_produce_a_truthful_empty_pack() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([5; 20]);
        let mut input = context_input(SymbolId::from_bytes([2; 20]));
        input.token_budget =
            u16::try_from(MAX_PACK_TOKENS).expect("maximum pack budget fits the public field");
        let canonical = CanonicalContextPackRequest::new(&input, repository, generation)
            .expect("fixture request canonicalizes");

        let planned = DefaultContextPackPlanner
            .collect_and_plan(
                &UnsupportedEvidencePort,
                &canonical,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("provider absence remains a truthful result");

        assert!(planned.data.items.is_empty());
        assert!(!planned.data.omitted.is_empty());
        assert!(planned.continuation.is_none());
        assert_eq!(
            planned.completeness.state,
            CompletenessState::UnsupportedPartial
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
        // BugFix requires Definition, Implementation, Caller, and Test.
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
            candidate("caller1", EvidenceRole::Caller, 450, 300),
            candidate("test1", EvidenceRole::Test, 400, 300),
        ];
        // Budget fits exactly one of each required role (4 * 300) but not all
        // six candidates.
        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 1_200).expect("valid pack");
        let roles: Vec<EvidenceRole> = result.items.iter().map(|i| i.candidate.role).collect();
        assert!(
            roles.contains(&EvidenceRole::Definition),
            "definition represented"
        );
        assert!(
            roles.contains(&EvidenceRole::Implementation),
            "implementation represented"
        );
        assert!(roles.contains(&EvidenceRole::Caller), "caller represented");
        assert!(roles.contains(&EvidenceRole::Test), "test represented");
        assert!(result.total_tokens <= 1_200, "budget respected");
    }

    #[test]
    fn required_role_reservations_survive_the_item_window() {
        let mut candidates = (0..=MAX_PACK_ITEMS)
            .map(|index| {
                candidate(
                    &format!("definition-{index:03}"),
                    EvidenceRole::Definition,
                    900,
                    100,
                )
            })
            .collect::<Vec<_>>();
        candidates.extend([
            candidate("implementation", EvidenceRole::Implementation, 800, 100),
            candidate("caller", EvidenceRole::Caller, 800, 100),
            candidate("test", EvidenceRole::Test, 800, 100),
        ]);

        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 500).expect("valid pack");
        let roles = result
            .items
            .iter()
            .map(|item| item.candidate.role)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            roles,
            BTreeSet::from([
                EvidenceRole::Definition,
                EvidenceRole::Implementation,
                EvidenceRole::Caller,
                EvidenceRole::Test,
            ])
        );
        assert_eq!(result.items.len(), 5);
        assert_eq!(result.total_tokens, 500);
        assert!(result.truncated);
    }

    #[test]
    fn explicit_seed_anchors_precede_role_diversity() {
        let mut first = candidate("seed-first", EvidenceRole::Definition, 950, 250);
        first.source_path = "src/lib.rs".to_owned();
        first.source_region = 10;
        let mut second = candidate("seed-second", EvidenceRole::Definition, 940, 250);
        second.source_path = "src/lib.rs".to_owned();
        second.source_region = 20;
        let mut architecture = candidate("architecture", EvidenceRole::Architecture, 999, 250);
        architecture.source_path = "src/architecture.rs".to_owned();
        let mut candidates = vec![architecture, second, first];
        let anchors = BTreeSet::from(["seed-first".to_owned(), "seed-second".to_owned()]);

        let result = optimize_admitted_pack(
            PackObjective::Explanation,
            &mut candidates,
            500,
            Diversity::Architecture,
            &anchors,
        )
        .expect("explicit seed anchors fit exactly");
        let identities = result
            .items
            .iter()
            .map(|item| item.candidate.identity.as_str())
            .collect::<Vec<_>>();

        assert_eq!(identities, vec!["seed-first", "seed-second"]);
        assert_eq!(result.total_tokens, 500);
    }

    #[test]
    fn required_role_reservations_prioritize_relevance_in_one_file() {
        let mut definition_helper =
            candidate("definition-helper", EvidenceRole::Definition, 950, 200);
        definition_helper.source_path = "src/lib.rs".to_owned();
        definition_helper.source_region = 10;
        let mut definition_entry =
            candidate("definition-entry", EvidenceRole::Definition, 940, 200);
        definition_entry.source_path = "src/lib.rs".to_owned();
        definition_entry.source_region = 20;
        let mut implementation_helper = candidate(
            "implementation-helper",
            EvidenceRole::Implementation,
            950,
            200,
        );
        implementation_helper.source_path = "src/lib.rs".to_owned();
        implementation_helper.source_region = 10;
        let mut implementation_entry = candidate(
            "implementation-entry",
            EvidenceRole::Implementation,
            940,
            200,
        );
        implementation_entry.source_path = "src/lib.rs".to_owned();
        implementation_entry.source_region = 20;
        let mut caller = candidate("caller", EvidenceRole::Caller, 920, 200);
        caller.source_path = "src/lib.rs".to_owned();
        caller.source_region = 40;
        let mut test = candidate("test", EvidenceRole::Test, 900, 200);
        test.source_path = "tests/integration.rs".to_owned();
        test.source_region = 30;
        let mut candidates = vec![
            definition_helper,
            definition_entry,
            implementation_helper,
            implementation_entry,
            caller,
            test,
        ];

        let result =
            optimize_pack(PackObjective::BugFix, &mut candidates, 800).expect("valid pack");
        let identities = result
            .items
            .iter()
            .map(|item| item.candidate.identity.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            identities,
            vec![
                "definition-helper",
                "implementation-helper",
                "caller",
                "test"
            ],
            "required roles retain the highest-ranked evidence in a shared file"
        );
    }

    #[test]
    fn continuation_pages_are_deterministic_for_every_objective_without_duplicates() {
        let make_candidates = || {
            EvidenceRole::ALL
                .into_iter()
                .enumerate()
                .map(|(index, role)| {
                    candidate(
                        &format!("candidate-{index}"),
                        role,
                        900_u16.saturating_sub(u16::try_from(index).unwrap_or(u16::MAX)),
                        400,
                    )
                })
                .collect::<Vec<_>>()
        };
        let page_sequence = |objective| {
            let mut continuation = None;
            let mut sequence = Vec::new();
            for _ in 0..EvidenceRole::ALL.len() {
                let mut candidates = make_candidates();
                let page = optimize_context_page(
                    objective,
                    &mut candidates,
                    500,
                    Diversity::Balanced,
                    &BTreeSet::new(),
                    continuation,
                )
                .expect("page is valid");
                assert_eq!(page.pack.items.len(), 1);
                let identity = page.pack.items[0].candidate.identity.clone();
                assert!(!sequence.contains(&identity), "pages never repeat evidence");
                sequence.push(identity);
                continuation = page.continuation;
                if continuation.is_none() {
                    break;
                }
            }
            sequence
        };

        for objective in objectives() {
            let first = page_sequence(objective);
            let replay = page_sequence(objective);
            assert_eq!(first, replay, "{objective:?} has a stable page golden");
            assert_eq!(first.len(), EvidenceRole::ALL.len());
        }
    }

    #[test]
    fn resumable_frontier_preserves_non_resumable_partial_truth() {
        use rootlight_mcp_contract::completeness::{
            ContinuationAvailability, ContinuationGuidance, LimitingResource,
        };

        for state in [
            CompletenessState::UnsupportedPartial,
            CompletenessState::Indeterminate,
        ] {
            let current = ResultCompleteness::new(
                state,
                vec![LimitingResource::kind(LimitingResourceKind::Capability)],
                ContinuationAvailability::Unavailable,
                vec![ContinuationGuidance::UnsupportedNoContinuation],
            )
            .expect("partial fixture is valid");
            let resumed =
                resumable_completeness(&current).expect("mixed continuation truth is valid");
            assert_eq!(resumed.state, state);
            assert_eq!(resumed.continuation, ContinuationAvailability::Available);
            assert!(resumed.guidance.contains(&ContinuationGuidance::UseCursor));
            assert!(
                resumed
                    .guidance
                    .contains(&ContinuationGuidance::UnsupportedNoContinuation)
            );
        }
    }

    #[test]
    fn late_final_eviction_creates_the_first_truthful_frontier() {
        let mut candidates = vec![
            candidate("required", EvidenceRole::Definition, 900, 100),
            candidate("optional", EvidenceRole::Definition, 800, 100),
        ];
        let mut selection = optimize_context_page(
            PackObjective::Explanation,
            &mut candidates,
            500,
            Diversity::Balanced,
            &BTreeSet::new(),
            None,
        )
        .expect("all candidates initially fit");
        assert!(selection.continuation.is_none());
        let retained = vec![selection.pack.items[0].candidate.identity.clone()];
        let state = selection
            .frontier
            .retain_current_page(&retained)
            .expect("late eviction creates a continuation");
        assert_eq!(state.remaining_candidates(), 1);
        assert_eq!(state.emitted_count(), 1);
        assert_eq!(state.page_item_counts(), &[1]);
    }

    #[test]
    fn final_envelope_accounting_converges_at_the_exact_budget_boundary() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([5; 20]);
        let symbol = SymbolId::from_bytes([2; 20]);
        let mut input = context_input(symbol);
        input.token_budget = 20_000;
        let canonical = CanonicalContextPackRequest::new(&input, repository, generation)
            .expect("fixture request canonicalizes");
        let symbols = [explanation(symbol, generation)];
        let planned = DefaultContextPackPlanner
            .plan(
                ContextPackPlanRequest {
                    request: &canonical,
                    symbols: &symbols,
                },
                &NeverCancelled,
            )
            .expect("fixture pack plans");
        let mut envelope = ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: ResolvedRepository {
                repository_id: repository,
                display_name: "fixture".to_owned(),
            },
            generation: GenerationSummary {
                generation_id: generation,
                parent_generation: RequiredNullable(None),
                structural_freshness: Freshness::Current,
                semantic_freshness: Freshness::Current,
            },
            coverage: CoverageSummary {
                status: CoverageStatus::Bounded,
                languages: Vec::new(),
                skipped_inputs: 0,
            },
            data: planned.data,
            truncated: planned.truncated,
            completeness: planned.completeness,
            next_cursor: RequiredNullable(None),
            usage: usage_summary(planned.usage, "accounting-boundary"),
            warnings: Vec::new(),
            trust: TrustClassification::UntrustedRepositoryData,
        };
        reconcile_final_envelope(&canonical, &mut envelope).expect("generous envelope converges");
        let exact_budget =
            u16::try_from(envelope.usage.estimated_tokens).expect("fixture fits u16");
        let section_total = envelope
            .data
            .token_accounting
            .by_section
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add);
        assert!(envelope.data.token_accounting.by_section.len() >= 10);
        assert_eq!(
            section_total,
            envelope.data.token_accounting.estimated_total
        );
        assert_eq!(u64::from(section_total), envelope.usage.estimated_tokens);

        let mut exact = envelope.clone();
        reconcile_final_envelope_with_budget(&canonical, &mut exact, exact_budget)
            .expect("exact measured boundary is accepted");
        let mut below = envelope;
        assert_eq!(
            reconcile_final_envelope_with_budget(
                &canonical,
                &mut below,
                exact_budget.saturating_sub(1),
            ),
            Err(ContextPackPlanningError::FinalRepresentationExceeded)
        );
    }

    #[test]
    fn diversity_bias_changes_optional_selection_without_displacing_required_roles() {
        let make_candidates = || {
            vec![
                candidate("definition", EvidenceRole::Definition, 900, 100),
                candidate("architecture", EvidenceRole::Architecture, 900, 100),
                candidate("implementation", EvidenceRole::Implementation, 900, 300),
                candidate("test", EvidenceRole::Test, 900, 300),
            ]
        };
        let mut balanced = make_candidates();
        let balanced = optimize_pack_with_diversity(
            PackObjective::Explanation,
            &mut balanced,
            500,
            Diversity::Balanced,
        )
        .expect("balanced pack");
        let mut tests = make_candidates();
        let tests = optimize_pack_with_diversity(
            PackObjective::Explanation,
            &mut tests,
            500,
            Diversity::Tests,
        )
        .expect("test-biased pack");

        assert!(
            balanced
                .items
                .iter()
                .any(|item| item.candidate.role == EvidenceRole::Implementation)
        );
        assert!(
            tests
                .items
                .iter()
                .any(|item| item.candidate.role == EvidenceRole::Test)
        );
        for result in [&balanced, &tests] {
            assert!(
                result
                    .items
                    .iter()
                    .any(|item| { item.candidate.role == EvidenceRole::Definition })
            );
            assert!(
                result
                    .items
                    .iter()
                    .any(|item| { item.candidate.role == EvidenceRole::Architecture })
            );
        }
    }

    proptest! {
        #[test]
        fn every_diversity_policy_preserves_fitting_required_roles(
            definition_relevance in 0u16..=1_000,
            architecture_relevance in 0u16..=1_000,
            optional_relevance in 0u16..=1_000,
        ) {
            for diversity in [
                Diversity::Balanced,
                Diversity::Implementation,
                Diversity::Tests,
                Diversity::Impact,
                Diversity::Architecture,
            ] {
                let mut candidates = vec![
                    candidate("definition", EvidenceRole::Definition, definition_relevance, 100),
                    candidate("architecture", EvidenceRole::Architecture, architecture_relevance, 100),
                    candidate("optional", EvidenceRole::Risk, optional_relevance, 300),
                ];
                let result = optimize_pack_with_diversity(
                    PackObjective::Explanation,
                    &mut candidates,
                    500,
                    diversity,
                )
                .expect("property pack");
                let has_definition = result
                    .items
                    .iter()
                    .any(|item| item.candidate.role == EvidenceRole::Definition);
                let has_architecture = result
                    .items
                    .iter()
                    .any(|item| item.candidate.role == EvidenceRole::Architecture);
                prop_assert!(has_definition);
                prop_assert!(has_architecture);
            }
        }
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
    fn deduplication_limits_optional_same_path_items() {
        let mut candidates = vec![
            EvidenceCandidate {
                identity: "definition".to_owned(),
                role: EvidenceRole::Definition,
                relevance: 900,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/definition.rs".to_owned(),
                provider_key: "fixture".to_owned(),
                source_region: 0,
            },
            EvidenceCandidate {
                identity: "architecture".to_owned(),
                role: EvidenceRole::Architecture,
                relevance: 900,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/architecture.rs".to_owned(),
                provider_key: "fixture".to_owned(),
                source_region: 0,
            },
            EvidenceCandidate {
                identity: "implementation".to_owned(),
                role: EvidenceRole::Implementation,
                relevance: 850,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
                provider_key: "fixture".to_owned(),
                source_region: 1,
            },
            EvidenceCandidate {
                identity: "caller".to_owned(),
                role: EvidenceRole::Caller,
                relevance: 800,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
                provider_key: "fixture".to_owned(),
                source_region: 2,
            },
            EvidenceCandidate {
                identity: "test".to_owned(),
                role: EvidenceRole::Test,
                relevance: 750,
                confidence: 800,
                estimated_tokens: 100,
                source_path: "src/shared.rs".to_owned(),
                provider_key: "fixture".to_owned(),
                source_region: 3,
            },
        ];
        let result =
            optimize_pack(PackObjective::Explanation, &mut candidates, 5_000).expect("valid pack");
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
                    EvidenceRole::Caller,
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
        assert_eq!(followups.len(), 3);
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
        assert!(
            followups
                .iter()
                .any(|followup| followup.tool == "symbol.relationships")
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

    #[test]
    fn source_modes_enforce_body_policy_and_authoritative_byte_accounting() {
        assert_eq!(
            source_materialization_limits(SourcePolicy::EvidenceHeavy, ResponseProfile::Compact),
            (false, 1_024)
        );
        assert_eq!(
            source_materialization_limits(SourcePolicy::EvidenceHeavy, ResponseProfile::Standard),
            (true, 4_096)
        );
        assert_eq!(
            source_materialization_limits(SourcePolicy::EvidenceHeavy, ResponseProfile::Evidence),
            (true, 8_192)
        );

        let symbol = SymbolId::from_bytes([2; 20]);
        let generation = GenerationId::from_bytes([5; 20]);
        let public = context_input(symbol);
        let canonical = CanonicalContextPackRequest::new(
            &public,
            RepositoryId::from_bytes([1; 16]),
            generation,
        )
        .expect("fixture request canonicalizes");
        let invocation = ContextEvidenceProviderRegistry
            .plan(&canonical)
            .expect("provider plan")
            .invocations()[0]
            .clone();
        let source_ref = explanation(symbol, generation).definition;
        let candidate = TypedEvidenceCandidate::from_draft(
            canonical.repository(),
            canonical.generation(),
            EvidenceCandidateDraft {
                repository: canonical.repository(),
                generation: canonical.generation(),
                invocation: invocation.id().clone(),
                provider: invocation.provider(),
                role: invocation.role(),
                provenance: EvidenceProvenance::Graph,
                symbol_id: Some(symbol),
                identity: symbol.to_string(),
                relevance: 900,
                confidence: 900,
                cost: BudgetCharge {
                    results: 1,
                    tokens: 32,
                    ..BudgetCharge::default()
                },
                source_refs: vec![source_ref.clone()],
                dependencies: Vec::new(),
            },
        )
        .expect("candidate validates");
        let signature = "fn parse_request(input: &str)".to_owned();
        let signature_request = ContextSourceRequest {
            repository: canonical.repository(),
            generation: canonical.generation(),
            source_policy: SourcePolicy::Signatures,
            include_snippets: false,
            max_bytes_per_snippet: 1_024,
            targets: vec![ContextSourceTarget {
                candidate_id: candidate.id().clone(),
                source_ref: source_ref.clone(),
            }],
        };
        let signature_material = ContextSourceMaterial {
            candidate_id: candidate.id().clone(),
            source_ref: source_ref.clone(),
            projection: ContextSourceProjection {
                start_byte: source_ref.span().start_byte(),
                end_byte: source_ref.span().start_byte() + u64::try_from(signature.len()).unwrap(),
                encoding: None,
                text_bytes: u64::try_from(signature.len()).unwrap(),
            },
            signature: Some(signature.clone()),
            snippet: None,
        };
        let mut signature_output = ContextSourceOutput {
            repository: canonical.repository(),
            generation: canonical.generation(),
            materials: vec![signature_material.clone()],
            completeness: ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: 1,
                source_bytes: u64::try_from(signature.len()).expect("fixture length fits"),
                ..BudgetCharge::default()
            },
        };
        assert!(validate_source_output(&signature_request, &signature_output).is_ok());

        signature_output.usage.source_bytes = 0;
        assert_eq!(
            validate_source_output(&signature_request, &signature_output),
            Err(ContextEvidenceCollectionError::InvalidProviderResponse)
        );

        let mut forbidden_body = signature_output;
        forbidden_body.usage.source_bytes = 64;
        forbidden_body.materials[0].snippet = Some(ContextSourceSnippet {
            content: "fn parse_request() {}".to_owned(),
            language: "rust".to_owned(),
            truncated: false,
        });
        assert_eq!(
            validate_source_output(&signature_request, &forbidden_body),
            Err(ContextEvidenceCollectionError::InvalidProviderResponse)
        );

        let snippet = "fn parse_request() {}".to_owned();
        let focused_request = ContextSourceRequest {
            source_policy: SourcePolicy::FocusedSnippets,
            include_snippets: true,
            max_bytes_per_snippet: 2_048,
            ..signature_request
        };
        let focused_output = ContextSourceOutput {
            repository: canonical.repository(),
            generation: canonical.generation(),
            materials: vec![ContextSourceMaterial {
                signature: Some("fn parse_request() {".to_owned()),
                projection: ContextSourceProjection {
                    start_byte: source_ref.span().start_byte(),
                    end_byte: source_ref.span().start_byte()
                        + u64::try_from(snippet.len()).unwrap(),
                    encoding: None,
                    text_bytes: u64::try_from(snippet.len()).unwrap(),
                },
                snippet: Some(ContextSourceSnippet {
                    content: snippet.clone(),
                    language: "rust".to_owned(),
                    truncated: true,
                }),
                ..signature_material
            }],
            completeness: ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: 1,
                source_bytes: u64::try_from(snippet.len()).expect("fixture length fits"),
                ..BudgetCharge::default()
            },
        };
        assert!(validate_source_output(&focused_request, &focused_output).is_ok());

        let bounds = super::source_metadata::tests::fixture_bounds(2, true);
        let shaping = bounds.shaping(1, 2_048, true).unwrap();
        let metadata = super::source_metadata::shaping_fixed(
            &super::source_metadata::tests::fixture_request(1, true).targets[0].source_ref,
            true,
        )
        .unwrap();
        assert_eq!(
            shaping.json_bytes,
            2_048 * super::JSON_ESCAPE_EXPANSION
                + 2 * super::SOURCE_LANGUAGE_BYTES
                + metadata.json_bytes,
            "the full snippet, control-free language, and metadata are all reserved"
        );
        let provider = bounds.provider(1, 2_048).unwrap();
        assert_eq!(
            provider.tokens, provider.json_bytes,
            "provider tokens retain the authoritative UTF-8 byte upper bound"
        );
        let exact_capacity = bounds.combined(1, 2_048, true).unwrap();
        assert_eq!(
            bounds.affordable(exact_capacity, 2_048, true),
            SourceMaterializationPlan {
                target_count: 1,
                max_bytes_per_snippet: 2_048,
            }
        );
        let minimum_capacity = bounds
            .combined(1, super::MIN_SOURCE_MATERIAL_BYTES, true)
            .unwrap();
        assert_eq!(
            bounds.affordable(minimum_capacity, 2_048, true),
            SourceMaterializationPlan {
                target_count: 1,
                max_bytes_per_snippet: super::MIN_SOURCE_MATERIAL_BYTES,
            }
        );
        let two_target_capacity = bounds
            .combined(2, super::MIN_SOURCE_MATERIAL_BYTES, true)
            .unwrap();
        assert_eq!(
            bounds.affordable(two_target_capacity, 2_048, true),
            SourceMaterializationPlan {
                target_count: 2,
                max_bytes_per_snippet: super::MIN_SOURCE_MATERIAL_BYTES,
            },
            "snippet size shrinks before an explicit target is dropped"
        );
        let insufficient = BudgetCharge {
            tokens: minimum_capacity.tokens.saturating_sub(1),
            ..minimum_capacity
        };
        assert_eq!(
            bounds.affordable(insufficient, 2_048, true),
            SourceMaterializationPlan {
                target_count: 0,
                max_bytes_per_snippet: 0,
            }
        );

        let mut stale_output = focused_output;
        stale_output.generation = GenerationId::from_bytes([9; 20]);
        assert_eq!(
            validate_source_output(&focused_request, &stale_output),
            Err(ContextEvidenceCollectionError::InvalidProviderResponse)
        );
    }

    fn projection_fixture(
        text: &str,
        encoding: Option<rootlight_ir::SourceTextEncoding>,
        label: &str,
    ) -> (ContextSourceRequest, ContextSourceOutput) {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([5; 20]);
        let symbol = SymbolId::from_bytes([2; 20]);
        let canonical =
            CanonicalContextPackRequest::new(&context_input(symbol), repository, generation)
                .unwrap();
        let invocation = ContextEvidenceProviderRegistry
            .plan(&canonical)
            .unwrap()
            .invocations()[0]
            .clone();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(FileId::from_bytes([3; 20]), 2, 100).unwrap(),
            ContentHash::from_bytes([4; 32]),
            Some(LineRange::new(1, 4).unwrap()),
        );
        let candidate = TypedEvidenceCandidate::from_draft(
            repository,
            generation,
            EvidenceCandidateDraft {
                repository,
                generation,
                invocation: invocation.id().clone(),
                provider: invocation.provider(),
                role: invocation.role(),
                provenance: EvidenceProvenance::Graph,
                symbol_id: Some(symbol),
                identity: label.to_owned(),
                relevance: 900,
                confidence: 900,
                cost: BudgetCharge {
                    results: 1,
                    tokens: 1,
                    ..BudgetCharge::default()
                },
                source_refs: vec![source.clone()],
                dependencies: Vec::new(),
            },
        )
        .unwrap();
        let text_bytes = u64::try_from(text.len()).unwrap();
        let physical_bytes = if encoding.is_some() {
            u64::try_from(text.encode_utf16().count() * 2).unwrap()
        } else {
            text_bytes
        };
        let request = ContextSourceRequest {
            repository,
            generation,
            source_policy: SourcePolicy::FocusedSnippets,
            include_snippets: true,
            max_bytes_per_snippet: u32::try_from(text.len().max(1)).unwrap(),
            targets: vec![ContextSourceTarget {
                candidate_id: candidate.id().clone(),
                source_ref: source.clone(),
            }],
        };
        let output = ContextSourceOutput {
            repository,
            generation,
            materials: vec![ContextSourceMaterial {
                candidate_id: candidate.id().clone(),
                source_ref: source,
                projection: ContextSourceProjection {
                    start_byte: 2,
                    end_byte: 2 + physical_bytes,
                    encoding,
                    text_bytes,
                },
                signature: None,
                snippet: Some(ContextSourceSnippet {
                    content: text.to_owned(),
                    language: "rust".to_owned(),
                    truncated: true,
                }),
            }],
            completeness: ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: 1,
                source_bytes: physical_bytes,
                ..BudgetCharge::default()
            },
        };
        (request, output)
    }

    #[test]
    fn source_projection_charges_exact_physical_width_and_rejects_one_under() {
        for encoding in [
            None,
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            Some(rootlight_ir::SourceTextEncoding::Utf16Be),
        ] {
            for text in ["☃", "a", "🧭"] {
                let (request, mut output) = projection_fixture(text, encoding, "projection-fit");
                let expected = if encoding.is_some() {
                    text.encode_utf16().count() * 2
                } else {
                    text.len()
                };
                assert_eq!(output.usage.source_bytes, u64::try_from(expected).unwrap());
                assert!(validate_source_output(&request, &output).is_ok());
                output.usage.source_bytes -= 1;
                assert_eq!(
                    validate_source_output(&request, &output),
                    Err(ContextEvidenceCollectionError::InvalidProviderResponse)
                );
            }
        }
    }

    #[test]
    fn source_projection_deduplicates_work_only_for_identical_material() {
        let (mut request, mut output) = projection_fixture(
            "☃",
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            "first-site",
        );
        let (second_request, second_output) = projection_fixture(
            "☃",
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            "second-site",
        );
        assert_ne!(
            request.targets[0].candidate_id,
            second_request.targets[0].candidate_id
        );
        request.targets.extend(second_request.targets);
        output.materials.extend(second_output.materials);
        assert_eq!(output.usage.source_bytes, 2);
        assert_eq!(output.usage.results, 1);
        assert!(validate_source_output(&request, &output).is_ok());
        output.materials[1].snippet.as_mut().unwrap().content = "éx".to_owned();
        assert!(validate_source_output(&request, &output).is_err());
        output.materials[1] = output.materials[0].clone();
        output.materials[1].candidate_id = request.targets[1].candidate_id.clone();
        output.usage.source_bytes = 1;
        assert!(validate_source_output(&request, &output).is_err());
    }

    #[test]
    fn source_projection_rejects_blank_material_but_accepts_truthful_omission() {
        let (request, mut empty) = projection_fixture(
            "",
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            "empty-site",
        );
        assert!(validate_source_output(&request, &empty).is_err());
        empty.materials.clear();
        empty.usage = BudgetCharge::default();
        assert!(validate_source_output(&request, &empty).is_err());
        empty.completeness = super::snippet_truncation_completeness().unwrap();
        assert!(validate_source_output(&request, &empty).is_ok());
    }

    #[test]
    fn source_projection_rejects_visible_split_surrogate_endpoints() {
        for encoding in [
            rootlight_ir::SourceTextEncoding::Utf16Le,
            rootlight_ir::SourceTextEncoding::Utf16Be,
        ] {
            let (mut request, mut output) = projection_fixture("🧭", Some(encoding), "split-site");
            let source = &request.targets[0].source_ref;
            let split = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(source.span().file(), 2, 4).unwrap(),
                source.content_hash(),
                source.line_hint(),
            );
            request.targets[0].source_ref = split.clone();
            output.materials[0].source_ref = split;
            assert!(validate_source_output(&request, &output).is_err());
        }
    }

    #[test]
    fn source_language_bounds_cover_escaped_labels_and_reject_controls() {
        let (request, output) = projection_fixture("fn sample();", None, "escaped-language");
        for language in [
            "\"".repeat(64),
            "\\".repeat(64),
            "λ".repeat(32),
            "🧭".repeat(16),
        ] {
            let mut candidate = output.clone();
            candidate.materials[0].snippet.as_mut().unwrap().language = language.clone();
            assert!(validate_source_output(&request, &candidate).is_ok());
            assert!(serde_json::to_vec(&language).unwrap().len() - 2 <= 2 * language.len());
        }
        for language in ["\0", "\n", "\t", "\u{7f}", "\u{85}"] {
            let mut candidate = output.clone();
            candidate.materials[0].snippet.as_mut().unwrap().language = language.to_owned();
            assert!(validate_source_output(&request, &candidate).is_err());
        }
    }

    #[test]
    fn source_projection_signature_only_preserves_codec_and_text_bounds() {
        for encoding in [
            None,
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            Some(rootlight_ir::SourceTextEncoding::Utf16Be),
        ] {
            let (mut request, mut output) = projection_fixture("☃", encoding, "signature-site");
            request.source_policy = SourcePolicy::Signatures;
            request.include_snippets = false;
            output.materials[0].snippet = None;
            output.materials[0].signature = Some("☃".to_owned());
            assert!(validate_source_output(&request, &output).is_ok());
            output.materials[0].signature = Some("longer".to_owned());
            assert!(validate_source_output(&request, &output).is_err());
        }
    }

    #[test]
    fn source_projection_rejects_codec_width_and_text_count_lies() {
        for encoding in [
            rootlight_ir::SourceTextEncoding::Utf16Le,
            rootlight_ir::SourceTextEncoding::Utf16Be,
        ] {
            let (request, output) = projection_fixture("☃", Some(encoding), "codec-geometry");
            let mut tampered = output.clone();
            tampered.materials[0].projection.text_bytes += 1;
            assert!(validate_source_output(&request, &tampered).is_err());
            tampered = output.clone();
            tampered.materials[0].projection.end_byte += 2;
            tampered.usage.source_bytes += 2;
            assert!(
                validate_source_output(&request, &tampered).is_err(),
                "even alignment alone does not authenticate physical scalar width"
            );
            let (request, output) = projection_fixture("\0", Some(encoding), "utf16-nul");
            assert!(validate_source_output(&request, &output).is_err());
        }
        let (request, output) = projection_fixture("\0", None, "utf8-nul");
        assert!(validate_source_output(&request, &output).is_ok());
    }

    #[test]
    fn source_reservations_cover_physical_text_escape_and_transport_batches() {
        for targets in [1_u64, 32, 33, 64] {
            let bounds = super::source_metadata::tests::fixture_bounds(
                usize::try_from(targets).unwrap(),
                true,
            );
            let reservation = bounds
                .provider(usize::try_from(targets).unwrap(), 64)
                .unwrap();
            let text = targets * 64;
            let physical = 2 * text;
            let fixed = targets * 2048;
            let serialized = 6 * text + fixed;
            assert_eq!(reservation.source_bytes, physical);
            assert_eq!(reservation.json_bytes, serialized);
            assert_eq!(reservation.tokens, serialized);
            assert_eq!(
                reservation.memory_bytes,
                serialized + physical + text + fixed
            );
            assert_eq!(reservation.traversal_facts, targets * 10);
        }
        let controls = "\0\u{1}\u{1f}\"\\\n\r\t\u{8}\u{c}☃🧭";
        let serialized = serde_json::to_string(controls).unwrap();
        assert_eq!(
            super::json_string_content_bytes(controls),
            u64::try_from(serialized.len() - 2).unwrap()
        );
        assert!(
            super::json_string_content_bytes(controls)
                <= 6 * u64::try_from(controls.len()).unwrap()
        );
    }

    #[test]
    fn source_projection_signature_uses_visible_approved_selection_not_context() {
        let context = "prior();\r\n";
        let text = format!("{context}fn selected() {{\r\n🧭body\r\n}}");
        for encoding in [
            None,
            Some(rootlight_ir::SourceTextEncoding::Utf16Le),
            Some(rootlight_ir::SourceTextEncoding::Utf16Be),
        ] {
            let (mut request, mut output) =
                projection_fixture(&text, encoding, "context-signature");
            let previous = &request.targets[0].source_ref;
            let context_width = if encoding.is_some() {
                context.encode_utf16().count() * 2
            } else {
                context.len()
            };
            let approved = SourceRef::new(
                previous.repository(),
                previous.generation(),
                SourceSpan::new(
                    previous.span().file(),
                    2 + u64::try_from(context_width).unwrap(),
                    output.materials[0].projection.end_byte,
                )
                .unwrap(),
                previous.content_hash(),
                previous.line_hint(),
            );
            request.targets[0].source_ref = approved.clone();
            output.materials[0].source_ref = approved;
            output.materials[0].signature = Some("fn selected() {".to_owned());
            assert!(validate_source_output(&request, &output).is_ok());
            output.materials[0].signature = Some("prior();".to_owned());
            assert!(validate_source_output(&request, &output).is_err());
            output.materials[0].signature = None;
            let prefix = output.materials[0].snippet.as_mut().unwrap();
            prefix.content = context.to_owned();
            output.materials[0].projection.text_bytes = u64::try_from(context.len()).unwrap();
            output.materials[0].projection.end_byte =
                request.targets[0].source_ref.span().start_byte();
            assert!(
                validate_source_output(&request, &output).is_err(),
                "preceding-context-only material must be omitted rather than published"
            );
        }
    }

    #[tokio::test]
    async fn standard_and_evidence_profiles_preserve_identity_under_live_source_budget() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([5; 20]);
        let plan_for_profile = |profile| {
            let input = ContextPackInput {
                repository: RepositorySelector::ById(RepositoryIdSelector {
                    repository_id: repository,
                }),
                generation: None,
                task: "explain parser".to_owned(),
                seeds: ContextSeedSelector {
                    symbols: Some(vec![SymbolId::from_bytes([2; 20])]),
                    paths: None,
                    routes: None,
                    tests: None,
                    located: None,
                    change: None,
                    plan: None,
                },
                token_budget: 20_000,
                source_policy: Some(SourcePolicy::EvidenceHeavy),
                sections: Some(vec![
                    ContextSection::Definitions,
                    ContextSection::Architecture,
                    ContextSection::Source,
                ]),
                diversity: Some(Diversity::Balanced),
                min_confidence: Some(700),
                response_profile: Some(profile),
                continuation: None,
                explain: None,
            };
            CanonicalContextPackRequest::new(&input, repository, generation)
                .expect("profile request canonicalizes")
        };
        let standard_request = plan_for_profile(ResponseProfile::Standard);
        let evidence_request = plan_for_profile(ResponseProfile::Evidence);
        let registry = ContextEvidenceProviderRegistry
            .plan(&standard_request)
            .unwrap();
        let corpus = ContextEvidenceCollector
            .collect(
                &ProfileSourcePort,
                &standard_request,
                &registry,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("pre-source corpus is collected");
        let before_source = DefaultContextPackPlanner
            .plan_corpus(&standard_request, &corpus, &NeverCancelled)
            .expect("pre-source pack is planned");
        // The port declares its fixed transport bound; shaping counts the full
        // selected reference rather than guessing a path or metadata width.
        let target = before_source.data.items[0].source_ref.as_ref().unwrap();
        let metadata = super::source_metadata::shaping_fixed(target, true).unwrap();
        let fixed_tokens = 2048 + 2 * super::SOURCE_LANGUAGE_BYTES + metadata.json_bytes;
        let remaining_tokens = 20_000 - before_source.usage.tokens - 1;
        let expected_bytes = (remaining_tokens - fixed_tokens) / 12;
        assert!((64..4_096).contains(&expected_bytes));
        assert!(12 * (expected_bytes + 1) + fixed_tokens > remaining_tokens);
        assert_eq!(
            source_materialization_limits(SourcePolicy::EvidenceHeavy, ResponseProfile::Standard),
            (true, 4_096)
        );
        assert_eq!(
            source_materialization_limits(SourcePolicy::EvidenceHeavy, ResponseProfile::Evidence),
            (true, 8_192)
        );
        let standard = DefaultContextPackPlanner
            .collect_and_plan(
                &ProfileSourcePort,
                &standard_request,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("standard pack materializes");
        let evidence = DefaultContextPackPlanner
            .collect_and_plan(
                &ProfileSourcePort,
                &evidence_request,
                NeverCancelled,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("evidence pack materializes");

        let semantic_projection = |planned: &super::PlannedContextPack| {
            planned
                .data
                .items
                .iter()
                .map(|item| {
                    (
                        item.role,
                        item.symbol_id,
                        item.source_ref.clone(),
                        item.score,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            semantic_projection(&standard),
            semantic_projection(&evidence),
            "response profiles cannot change evidence identity or ranking"
        );
        let standard_snippet_bytes = standard.data.items[0]
            .snippet
            .as_ref()
            .expect("standard snippet")
            .content
            .len();
        let evidence_snippet_bytes = evidence.data.items[0]
            .snippet
            .as_ref()
            .expect("evidence snippet")
            .content
            .len();
        assert_eq!(
            u64::try_from(standard_snippet_bytes).unwrap(),
            expected_bytes
        );
        assert_eq!(
            evidence_snippet_bytes, standard_snippet_bytes,
            "both larger profile ceilings respect the same affordable source reservation"
        );
        assert_eq!(evidence.data.items[0].tokens, standard.data.items[0].tokens);
        for planned in [&standard, &evidence] {
            assert!(planned.data.items[0].snippet.as_ref().unwrap().truncated);
            assert!(planned.data.items[0].signature.is_none());
            assert!(
                planned.data.items[0]
                    .snippet
                    .as_ref()
                    .unwrap()
                    .content
                    .starts_with("fn profile_fixture()")
            );
            assert_eq!(
                planned.data.items[0]
                    .source_ref
                    .as_ref()
                    .unwrap()
                    .span()
                    .end_byte(),
                8_192
            );
        }
    }

    proptest! {
        #[test]
        fn control_free_language_labels_fit_two_byte_json_expansion(
            scalars in proptest::collection::vec(any::<char>().prop_filter("language labels reject controls", |scalar| !scalar.is_control()), 1..=16),
        ) {
            let language = scalars.into_iter().collect::<String>();
            let encoded = serde_json::to_vec(&language).unwrap();
            prop_assert!(encoded.len() - 2 <= language.len() * 2);
        }

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
