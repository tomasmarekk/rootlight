//! Transport-neutral normalization and shaping for `plan.change`.
//!
//! The MCP application supplies daemon facts through its client adapter. This
//! module owns request admission, source-free explain shaping, and conversion
//! of client-independent planning facts into the public contract.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Instant,
};

use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_mcp_contract::{
    ErrorCode, GenerationSelector, PublicError, RepositorySelector, SchemaVersion,
    TrustClassification,
    change::{
        ChangeImpactData, ChangePlanStep, ContextPackRequest, PlanChangeData, PlanChangeInput,
        PlanDecision, PlanEvidenceKind, PlanEvidenceOmission, PlanEvidenceOmissionReason,
        PlanEvidenceProvider, PlanEvidenceRecord, PlanImpactSummary, PlanObjective,
        PlanProviderCoverage, PlanProviderState, PlanTargetSelector, RiskLevel, TestCandidate,
        TestsSelectData,
    },
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    context::{BatchTool, PlanExplanation},
    intent::{ArchitectureOverviewData, SymbolRelationshipsData},
    vertical::{
        ReadEnvelope, RequiredNullable, ResponseBudget, ResponseProfile, ResponseWarning,
        UsageSummary,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use crate::{
    explain::{finalize_plan, plan_change_plan},
    policy::{BudgetCharge, BudgetLedger, CancellationSignal, ExecutionPolicyError},
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentResolutionContext,
        AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
};

const PLAN_PROVIDER_COUNT: usize = 7;
const PLAN_EVIDENCE_RECORD_LIMIT: usize = 64;

/// Admitted `plan.change` request independent of a concrete daemon client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangeRequest {
    repository: RepositoryId,
    generation: GenerationSelector,
    objective: PlanObjective,
    objective_text: String,
    target_symbols: Vec<SymbolId>,
    target_files: Vec<FileId>,
    max_steps: Option<u8>,
    budget: Option<ResponseBudget>,
    response_profile: ResponseProfile,
    explain_only: bool,
}

impl PlanChangeRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> &GenerationSelector {
        &self.generation
    }

    /// Returns the stable objective wire label expected by the daemon.
    #[must_use]
    pub const fn objective(&self) -> &'static str {
        objective_label(self.objective)
    }

    /// Returns the typed planning objective.
    #[must_use]
    pub const fn objective_kind(&self) -> PlanObjective {
        self.objective
    }

    /// Returns the concrete objective description.
    #[must_use]
    pub fn objective_text(&self) -> &str {
        &self.objective_text
    }

    /// Returns the explicit target symbol identifiers.
    #[must_use]
    pub fn target_symbols(&self) -> &[SymbolId] {
        &self.target_symbols
    }

    /// Returns the explicit target file identifiers.
    #[must_use]
    pub fn target_files(&self) -> &[FileId] {
        &self.target_files
    }

    /// Returns the optional plan-step ceiling.
    #[must_use]
    pub const fn max_steps(&self) -> Option<u8> {
        self.max_steps
    }

    /// Returns optional caller reductions for the shared execution budget.
    #[must_use]
    pub const fn budget(&self) -> Option<&ResponseBudget> {
        self.budget.as_ref()
    }

    /// Returns the requested final public response representation.
    #[must_use]
    pub const fn response_profile(&self) -> ResponseProfile {
        self.response_profile
    }

    /// Reports whether the request asks for planning metadata without retrieval.
    #[must_use]
    pub const fn explain_only(&self) -> bool {
        self.explain_only
    }

    /// Returns the total number of explicit targets.
    #[must_use]
    pub fn target_count(&self) -> usize {
        self.target_symbols
            .len()
            .saturating_add(self.target_files.len())
    }
}

/// Request-admission or response-shaping failure for `plan.change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanChangeError {
    /// A repository alias cannot be resolved at the agent boundary.
    #[error("repository selector is not supported")]
    UnsupportedRepository,
    /// One requested option is not implemented by the current planner.
    #[error("plan option is not supported")]
    UnsupportedOption,
    /// The request did not contain a resolvable target.
    #[error("plan requires at least one target")]
    EmptyTargets,
    /// The planning result contained an unknown risk label.
    #[error("plan result contains an invalid risk label")]
    InvalidRisk,
    /// The structural planner returned an invalid step ordering.
    #[error("plan result contains invalid ordered steps")]
    InvalidPlan,
}

/// Normalizes and admits one public `plan.change` request.
///
/// # Errors
///
/// Returns [`PlanChangeError`] when the request requires alias resolution,
/// unsupported context behavior or contains no symbol or file target.
pub fn normalize_plan_change(input: PlanChangeInput) -> Result<PlanChangeRequest, PlanChangeError> {
    let RepositorySelector::ById(repository) = input.repository else {
        return Err(PlanChangeError::UnsupportedRepository);
    };
    if input.change_context.is_some() || input.constraints.is_some() {
        return Err(PlanChangeError::UnsupportedOption);
    }

    let mut target_symbols = Vec::new();
    let mut target_files = Vec::new();
    for target in input.targets {
        match target {
            PlanTargetSelector::Symbol(symbol) => target_symbols.push(symbol.symbol_id),
            PlanTargetSelector::File(file) => target_files.push(file.file_id),
        }
    }
    if target_symbols.is_empty() && target_files.is_empty() {
        return Err(PlanChangeError::EmptyTargets);
    }

    Ok(PlanChangeRequest {
        repository: repository.repository_id,
        generation: input.generation.unwrap_or(GenerationSelector::Active(
            rootlight_mcp_contract::vertical::ActiveGeneration::Active,
        )),
        objective: input.objective,
        objective_text: input.objective_text,
        target_symbols,
        target_files,
        max_steps: input.max_steps,
        budget: input.budget,
        response_profile: input.profile.unwrap_or(ResponseProfile::Compact),
        explain_only: input.explain == Some(true),
    })
}

/// Client-independent planning facts returned by a daemon adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangeResult {
    /// Ordered plan steps.
    pub plan: Vec<ChangePlanStep>,
    /// Compact affected-scope facts.
    pub affected_scope: PlanImpactResult,
    /// Ranked verification candidates.
    pub test_plan: Vec<TestCandidate>,
    /// Decisions that require user input.
    pub open_decisions: Vec<PlanDecision>,
    /// Follow-up context-pack arguments.
    pub context_pack_request: ContextPackRequest,
}

