//! Transport-neutral normalization and shaping for `plan.change`.
//!
//! The MCP application supplies daemon facts through its client adapter. This
//! module owns request admission, source-free explain shaping, and conversion
//! of client-independent planning facts into the public contract.

use std::{future::Future, pin::Pin, sync::Arc, time::Instant};

use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_mcp_contract::{
    GenerationSelector, PublicError, RepositorySelector, SchemaVersion, TrustClassification,
    change::{
        ChangePlanStep, ContextPackRequest, PlanChangeData, PlanChangeInput, PlanDecision,
        PlanImpactSummary, PlanObjective, PlanTargetSelector, RiskLevel, TestCandidate,
    },
    completeness::{
        CompletenessState, ContinuationAvailability, LimitingResourceKind, ResultCompleteness,
    },
    context::PlanExplanation,
    vertical::{ReadEnvelope, RequiredNullable, ResponseWarning, UsageSummary},
};

use crate::{
    explain::{finalize_plan, plan_change_plan},
    policy::CancellationSignal,
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentResolutionContext,
        AgentResolvedIdentity,
    },
};

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
}

/// Normalizes and admits one public `plan.change` request.
///
/// # Errors
///
/// Returns [`PlanChangeError`] when the request requires alias resolution,
/// unsupported context or budget behavior, or contains no symbol or file
/// target.
pub fn normalize_plan_change(input: PlanChangeInput) -> Result<PlanChangeRequest, PlanChangeError> {
    let RepositorySelector::ById(repository) = input.repository else {
        return Err(PlanChangeError::UnsupportedRepository);
    };
    if input.change_context.is_some() || input.constraints.is_some() || input.budget.is_some() {
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

/// Client-free boundary for plan-change identity and planning facts.
pub trait PlanChangePort<C>: Send + Sync + 'static
where
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    /// Resolves immutable identity without source retrieval or mutation.
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<C>,
    ) -> PlanChangePortFuture<Result<AgentResolvedIdentity, AgentPortError>>;

    /// Fetches typed planning facts under the supplied execution policy.
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
        if request.explain_only() {
            let identity = port
                .resolve_identity(
                    AgentIdentityRequest::new(
                        RepositorySelector::ById(
                            rootlight_mcp_contract::vertical::RepositoryIdSelector {
                                repository_id: request.repository(),
                            },
                        ),
                        Some(request.generation().clone()),
                    ),
                    AgentResolutionContext::new(cancellation.clone(), deadline),
                )
                .await
                .map_err(map_plan_port_error)?;
            plan_change_checkpoint(&cancellation, deadline)?;
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

        let budget = rootlight_mcp_contract::vertical::ResponseBudget {
            max_results: request.max_steps().map(u16::from),
            max_tokens: None,
            max_source_bytes: None,
            max_traversal_facts: None,
            max_depth: None,
            max_paths: None,
            timeout_ms: None,
            evidence_level: None,
        };
        let output = port
            .plan_change(
                request.clone(),
                AgentCallContext::new(cancellation.clone(), budget, Some(deadline)),
            )
            .await
            .map_err(map_plan_port_error)?;
        plan_change_checkpoint(&cancellation, deadline)?;
        if output.identity.repository.repository_id != request.repository()
            || matches!(
                request.generation(),
                GenerationSelector::Explicit(expected)
                    if output.identity.generation.generation_id != *expected
            )
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
        let data = shape_plan_change(output.result).map_err(PlanChangeServiceError::Admission)?;
        Ok(ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: output.identity.repository,
            generation: output.identity.generation,
            coverage: output.identity.coverage,
            data,
            truncated: output.truncated,
            completeness: output.completeness,
            next_cursor: RequiredNullable(None),
            usage: output.usage,
            warnings: output.warnings,
            trust: TrustClassification::UntrustedRepositoryData,
        })
    }
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
    match error {
        AgentPortError::Public(error) => PlanChangeServiceError::Public(error),
        AgentPortError::Cancelled => PlanChangeServiceError::Cancelled,
        AgentPortError::DeadlineExceeded => PlanChangeServiceError::DeadlineExceeded,
        AgentPortError::LocalDeadlineExceeded => PlanChangeServiceError::InvalidResponse,
        AgentPortError::InvalidResponse => PlanChangeServiceError::InvalidResponse,
        AgentPortError::Unavailable => PlanChangeServiceError::Unavailable,
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
        explanation: Some(explanation),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_ids::{FileId, GenerationId, RepositoryId, SymbolId};
    use rootlight_mcp_contract::{
        RepositorySelector,
        change::{
            ContextPackRequest, PlanChangeInput, PlanFileTarget, PlanImpactSummary, PlanObjective,
            PlanSymbolTarget, PlanTargetSelector, RiskLevel,
        },
        vertical::{RepositoryIdSelector, ResponseProfile},
    };

    use super::{
        PlanChangeError, PlanChangeResult, PlanImpactResult, explain_plan_change,
        normalize_plan_change, shape_plan_change,
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
            plan: Vec::new(),
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
            plan: Vec::new(),
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
}
