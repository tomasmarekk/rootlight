//! Public integration tests for transport-neutral change-plan orchestration.
//!
//! These tests keep request admission, policy propagation, identity pinning,
//! and public response shaping independent from the application adapter.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rootlight_agent::{
    change::{
        PlanChangeError, PlanChangePort, PlanChangePortFuture, PlanChangePortOutput,
        PlanChangeRequest, PlanChangeResult, PlanChangeService, PlanChangeServiceError,
        PlanImpactResult,
    },
    policy::CancellationSignal,
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentResolutionContext,
        AgentResolvedIdentity,
    },
};
use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_ir::CoverageStatus;
use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, TrustClassification,
    change::{
        ChangePlanStep, ContextPackRequest, PlanChangeData, PlanChangeInput, PlanObjective,
        PlanSymbolTarget, PlanTargetSelector, RiskLevel,
    },
    vertical::{
        CacheStatus, CoverageSummary, Freshness, GenerationSelector, GenerationSummary,
        ReadEnvelope, RepositoryIdSelector, RequiredNullable, ResolvedRepository, ResponseBudget,
        ResponseProfile, UsageSummary,
    },
};

#[derive(Debug, Clone, Copy)]
struct TestCancellation(bool);

impl CancellationSignal for TestCancellation {
    fn is_cancelled(&self) -> bool {
        self.0
    }
}

#[derive(Debug)]
struct IdentityCall {
    repository: RepositorySelector,
    generation: Option<GenerationSelector>,
    cancelled: bool,
    deadline: Instant,
}

#[derive(Debug)]
struct PlanCall {
    request: PlanChangeRequest,
    budget: ResponseBudget,
    cancelled: bool,
    deadline: Option<Instant>,
}

#[derive(Debug)]
struct FakePort {
    identity_response: Mutex<Option<Result<AgentResolvedIdentity, AgentPortError>>>,
    plan_response: Mutex<Option<Result<PlanChangePortOutput, AgentPortError>>>,
    identity_calls: Mutex<Vec<IdentityCall>>,
    plan_calls: Mutex<Vec<PlanCall>>,
}

impl FakePort {
    fn new(
        identity_response: Option<Result<AgentResolvedIdentity, AgentPortError>>,
        plan_response: Option<Result<PlanChangePortOutput, AgentPortError>>,
    ) -> Self {
        Self {
            identity_response: Mutex::new(identity_response),
            plan_response: Mutex::new(plan_response),
            identity_calls: Mutex::new(Vec::new()),
            plan_calls: Mutex::new(Vec::new()),
        }
    }
}

impl PlanChangePort<TestCancellation> for FakePort {
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<TestCancellation>,
    ) -> PlanChangePortFuture<Result<AgentResolvedIdentity, AgentPortError>> {
        let (repository, generation) = request.into_selectors();
        self.identity_calls
            .lock()
            .expect("identity call lock is available")
            .push(IdentityCall {
                repository,
                generation,
                cancelled: context.cancellation().is_cancelled(),
                deadline: context.deadline(),
            });
        let response = self
            .identity_response
            .lock()
            .expect("identity response lock is available")
            .take()
            .expect("test configured one identity response");
        Box::pin(async move { response })
    }

    fn plan_change(
        &self,
        request: PlanChangeRequest,
        context: AgentCallContext<TestCancellation>,
    ) -> PlanChangePortFuture<Result<PlanChangePortOutput, AgentPortError>> {
        self.plan_calls
            .lock()
            .expect("plan call lock is available")
            .push(PlanCall {
                request,
                budget: context.budget().clone(),
                cancelled: context.cancellation().is_cancelled(),
                deadline: context.deadline(),
            });
        let response = self
            .plan_response
            .lock()
            .expect("plan response lock is available")
            .take()
            .expect("test configured one plan response");
        Box::pin(async move { response })
    }
}

fn repository() -> RepositoryId {
    RepositoryId::from_bytes([1; 16])
}

fn generation(byte: u8) -> GenerationId {
    GenerationId::from_bytes([byte; 20])
}

fn symbol() -> SymbolId {
    SymbolId::from_bytes([3; 20])
}