/// Client-independent affected-scope facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanImpactResult {
    /// Total affected symbol count.
    pub affected_symbols: u32,
    /// Total affected file count.
    pub affected_files: u32,
    /// Stable daemon risk label.
    pub risk_level: String,
    /// Whether the public API surface is affected.
    pub touches_public_surface: bool,
}

/// Future returned by the transport-neutral plan-change port.
pub type PlanChangePortFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Typed daemon facts and read metadata adapted for agent-owned shaping.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanChangePortOutput {
    /// Immutable repository and generation context.
    pub identity: AgentResolvedIdentity,
    /// Client-independent planning facts.
    pub result: PlanChangeResult,
    /// Measured daemon usage.
    pub usage: UsageSummary,
    /// Whether the daemon truncated its bounded result.
    pub truncated: bool,
    /// Authoritative daemon execution completeness.
    pub completeness: ResultCompleteness,
    /// Source-free response warnings.
    pub warnings: Vec<ResponseWarning>,
}

/// Client-free boundary for plan evidence and structural planning facts.
pub trait PlanChangePort<C>: AgentToolPort<C>
where
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    /// Fetches the structural plan proposal after evidence providers complete.
    fn plan_change(
        &self,
        request: PlanChangeRequest,
        context: AgentCallContext<C>,
    ) -> PlanChangePortFuture<Result<PlanChangePortOutput, AgentPortError>>;
}

/// Failure returned by complete plan-change orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlanChangeServiceError {
    /// Public request admission failed.
    Admission(PlanChangeError),
    /// A checked adapter error occurred.
    Public(Box<PublicError>),
    /// Cooperative cancellation won.
    Cancelled,
    /// The bounded request deadline elapsed.
    DeadlineExceeded,
    /// The shared evidence and publication budget was exhausted.
    BudgetExceeded,
    /// Adapter facts violated the typed identity contract.
    InvalidResponse,
    /// The provider was unavailable.
    Unavailable,
}

/// Complete transport-neutral service for `plan.change`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanChangeService;

impl PlanChangeService {
    /// Normalizes, orchestrates, and shapes one public change plan.
    ///
    /// # Errors
    ///
    /// Returns [`PlanChangeServiceError`] when admission fails, cancellation or
    /// the deadline wins, or adapter facts violate the typed contract.
    pub async fn execute<P, C>(
        &self,
        port: Arc<P>,
        input: PlanChangeInput,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ReadEnvelope<PlanChangeData>, PlanChangeServiceError>
    where
        P: PlanChangePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        plan_change_checkpoint(&cancellation, deadline)?;
        let request = normalize_plan_change(input).map_err(PlanChangeServiceError::Admission)?;
        let identity = <P as AgentToolPort<C>>::resolve_identity(
            port.as_ref(),
            AgentIdentityRequest::new(
                RepositorySelector::ById(rootlight_mcp_contract::vertical::RepositoryIdSelector {
                    repository_id: request.repository(),
                }),
                Some(request.generation().clone()),
            ),
            AgentResolutionContext::new(cancellation.clone(), deadline),
        )
        .await
        .map_err(map_plan_port_error)?;
        plan_change_checkpoint(&cancellation, deadline)?;

        self.execute_admitted_with_identity(port, request, identity, cancellation, deadline)
            .await
    }

    /// Plans a change under a repository and generation identity pinned by the
    /// caller, without performing another identity lookup.
    ///
    /// # Errors
    ///
    /// Returns [`PlanChangeServiceError`] when admission fails, cancellation or
    /// the deadline wins, or adapter facts violate the pinned identity.
    pub async fn execute_with_identity<P, C>(
        &self,
        port: Arc<P>,
        input: PlanChangeInput,
        identity: AgentResolvedIdentity,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ReadEnvelope<PlanChangeData>, PlanChangeServiceError>
    where
        P: PlanChangePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        plan_change_checkpoint(&cancellation, deadline)?;
        let request = normalize_plan_change(input).map_err(PlanChangeServiceError::Admission)?;
        self.execute_admitted_with_identity(port, request, identity, cancellation, deadline)
            .await
    }

    async fn execute_admitted_with_identity<P, C>(
        &self,
        port: Arc<P>,
        mut request: PlanChangeRequest,
        identity: AgentResolvedIdentity,
        cancellation: C,
        deadline: Instant,
    ) -> Result<ReadEnvelope<PlanChangeData>, PlanChangeServiceError>
    where
        P: PlanChangePort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        if identity.repository.repository_id != request.repository() {
            return Err(PlanChangeServiceError::InvalidResponse);
        }
        if matches!(
            request.generation(),
            GenerationSelector::Explicit(expected)
                if identity.generation.generation_id != *expected
        ) {
            return Err(PlanChangeServiceError::InvalidResponse);
        }
        request.generation = GenerationSelector::Explicit(identity.generation.generation_id);

        if request.explain_only() {
            let data = explain_plan_change(&request, identity.generation.generation_id);
            return Ok(ReadEnvelope {
                schema_version: SchemaVersion::V1_0,
                repository: identity.repository,
                generation: identity.generation,
                coverage: identity.coverage,
                data,
                truncated: false,
                completeness: ResultCompleteness::complete(),
                next_cursor: RequiredNullable(None),
                usage: empty_plan_usage(),
                warnings: identity.warnings,
                trust: TrustClassification::UntrustedRepositoryData,
            });
        }

        let mut ledger = BudgetLedger::new(request.budget().cloned());
        let publication_floor = minimum_plan_publication_charge(&identity)?;
        ledger.charge(publication_floor).map_err(map_policy_error)?;