fn identity(generation_id: GenerationId) -> AgentResolvedIdentity {
    AgentResolvedIdentity {
        repository: ResolvedRepository {
            repository_id: repository(),
            display_name: "fixture".to_owned(),
        },
        generation: GenerationSummary {
            generation_id,
            parent_generation: RequiredNullable(None),
            structural_freshness: Freshness::Current,
            semantic_freshness: Freshness::Current,
        },
        coverage: CoverageSummary {
            status: CoverageStatus::Bounded,
            languages: Vec::new(),
            skipped_inputs: 0,
        },
        warnings: Vec::new(),
    }
}

fn usage() -> UsageSummary {
    UsageSummary {
        rows: 12,
        edges: 4,
        source_bytes: 0,
        json_bytes: 256,
        estimated_tokens: 64,
        wall_time_ms: 3,
        cache_status: CacheStatus::Miss,
        trace_id: "plan-change-child".to_owned(),
    }
}

fn plan_result() -> PlanChangeResult {
    PlanChangeResult {
        plan: vec![ChangePlanStep {
            step: 1,
            action: "preserve request admission".to_owned(),
            targets: vec![symbol()],
            depends_on: Vec::new(),
            risks: vec!["public_contract".to_owned()],
            verification: Some("run agent service integration tests".to_owned()),
        }],
        affected_scope: PlanImpactResult {
            affected_symbols: 1,
            affected_files: 1,
            risk_level: "high".to_owned(),
            touches_public_surface: true,
        },
        test_plan: Vec::new(),
        open_decisions: Vec::new(),
        context_pack_request: ContextPackRequest {
            symbols: vec![symbol()],
            files: Vec::new(),
        },
    }
}

fn plan_output(generation_id: GenerationId) -> PlanChangePortOutput {
    PlanChangePortOutput {
        identity: identity(generation_id),
        result: plan_result(),
        usage: usage(),
        truncated: true,
        completeness: rootlight_mcp_contract::completeness::ResultCompleteness::new(
            rootlight_mcp_contract::completeness::CompletenessState::Truncated,
            vec![
                rootlight_mcp_contract::completeness::LimitingResource::kind(
                    rootlight_mcp_contract::completeness::LimitingResourceKind::Results,
                ),
            ],
            rootlight_mcp_contract::completeness::ContinuationAvailability::Unavailable,
            vec![rootlight_mcp_contract::completeness::ContinuationGuidance::NarrowScope],
        )
        .expect("fixture completeness is valid"),
        warnings: Vec::new(),
    }
}

fn input(generation_id: GenerationId) -> PlanChangeInput {
    PlanChangeInput {
        repository: RepositorySelector::ById(RepositoryIdSelector {
            repository_id: repository(),
        }),
        generation: Some(GenerationSelector::Explicit(generation_id)),
        objective: PlanObjective::BugFix,
        objective_text: "repair request admission".to_owned(),
        targets: vec![PlanTargetSelector::Symbol(PlanSymbolTarget {
            symbol_id: symbol(),
        })],
        constraints: None,
        change_context: None,
        max_steps: Some(4),
        budget: None,
        profile: Some(ResponseProfile::Compact),
        explain: None,
    }
}

#[tokio::test]
async fn admission_rejects_empty_targets_before_port_work() {
    let mut request = input(generation(2));
    request.targets.clear();
    let port = Arc::new(FakePort::new(None, None));

    let result = PlanChangeService
        .execute(
            Arc::clone(&port),
            request,
            TestCancellation(false),
            Instant::now() + Duration::from_secs(1),
        )
        .await;

    assert_eq!(
        result,
        Err(PlanChangeServiceError::Admission(
            PlanChangeError::EmptyTargets
        ))
    );
    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn explain_resolves_explicit_identity_without_plan_call_and_shapes_envelope() {
    let mut request = input(generation(2));
    request.explain = Some(true);
    let port = Arc::new(FakePort::new(Some(Ok(identity(generation(2)))), None));
    let deadline = Instant::now() + Duration::from_secs(1);

    let output = PlanChangeService
        .execute(
            Arc::clone(&port),
            request,
            TestCancellation(false),
            deadline,
        )
        .await
        .expect("explain request succeeds");

    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
    let calls = port
        .identity_calls
        .lock()
        .expect("identity call lock is available");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].repository,
        RepositorySelector::ById(RepositoryIdSelector {
            repository_id: repository(),
        })
    );
    assert_eq!(
        calls[0].generation,
        Some(GenerationSelector::Explicit(generation(2)))
    );
    assert!(!calls[0].cancelled);
    assert_eq!(calls[0].deadline, deadline);
    assert_eq!(output.generation.generation_id, generation(2));
    assert!(output.data.explanation.is_some());
    let encoded = serde_json::to_value(output).expect("change-plan envelope serializes");
    serde_json::from_value::<ReadEnvelope<PlanChangeData>>(encoded)
        .expect("change-plan envelope retains its public schema");
}