        let evidence = collect_plan_evidence(
            Arc::clone(&port),
            &request,
            &identity,
            cancellation.clone(),
            deadline,
            &mut ledger,
        )
        .await?;
        let mut budget = remaining_response_budget(&ledger, request.budget())?;
        budget.max_results = request
            .max_steps()
            .map(u16::from)
            .into_iter()
            .chain(budget.max_results)
            .min();
        let output = port
            .plan_change(
                request.clone(),
                AgentCallContext::new(cancellation.clone(), budget, Some(deadline))
                    .with_pinned_identity(identity.clone()),
            )
            .await
            .map_err(map_plan_port_error)?;
        plan_change_checkpoint(&cancellation, deadline)?;
        if output.identity.repository.repository_id != identity.repository.repository_id
            || output.identity.generation.generation_id != identity.generation.generation_id
        {
            return Err(PlanChangeServiceError::InvalidResponse);
        }
        let resource_truncated = output.completeness.state == CompletenessState::Truncated
            || output
                .completeness
                .limiting_resources
                .iter()
                .any(|resource| {
                    !matches!(
                        resource.kind,
                        LimitingResourceKind::Capability | LimitingResourceKind::Coverage
                    )
                });
        if output.completeness.continuation == ContinuationAvailability::Available
            || resource_truncated != output.truncated
        {
            return Err(PlanChangeServiceError::InvalidResponse);
        }
        charge_usage(&mut ledger, &output.usage, output.result.plan.len())?;
        let mut data =
            shape_plan_change(output.result).map_err(PlanChangeServiceError::Admission)?;
        attach_plan_evidence(&mut data, request.objective_kind(), &evidence);
        let completeness = merge_plan_completeness(&output.completeness, &evidence)?;
        let truncated = completeness.state == CompletenessState::Truncated
            || completeness.limiting_resources.iter().any(|resource| {
                !matches!(
                    resource.kind,
                    LimitingResourceKind::Capability | LimitingResourceKind::Coverage
                )
            });
        let warnings = aggregate_plan_warnings(
            identity.warnings.clone(),
            evidence
                .iter()
                .flat_map(|provider| provider.warnings.iter()),
            output.warnings,
        );
        let mut envelope = ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: identity.repository,
            generation: identity.generation,
            coverage: identity.coverage,
            data,
            truncated,
            completeness,
            next_cursor: RequiredNullable(None),
            usage: aggregate_plan_usage(&ledger, output.usage),
            warnings,
            trust: TrustClassification::UntrustedRepositoryData,
        };
        crate::response_profile::shape_read_envelope(&mut envelope, request.response_profile());
        charge_final_plan_representation(&mut ledger, &mut envelope, publication_floor)?;
        envelope.usage = aggregate_plan_usage(&ledger, envelope.usage);
        Ok(envelope)
    }
}

#[derive(Debug, Clone)]
struct CollectedPlanProvider {
    coverage: PlanProviderCoverage,
    warnings: Vec<ResponseWarning>,
}

async fn collect_plan_evidence<P, C>(
    port: Arc<P>,
    request: &PlanChangeRequest,
    identity: &AgentResolvedIdentity,
    cancellation: C,
    deadline: Instant,
    ledger: &mut BudgetLedger,
) -> Result<Vec<CollectedPlanProvider>, PlanChangeServiceError>
where
    P: PlanChangePort<C>,
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    let mut providers = Vec::with_capacity(PLAN_PROVIDER_COUNT);
    if request.target_symbols().is_empty() {
        providers.push(unsupported_provider(
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceOmissionReason::NoCompatibleTargets,
        ));
    } else {
        let arguments = object(json!({
            "repository": repository_selector(request.repository()),
            "generation": GenerationSelector::Explicit(identity.generation.generation_id),
            "change": {"symbol_ids": request.target_symbols()},
            "include_tests": true
        }))?;
        let mut provider = collect_typed_provider::<P, C, ChangeImpactData>(
            Arc::clone(&port),
            BatchTool::ChangeImpact,
            arguments,
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceKind::ImpactScope,
            "change-impact",
            identity,
            cancellation.clone(),
            deadline,
            ledger,
            |data| {
                data.resolved_changes
                    .len()
                    .saturating_add(data.impacted.len())
                    .saturating_add(data.service_impacts.len())
                    .saturating_add(data.tests.len())
            },
        )
        .await?;
        if !request.target_files().is_empty() {
            mark_partially_unsupported(
                &mut provider,
                PlanEvidenceOmissionReason::NoCompatibleTargets,
            )?;
        }
        providers.push(provider);
    }

    if request.target_symbols().is_empty() {
        providers.push(unsupported_provider(
            PlanEvidenceProvider::Relationships,
            PlanEvidenceOmissionReason::NoCompatibleTargets,
        ));
    } else {
        let arguments = object(json!({
            "repository": repository_selector(request.repository()),
            "generation": GenerationSelector::Explicit(identity.generation.generation_id),
            "symbol_ids": request.target_symbols(),
            "relations": ["calls", "called_by", "references", "types", "implements", "imports"],
            "direction": "both"
        }))?;
        providers.push(
            collect_typed_provider::<P, C, SymbolRelationshipsData>(
                Arc::clone(&port),
                BatchTool::SymbolRelationships,
                arguments,
                PlanEvidenceProvider::Relationships,
                PlanEvidenceKind::RelationshipGraph,
                "relationships",
                identity,
                cancellation.clone(),
                deadline,
                ledger,
                |data| {
                    data.groups
                        .iter()
                        .map(|group| group.items.len())
                        .fold(data.unresolved.len(), usize::saturating_add)
                },
            )
            .await?,
        );
    }

    if request.target_symbols().is_empty() {
        providers.push(unsupported_provider(
            PlanEvidenceProvider::Tests,
            PlanEvidenceOmissionReason::NoCompatibleTargets,
        ));
    } else {
        let arguments = object(json!({
            "repository": repository_selector(request.repository()),
            "generation": GenerationSelector::Explicit(identity.generation.generation_id),
            "seeds": {"symbols": request.target_symbols()},
            "include_commands": false
        }))?;
        let mut provider = collect_typed_provider::<P, C, TestsSelectData>(
            Arc::clone(&port),
            BatchTool::TestsSelect,
            arguments,
            PlanEvidenceProvider::Tests,
            PlanEvidenceKind::TestSelection,
            "tests",
            identity,
            cancellation.clone(),
            deadline,
            ledger,
            |data| data.tests.len().saturating_add(data.gaps.len()),
        )
        .await?;
        if !request.target_files().is_empty() {
            mark_partially_unsupported(
                &mut provider,
                PlanEvidenceOmissionReason::NoCompatibleTargets,
            )?;
        }
        providers.push(provider);
    }

    let arguments = object(json!({
        "repository": repository_selector(request.repository()),
        "generation": GenerationSelector::Explicit(identity.generation.generation_id),
        "include_edges": true
    }))?;
    providers.push(
        collect_typed_provider::<P, C, ArchitectureOverviewData>(
            port,
            BatchTool::ArchitectureOverview,
            arguments,
            PlanEvidenceProvider::Architecture,
            PlanEvidenceKind::Architecture,
            "architecture",
            identity,
            cancellation,
            deadline,
            ledger,
            |data| {
                data.components
                    .len()
                    .saturating_add(data.connections.len())
                    .saturating_add(data.hotspots.len())
            },
        )
        .await?,
    );

    providers.push(unsupported_provider(
        PlanEvidenceProvider::History,
        PlanEvidenceOmissionReason::HistoryBaselineUnavailable,
    ));
    providers.push(unsupported_provider(
        PlanEvidenceProvider::Source,
        PlanEvidenceOmissionReason::SourceReferencesUnavailable,
    ));
    providers.push(unsupported_provider(
        PlanEvidenceProvider::Ownership,
        PlanEvidenceOmissionReason::OwnershipProviderUnsupported,
    ));
    Ok(providers)
}

#[expect(
    clippy::too_many_arguments,
    reason = "provider collection keeps tool identity, policy, and evidence typing explicit"
)]
async fn collect_typed_provider<P, C, T>(
    port: Arc<P>,
    tool: BatchTool,
    arguments: Map<String, Value>,
    provider: PlanEvidenceProvider,
    kind: PlanEvidenceKind,
    evidence_prefix: &str,
    identity: &AgentResolvedIdentity,
    cancellation: C,
    deadline: Instant,
    ledger: &mut BudgetLedger,
    observed_items: fn(&T) -> usize,
) -> Result<CollectedPlanProvider, PlanChangeServiceError>
where
    P: PlanChangePort<C>,
    C: CancellationSignal + Clone + Send + Sync + 'static,
    T: DeserializeOwned,
{
    plan_change_checkpoint(&cancellation, deadline)?;
    let budget = match remaining_response_budget(ledger, None) {
        Ok(budget) => budget,
        Err(PlanChangeServiceError::BudgetExceeded) => {
            return Ok(omitted_provider(
                provider,
                PlanEvidenceOmissionReason::SharedBudgetExhausted,
            ));
        }
        Err(error) => return Err(error),
    };
    let context = AgentCallContext::new(cancellation, budget, Some(deadline))
        .with_response_profile(rootlight_mcp_contract::vertical::ResponseProfile::Compact)
        .with_pinned_identity(identity.clone());
    let envelope = match port
        .execute(AgentToolRequest::new(tool, arguments), context)
        .await
    {
        Ok(envelope) => envelope,
        Err(error) => {
            return provider_error_coverage(provider, error, ledger);
        }
    };
    if envelope.repository.repository_id != identity.repository.repository_id
        || envelope.generation.generation_id != identity.generation.generation_id
        || envelope.next_cursor.0.is_some()
    {
        return Err(PlanChangeServiceError::InvalidResponse);
    }
    let data: T = serde_json::from_value(envelope.data)
        .map_err(|_| PlanChangeServiceError::InvalidResponse)?;
    let observed = observed_items(&data);
    charge_usage(ledger, &envelope.usage, observed)?;
    let (coverage, projection_truncated) = provider_coverage(
        provider,
        kind,
        evidence_prefix,
        observed,
        envelope.completeness,
    )?;
    let mut warnings = envelope.warnings;
    if projection_truncated {
        warnings.truncate(31);
        warnings.push(plan_warning(
            "plan_provider_evidence_truncated",
            "plan provider evidence references were truncated",
        )?);
    }
    Ok(CollectedPlanProvider { coverage, warnings })
}

fn provider_error_coverage(
    provider: PlanEvidenceProvider,
    error: AgentPortError,
    ledger: &mut BudgetLedger,
) -> Result<CollectedPlanProvider, PlanChangeServiceError> {
    let (error, usage) = error.into_parts();
    if let Some(usage) = usage {
        charge_usage(ledger, &usage, 0)?;
    }
    match error {
        AgentPortError::Public(error)
            if matches!(
                error.code(),
                ErrorCode::UnsupportedCapability
                    | ErrorCode::IncompleteCoverage
                    | ErrorCode::NotFound
            ) =>
        {
            Ok(unsupported_provider(
                provider,
                PlanEvidenceOmissionReason::ProviderUnavailable,
            ))
        }
        AgentPortError::Public(error)
            if matches!(
                error.code(),
                ErrorCode::BudgetExceeded | ErrorCode::ResourceExhausted
            ) =>
        {
            Ok(omitted_provider(
                provider,
                PlanEvidenceOmissionReason::SharedBudgetExhausted,
            ))
        }
        AgentPortError::Public(error) => Err(PlanChangeServiceError::Public(error)),
        AgentPortError::Cancelled => Err(PlanChangeServiceError::Cancelled),
        AgentPortError::DeadlineExceeded => Err(PlanChangeServiceError::DeadlineExceeded),
        AgentPortError::LocalDeadlineExceeded => Err(PlanChangeServiceError::InvalidResponse),
        AgentPortError::InvalidResponse => Err(PlanChangeServiceError::InvalidResponse),
        AgentPortError::Unavailable => Ok(omitted_provider(
            provider,
            PlanEvidenceOmissionReason::ProviderUnavailable,
        )),
        AgentPortError::Measured { .. } => Err(PlanChangeServiceError::InvalidResponse),
    }
}

fn provider_coverage(
    provider: PlanEvidenceProvider,
    kind: PlanEvidenceKind,
    evidence_prefix: &str,
    observed: usize,
    mut completeness: ResultCompleteness,
) -> Result<(PlanProviderCoverage, bool), PlanChangeServiceError> {
    let retained = observed.min(PLAN_EVIDENCE_RECORD_LIMIT);
    let mut evidence = Vec::new();
    evidence
        .try_reserve_exact(retained)
        .map_err(|_| PlanChangeServiceError::Unavailable)?;
    for index in 0..retained {
        evidence.push(PlanEvidenceRecord {
            evidence_id: format!("{evidence_prefix}:{index:03}"),
            kind,
            observed_items: 1,
        });
    }
    let projection_truncated = observed > retained;
    if projection_truncated {
        completeness = merge_completeness(
            &completeness,
            &truncated_completeness(
                LimitingResourceKind::Results,
                u64::try_from(retained).unwrap_or(u64::MAX),
                u64::try_from(observed).unwrap_or(u64::MAX),
            )?,
        )?;
    }
    let state = provider_state(&completeness);
    Ok((
        PlanProviderCoverage {
            provider,
            state,
            evidence,
            completeness,
            omission: None,
        },
        projection_truncated,
    ))
}