#[tokio::test]
async fn plan_call_receives_cancellation_deadline_and_result_budget() {
    let port = Arc::new(FakePort::new(None, Some(Ok(plan_output(generation(2))))));
    let deadline = Instant::now() + Duration::from_secs(1);

    let output = PlanChangeService
        .execute(
            Arc::clone(&port),
            input(generation(2)),
            TestCancellation(false),
            deadline,
        )
        .await
        .expect("change-plan request succeeds");

    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    let calls = port.plan_calls.lock().expect("plan call lock is available");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].request.repository(), repository());
    assert_eq!(
        calls[0].request.generation(),
        &GenerationSelector::Explicit(generation(2))
    );
    assert_eq!(calls[0].request.max_steps(), Some(4));
    assert_eq!(calls[0].budget.max_results, Some(4));
    assert!(!calls[0].cancelled);
    assert_eq!(calls[0].deadline, Some(deadline));
    assert_eq!(output.generation.generation_id, generation(2));
    assert_eq!(output.usage, usage());
    assert!(output.truncated);
    assert_eq!(output.trust, TrustClassification::UntrustedRepositoryData);
    assert_eq!(output.data.affected_scope.risk_level, RiskLevel::High);
    assert!(output.data.explanation.is_none());
    let encoded = serde_json::to_value(output).expect("change-plan envelope serializes");
    serde_json::from_value::<ReadEnvelope<PlanChangeData>>(encoded)
        .expect("change-plan envelope retains its public schema");
}

#[tokio::test]
async fn already_cancelled_request_stops_before_identity_or_plan_work() {
    let port = Arc::new(FakePort::new(None, None));

    assert_eq!(
        PlanChangeService
            .execute(
                Arc::clone(&port),
                input(generation(2)),
                TestCancellation(true),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::Cancelled)
    );
    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn expired_deadline_stops_before_identity_or_plan_work() {
    let port = Arc::new(FakePort::new(None, None));

    assert_eq!(
        PlanChangeService
            .execute(
                Arc::clone(&port),
                input(generation(2)),
                TestCancellation(false),
                Instant::now() - Duration::from_millis(1),
            )
            .await,
        Err(PlanChangeServiceError::DeadlineExceeded)
    );
    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn explicit_generation_mismatch_fails_closed() {
    let port = Arc::new(FakePort::new(None, Some(Ok(plan_output(generation(9))))));

    assert_eq!(
        PlanChangeService
            .execute(
                port,
                input(generation(2)),
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::InvalidResponse)
    );
}

#[tokio::test]
async fn explain_identity_generation_mismatch_fails_closed() {
    let mut request = input(generation(2));
    request.explain = Some(true);
    let port = Arc::new(FakePort::new(Some(Ok(identity(generation(9)))), None));

    assert_eq!(
        PlanChangeService
            .execute(
                port,
                request,
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::InvalidResponse)
    );
}

#[tokio::test]
async fn port_errors_preserve_public_and_policy_classification() {
    let public = PublicError::builder(ErrorCode::UnsupportedCapability, "capability unavailable")
        .build()
        .expect("static public error is valid");
    let public_port = Arc::new(FakePort::new(
        None,
        Some(Err(AgentPortError::Public(Box::new(public.clone())))),
    ));
    assert_eq!(
        PlanChangeService
            .execute(
                public_port,
                input(generation(2)),
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::Public(Box::new(public)))
    );

    for (port_error, expected) in [
        (AgentPortError::Cancelled, PlanChangeServiceError::Cancelled),
        (
            AgentPortError::DeadlineExceeded,
            PlanChangeServiceError::DeadlineExceeded,
        ),
        (
            AgentPortError::InvalidResponse,
            PlanChangeServiceError::InvalidResponse,
        ),
    ] {
        let port = Arc::new(FakePort::new(None, Some(Err(port_error))));
        assert_eq!(
            PlanChangeService
                .execute(
                    port,
                    input(generation(2)),
                    TestCancellation(false),
                    Instant::now() + Duration::from_secs(1),
                )
                .await,
            Err(expected)
        );
    }
}