fn provider_state(completeness: &ResultCompleteness) -> PlanProviderState {
    match completeness.state {
        CompletenessState::Complete => PlanProviderState::Complete,
        CompletenessState::Truncated
        | CompletenessState::UnsupportedPartial
        | CompletenessState::Indeterminate => PlanProviderState::Partial,
    }
}

fn unsupported_provider(
    provider: PlanEvidenceProvider,
    reason: PlanEvidenceOmissionReason,
) -> CollectedPlanProvider {
    CollectedPlanProvider {
        coverage: PlanProviderCoverage {
            provider,
            state: PlanProviderState::Unsupported,
            evidence: Vec::new(),
            completeness: unsupported_completeness(),
            omission: Some(PlanEvidenceOmission { reason }),
        },
        warnings: Vec::new(),
    }
}

fn omitted_provider(
    provider: PlanEvidenceProvider,
    reason: PlanEvidenceOmissionReason,
) -> CollectedPlanProvider {
    let completeness = if reason == PlanEvidenceOmissionReason::SharedBudgetExhausted {
        truncated_completeness(LimitingResourceKind::EstimatedTokens, 0, 0)
            .unwrap_or_else(|_| ResultCompleteness::indeterminate())
    } else {
        ResultCompleteness::indeterminate()
    };
    CollectedPlanProvider {
        coverage: PlanProviderCoverage {
            provider,
            state: PlanProviderState::Omitted,
            evidence: Vec::new(),
            completeness,
            omission: Some(PlanEvidenceOmission { reason }),
        },
        warnings: Vec::new(),
    }
}

fn mark_partially_unsupported(
    provider: &mut CollectedPlanProvider,
    reason: PlanEvidenceOmissionReason,
) -> Result<(), PlanChangeServiceError> {
    provider.coverage.state = PlanProviderState::Partial;
    provider.coverage.completeness =
        merge_completeness(&provider.coverage.completeness, &unsupported_completeness())?;
    provider.coverage.omission = Some(PlanEvidenceOmission { reason });
    Ok(())
}

fn unsupported_completeness() -> ResultCompleteness {
    ResultCompleteness::new(
        CompletenessState::UnsupportedPartial,
        vec![LimitingResource::kind(LimitingResourceKind::Capability)],
        ContinuationAvailability::Unavailable,
        vec![ContinuationGuidance::UnsupportedNoContinuation],
    )
    .unwrap_or_else(|_| ResultCompleteness::indeterminate())
}

fn truncated_completeness(
    kind: LimitingResourceKind,
    limit: u64,
    observed: u64,
) -> Result<ResultCompleteness, PlanChangeServiceError> {
    ResultCompleteness::new(
        CompletenessState::Truncated,
        vec![LimitingResource {
            kind,
            limit: Some(limit),
            observed: Some(observed),
        }],
        ContinuationAvailability::Unavailable,
        vec![ContinuationGuidance::IncreaseBudgetWithinLimit],
    )
    .map_err(|_| PlanChangeServiceError::InvalidResponse)
}

fn attach_plan_evidence(
    data: &mut PlanChangeData,
    objective: PlanObjective,
    evidence: &[CollectedPlanProvider],
) {
    let evidence_refs = evidence
        .iter()
        .filter_map(|provider| provider.coverage.evidence.first())
        .map(|record| record.evidence_id.clone())
        .take(16)
        .collect::<Vec<_>>();
    let rationale = objective_rationale(objective);
    for step in &mut data.plan {
        step.rationale = rationale.to_owned();
        step.evidence_refs.clone_from(&evidence_refs);
    }
    data.provider_coverage = evidence
        .iter()
        .map(|provider| provider.coverage.clone())
        .collect();
}

const fn objective_rationale(objective: PlanObjective) -> &'static str {
    match objective {
        PlanObjective::BugFix => {
            "bounded defect repair step derived from generation pinned provider evidence"
        }
        PlanObjective::Refactor => {
            "bounded behavior preserving step derived from generation pinned provider evidence"
        }
        PlanObjective::Explanation => {
            "bounded read only step derived from generation pinned provider evidence"
        }
        PlanObjective::Migration => {
            "bounded compatibility migration step derived from generation pinned provider evidence"
        }
        PlanObjective::Review => {
            "bounded review step derived from generation pinned provider evidence"
        }
    }
}

fn merge_plan_completeness(
    planner: &ResultCompleteness,
    evidence: &[CollectedPlanProvider],
) -> Result<ResultCompleteness, PlanChangeServiceError> {
    evidence
        .iter()
        .try_fold(planner.clone(), |aggregate, provider| {
            merge_completeness(&aggregate, &provider.coverage.completeness)
        })
}

fn merge_completeness(
    left: &ResultCompleteness,
    right: &ResultCompleteness,
) -> Result<ResultCompleteness, PlanChangeServiceError> {
    let state = left.state.max(right.state);
    let mut resources = BTreeMap::<LimitingResourceKind, LimitingResource>::new();
    for resource in left
        .limiting_resources
        .iter()
        .chain(&right.limiting_resources)
    {
        resources
            .entry(resource.kind)
            .and_modify(|current| {
                current.limit = match (current.limit, resource.limit) {
                    (Some(left), Some(right)) => Some(left.min(right)),
                    (left, None) => left,
                    (None, right) => right,
                };
                current.observed = match (current.observed, resource.observed) {
                    (Some(left), Some(right)) => Some(left.max(right)),
                    (left, None) => left,
                    (None, right) => right,
                };
            })
            .or_insert(*resource);
    }
    let continuation = if state == CompletenessState::Complete {
        ContinuationAvailability::NotApplicable
    } else if left.continuation == ContinuationAvailability::Unavailable
        || right.continuation == ContinuationAvailability::Unavailable
    {
        ContinuationAvailability::Unavailable
    } else if left.continuation == ContinuationAvailability::Available
        || right.continuation == ContinuationAvailability::Available
    {
        ContinuationAvailability::Available
    } else {
        ContinuationAvailability::Unavailable
    };
    let guidance = left
        .guidance
        .iter()
        .chain(&right.guidance)
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|guidance| {
            *guidance != ContinuationGuidance::UseCursor
                || continuation == ContinuationAvailability::Available
        })
        .collect();
    ResultCompleteness::new(
        state,
        resources.into_values().collect(),
        continuation,
        guidance,
    )
    .map_err(|_| PlanChangeServiceError::InvalidResponse)
}

fn aggregate_plan_warnings<'a>(
    mut warnings: Vec<ResponseWarning>,
    provider_warnings: impl Iterator<Item = &'a ResponseWarning>,
    planner_warnings: Vec<ResponseWarning>,
) -> Vec<ResponseWarning> {
    for warning in provider_warnings {
        if warnings.len() == 32 {
            return warnings;
        }
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
    for warning in planner_warnings {
        if warnings.len() == 32 {
            break;
        }
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    warnings
}

fn repository_selector(repository: RepositoryId) -> RepositorySelector {
    RepositorySelector::ById(rootlight_mcp_contract::vertical::RepositoryIdSelector {
        repository_id: repository,
    })
}

fn object(value: Value) -> Result<Map<String, Value>, PlanChangeServiceError> {
    value
        .as_object()
        .cloned()
        .ok_or(PlanChangeServiceError::InvalidResponse)
}

fn remaining_response_budget(
    ledger: &BudgetLedger,
    requested: Option<&ResponseBudget>,
) -> Result<ResponseBudget, PlanChangeServiceError> {
    let remaining = ledger.remaining();
    if remaining.results == 0
        || remaining.tokens < 100
        || remaining.source_bytes == 0
        || remaining.traversal_facts == 0
        || remaining.depth == 0
        || remaining.paths == 0
        || remaining.time_ms < 10
    {
        return Err(PlanChangeServiceError::BudgetExceeded);
    }
    Ok(ResponseBudget {
        max_results: Some(u16::try_from(remaining.results).unwrap_or(u16::MAX)),
        max_tokens: Some(u16::try_from(remaining.tokens.min(16_000)).unwrap_or(16_000)),
        max_source_bytes: Some(u32::try_from(remaining.source_bytes).unwrap_or(u32::MAX)),
        max_traversal_facts: Some(u32::try_from(remaining.traversal_facts).unwrap_or(u32::MAX)),
        max_depth: Some(u8::try_from(remaining.depth).unwrap_or(u8::MAX)),
        max_paths: Some(u16::try_from(remaining.paths).unwrap_or(u16::MAX)),
        timeout_ms: Some(u32::try_from(remaining.time_ms).unwrap_or(u32::MAX)),
        evidence_level: requested.and_then(|budget| budget.evidence_level),
    })
}

fn charge_usage(
    ledger: &mut BudgetLedger,
    usage: &UsageSummary,
    results: usize,
) -> Result<(), PlanChangeServiceError> {
    ledger
        .charge(BudgetCharge {
            rows: usage.rows,
            results: u64::try_from(results).unwrap_or(u64::MAX),
            // Provider and structural responses are transient orchestration
            // inputs. Their returned rows/facts/source bytes consume the
            // shared work budget, while only the final public representation
            // consumes the caller's output-token and JSON-byte budgets.
            tokens: 0,
            actual_tokens: 0,
            source_bytes: usage.source_bytes,
            traversal_facts: usage.edges,
            depth: 0,
            paths: 0,
            json_bytes: 0,
            memory_bytes: 0,
            time_ms: usage.wall_time_ms,
        })
        .map_err(map_policy_error)
}

fn map_policy_error(error: ExecutionPolicyError) -> PlanChangeServiceError {
    match error {
        ExecutionPolicyError::Cancelled => PlanChangeServiceError::Cancelled,
        ExecutionPolicyError::BudgetExceeded { .. } => PlanChangeServiceError::BudgetExceeded,
    }
}

fn minimum_plan_publication_charge(
    identity: &AgentResolvedIdentity,
) -> Result<BudgetCharge, PlanChangeServiceError> {
    let coverage = [
        (
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceOmissionReason::ProviderUnavailable,
        ),
        (
            PlanEvidenceProvider::Relationships,
            PlanEvidenceOmissionReason::ProviderUnavailable,
        ),
        (
            PlanEvidenceProvider::Tests,
            PlanEvidenceOmissionReason::ProviderUnavailable,
        ),
        (
            PlanEvidenceProvider::Architecture,
            PlanEvidenceOmissionReason::ProviderUnavailable,
        ),
        (
            PlanEvidenceProvider::History,
            PlanEvidenceOmissionReason::HistoryBaselineUnavailable,
        ),
        (
            PlanEvidenceProvider::Source,
            PlanEvidenceOmissionReason::SourceReferencesUnavailable,
        ),
        (
            PlanEvidenceProvider::Ownership,
            PlanEvidenceOmissionReason::OwnershipProviderUnsupported,
        ),
    ]
    .into_iter()
    .map(|(provider, reason)| unsupported_provider(provider, reason).coverage)
    .collect();
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: identity.repository.clone(),
        generation: identity.generation.clone(),
        coverage: identity.coverage.clone(),
        data: PlanChangeData {
            plan: vec![ChangePlanStep {
                step: 1,
                action: "bounded plan unavailable".to_owned(),
                rationale: "bounded provider evidence unavailable".to_owned(),
                evidence_refs: Vec::new(),
                targets: Vec::new(),
                depends_on: Vec::new(),
                risks: Vec::new(),
                verification: None,
            }],
            affected_scope: PlanImpactSummary {
                affected_symbols: 0,
                affected_files: 0,
                risk_level: RiskLevel::None,
                touches_public_surface: false,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ContextPackRequest {
                symbols: Vec::new(),
                files: Vec::new(),
            },
            provider_coverage: coverage,
            explanation: None,
        },
        truncated: true,
        completeness: unsupported_completeness(),
        next_cursor: RequiredNullable(None),
        usage: empty_plan_usage(),
        warnings: identity.warnings.clone(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    publication_charge(&envelope)
}

fn publication_charge<T: Serialize>(
    envelope: &ReadEnvelope<T>,
) -> Result<BudgetCharge, PlanChangeServiceError> {
    let bytes = serde_json::to_vec(&rootlight_mcp_contract::vertical::ToolResponse::Success(
        envelope,
    ))
    .map_err(|_| PlanChangeServiceError::InvalidResponse)?
    .len();
    let bytes = u64::try_from(bytes)
        .map_err(|_| PlanChangeServiceError::InvalidResponse)?
        .saturating_add(64);
    let estimated_tokens = rootlight_mcp_contract::accounting::estimate_tokens(
        usize::try_from(bytes).map_err(|_| PlanChangeServiceError::InvalidResponse)?,
    );
    Ok(BudgetCharge {
        tokens: estimated_tokens,
        actual_tokens: 0,
        json_bytes: bytes,
        ..BudgetCharge::default()
    })
}

fn charge_final_plan_representation(
    ledger: &mut BudgetLedger,
    envelope: &mut ReadEnvelope<PlanChangeData>,
    publication_floor: BudgetCharge,
) -> Result<(), PlanChangeServiceError> {
    envelope.usage.json_bytes = 0;
    envelope.usage.estimated_tokens = 0;
    let actual = publication_charge(envelope)?;
    ledger
        .charge(BudgetCharge {
            tokens: actual.tokens.saturating_sub(publication_floor.tokens),
            actual_tokens: actual
                .actual_tokens
                .saturating_sub(publication_floor.actual_tokens),
            json_bytes: actual
                .json_bytes
                .saturating_sub(publication_floor.json_bytes),
            ..BudgetCharge::default()
        })
        .map_err(map_policy_error)
}

fn aggregate_plan_usage(ledger: &BudgetLedger, planner: UsageSummary) -> UsageSummary {
    let consumed = ledger.consumed();
    UsageSummary {
        rows: consumed.rows,
        edges: consumed.traversal_facts,
        source_bytes: consumed.source_bytes,
        // The application serializer replaces both representation counters
        // with the exact fixed point of the final public envelope.
        json_bytes: 0,
        estimated_tokens: 0,
        wall_time_ms: planner.wall_time_ms.max(consumed.time_ms),
        cache_status: planner.cache_status,
        trace_id: "plan-change-orchestration".to_owned(),
    }
}

fn plan_warning(code: &str, message: &str) -> Result<ResponseWarning, PlanChangeServiceError> {
    Ok(ResponseWarning {
        code: rootlight_mcp_contract::SafeLabel::parse(code)
            .map_err(|_| PlanChangeServiceError::InvalidResponse)?,
        message: rootlight_mcp_contract::vertical::SourceFreeMessage::parse(message)
            .map_err(|_| PlanChangeServiceError::InvalidResponse)?,
    })
}

fn plan_change_checkpoint<C>(
    cancellation: &C,
    deadline: Instant,
) -> Result<(), PlanChangeServiceError>
where
    C: CancellationSignal,
{
    if cancellation.is_cancelled() {
        Err(PlanChangeServiceError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(PlanChangeServiceError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn map_plan_port_error(error: AgentPortError) -> PlanChangeServiceError {
    let (error, _) = error.into_parts();
    match error {
        AgentPortError::Public(error) => PlanChangeServiceError::Public(error),
        AgentPortError::Cancelled => PlanChangeServiceError::Cancelled,
        AgentPortError::DeadlineExceeded => PlanChangeServiceError::DeadlineExceeded,
        AgentPortError::LocalDeadlineExceeded => PlanChangeServiceError::InvalidResponse,
        AgentPortError::InvalidResponse => PlanChangeServiceError::InvalidResponse,
        AgentPortError::Unavailable => PlanChangeServiceError::Unavailable,
        AgentPortError::Measured { .. } => PlanChangeServiceError::InvalidResponse,
    }
}

fn empty_plan_usage() -> UsageSummary {
    UsageSummary {
        rows: 0,
        edges: 0,
        source_bytes: 0,
        json_bytes: 0,
        estimated_tokens: 0,
        wall_time_ms: 0,
        cache_status: rootlight_mcp_contract::vertical::CacheStatus::NotApplicable,
        trace_id: "plan-change-explain".to_owned(),
    }
}

/// Shapes checked planning facts into the public response data contract.
///
/// # Errors
///
/// Returns [`PlanChangeError::InvalidRisk`] for an unknown daemon risk label.
pub fn shape_plan_change(result: PlanChangeResult) -> Result<PlanChangeData, PlanChangeError> {
    if result.plan.is_empty()
        || result.plan.len() > 100
        || result.plan.iter().enumerate().any(|(index, step)| {
            usize::from(step.step) != index.saturating_add(1)
                || step
                    .depends_on
                    .iter()
                    .any(|dependency| *dependency == 0 || *dependency >= step.step)
        })
    {
        return Err(PlanChangeError::InvalidPlan);
    }
    Ok(PlanChangeData {
        plan: result.plan,
        affected_scope: PlanImpactSummary {
            affected_symbols: result.affected_scope.affected_symbols,
            affected_files: result.affected_scope.affected_files,
            risk_level: risk_level(&result.affected_scope.risk_level)?,
            touches_public_surface: result.affected_scope.touches_public_surface,
        },
        test_plan: result.test_plan,
        open_decisions: result.open_decisions,
        context_pack_request: result.context_pack_request,
        provider_coverage: Vec::new(),
        explanation: None,
    })
}

/// Builds source-free `plan.change` output for explain-only execution.
#[must_use]
pub fn explain_plan_change(
    request: &PlanChangeRequest,
    generation: GenerationId,
) -> PlanChangeData {
    let explanation = finalize_plan(
        plan_change_plan(request.max_steps, request.target_count()),
        &generation.to_string(),
    );
    explain_data(explanation)
}

const fn objective_label(objective: PlanObjective) -> &'static str {
    match objective {
        PlanObjective::BugFix => "bug_fix",
        PlanObjective::Refactor => "refactor",
        PlanObjective::Explanation => "explanation",
        PlanObjective::Migration => "migration",
        PlanObjective::Review => "review",
    }
}

fn risk_level(label: &str) -> Result<RiskLevel, PlanChangeError> {
    match label {
        "none" => Ok(RiskLevel::None),
        "low" => Ok(RiskLevel::Low),
        "medium" => Ok(RiskLevel::Medium),
        "high" => Ok(RiskLevel::High),
        "critical" => Ok(RiskLevel::Critical),
        _ => Err(PlanChangeError::InvalidRisk),
    }
}

fn explain_data(explanation: PlanExplanation) -> PlanChangeData {
    PlanChangeData {
        plan: vec![ChangePlanStep {
            step: 1,
            action: "explain_only_no_change_planning_executed".to_owned(),
            rationale: "explain mode reports structure without provider evidence".to_owned(),
            evidence_refs: Vec::new(),
            targets: Vec::new(),
            depends_on: Vec::new(),
            risks: Vec::new(),
            verification: None,
        }],
        affected_scope: PlanImpactSummary {
            affected_symbols: 0,
            affected_files: 0,
            risk_level: RiskLevel::None,
            touches_public_surface: false,
        },
        test_plan: Vec::new(),
        open_decisions: Vec::new(),
        context_pack_request: ContextPackRequest {
            symbols: Vec::new(),
            files: Vec::new(),
        },
        provider_coverage: [
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceProvider::Relationships,
            PlanEvidenceProvider::Tests,
            PlanEvidenceProvider::Architecture,
            PlanEvidenceProvider::History,
            PlanEvidenceProvider::Source,
            PlanEvidenceProvider::Ownership,
        ]
        .into_iter()
        .map(|provider| {
            omitted_provider(provider, PlanEvidenceOmissionReason::ExplainOnly).coverage
        })
        .collect(),
        explanation: Some(explanation),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_mcp_contract::{
        RepositorySelector,
        change::{
            ChangePlanStep, ContextPackRequest, PlanChangeInput, PlanFileTarget, PlanImpactSummary,
            PlanObjective, PlanSymbolTarget, PlanTargetSelector, RiskLevel,
        },
        completeness::{
            CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
            LimitingResourceKind, ResultCompleteness,
        },
        vertical::{RepositoryIdSelector, ResponseProfile},
    };

    use super::{
        PlanChangeError, PlanChangeResult, PlanImpactResult, explain_plan_change,
        merge_completeness, normalize_plan_change, shape_plan_change,
    };

    fn input() -> PlanChangeInput {
        PlanChangeInput {
            repository: RepositorySelector::ById(RepositoryIdSelector {
                repository_id: RepositoryId::from_bytes([1; 16]),
            }),
            generation: None,
            objective: PlanObjective::BugFix,
            objective_text: "repair request admission".to_owned(),
            targets: vec![
                PlanTargetSelector::Symbol(PlanSymbolTarget {
                    symbol_id: SymbolId::from_bytes([2; 20]),
                }),
                PlanTargetSelector::File(PlanFileTarget {
                    file_id: FileId::from_bytes([3; 20]),
                }),
            ],
            constraints: None,
            change_context: None,
            max_steps: Some(4),
            budget: None,
            profile: Some(ResponseProfile::Compact),
            explain: None,
        }
    }

    #[test]
    fn normalization_owns_profile_and_target_admission() {
        let request = normalize_plan_change(input()).expect("request is admitted");

        assert_eq!(request.objective(), "bug_fix");
        assert_eq!(request.target_symbols().len(), 1);
        assert_eq!(request.target_files().len(), 1);
        assert_eq!(request.target_count(), 2);
    }

    #[test]
    fn every_response_profile_is_admitted_without_changing_the_plan() {
        for profile in [
            ResponseProfile::Compact,
            ResponseProfile::Standard,
            ResponseProfile::Evidence,
        ] {
            let mut input = input();
            input.profile = Some(profile);
            let request = normalize_plan_change(input).expect("profile is admitted");

            assert_eq!(request.objective(), "bug_fix");
            assert_eq!(request.target_count(), 2);
        }
    }

    #[test]
    fn result_shaping_maps_checked_risk_labels() {
        let data = shape_plan_change(PlanChangeResult {
            plan: vec![ChangePlanStep {
                step: 1,
                action: "inspect impact".to_owned(),
                rationale: String::new(),
                evidence_refs: Vec::new(),
                targets: Vec::new(),
                depends_on: Vec::new(),
                risks: Vec::new(),
                verification: None,
            }],
            affected_scope: PlanImpactResult {
                affected_symbols: 2,
                affected_files: 1,
                risk_level: "high".to_owned(),
                touches_public_surface: true,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ContextPackRequest {
                symbols: Vec::new(),
                files: Vec::new(),
            },
        })
        .expect("known risk is shaped");

        assert_eq!(
            data.affected_scope,
            PlanImpactSummary {
                affected_symbols: 2,
                affected_files: 1,
                risk_level: RiskLevel::High,
                touches_public_surface: true,
            }
        );
    }

    #[test]
    fn unknown_risk_label_fails_closed() {
        let result = PlanChangeResult {
            plan: vec![ChangePlanStep {
                step: 1,
                action: "inspect impact".to_owned(),
                rationale: String::new(),
                evidence_refs: Vec::new(),
                targets: Vec::new(),
                depends_on: Vec::new(),
                risks: Vec::new(),
                verification: None,
            }],
            affected_scope: PlanImpactResult {
                affected_symbols: 0,
                affected_files: 0,
                risk_level: "severe".to_owned(),
                touches_public_surface: false,
            },
            test_plan: Vec::new(),
            open_decisions: Vec::new(),
            context_pack_request: ContextPackRequest {
                symbols: Vec::new(),
                files: Vec::new(),
            },
        };

        assert_eq!(shape_plan_change(result), Err(PlanChangeError::InvalidRisk));
    }

    #[test]
    fn explain_output_is_schema_shaped_and_source_free() {
        let mut input = input();
        input.explain = Some(true);
        let request = normalize_plan_change(input).expect("request is admitted");

        let data = explain_plan_change(&request, GenerationId::from_bytes([4; 20]));

        assert_eq!(data.plan.len(), 1);
        assert_eq!(data.affected_scope.risk_level, RiskLevel::None);
        assert!(data.explanation.is_some());
        serde_json::to_value(data).expect("public plan data serializes");
    }

    #[test]
    fn completeness_merge_coalesces_duplicate_resource_observations() {
        let left = ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![LimitingResource {
                kind: LimitingResourceKind::Results,
                limit: Some(32),
                observed: Some(40),
            }],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::NarrowScope],
        )
        .expect("left completeness is valid");
        let right = ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![LimitingResource {
                kind: LimitingResourceKind::Results,
                limit: Some(64),
                observed: Some(65),
            }],
            ContinuationAvailability::Unavailable,
            vec![ContinuationGuidance::IncreaseBudgetWithinLimit],
        )
        .expect("right completeness is valid");

        let merged = merge_completeness(&left, &right).expect("duplicate resources coalesce");

        assert_eq!(
            merged.limiting_resources,
            vec![LimitingResource {
                kind: LimitingResourceKind::Results,
                limit: Some(32),
                observed: Some(65),
            }]
        );
    }
}
