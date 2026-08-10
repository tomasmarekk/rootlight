//! Public integration tests for transport-neutral change-plan orchestration.
//!
//! These tests keep request admission, policy propagation, identity pinning,
//! and public response shaping independent from the application adapter.

use std::{
    collections::BTreeMap,
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
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentPortFuture,
        AgentResolutionContext, AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
};
use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use rootlight_ir::CoverageStatus;
use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, TrustClassification,
    change::{
        ChangePlanStep, ContextPackRequest, PlanChangeData, PlanChangeInput,
        PlanEvidenceOmissionReason, PlanEvidenceProvider, PlanObjective, PlanProviderState,
        PlanSymbolTarget, PlanTargetSelector, RiskLevel,
    },
    context::BatchTool,
    vertical::{
        CacheStatus, CoverageSummary, Freshness, GenerationSelector, GenerationSummary,
        ProvenanceLevel, ReadEnvelope, RepositoryIdSelector, RequiredNullable, ResolvedRepository,
        ResponseBudget, ResponseProfile, UsageSummary,
    },
};
use serde_json::{Value, json};

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
struct ProviderCall {
    tool: BatchTool,
    budget: ResponseBudget,
    identity: AgentResolvedIdentity,
    cancelled: bool,
    deadline: Option<Instant>,
}

#[derive(Debug)]
struct FakePort {
    identity_response: Mutex<Option<Result<AgentResolvedIdentity, AgentPortError>>>,
    plan_response: Mutex<Option<Result<PlanChangePortOutput, AgentPortError>>>,
    identity_calls: Mutex<Vec<IdentityCall>>,
    plan_calls: Mutex<Vec<PlanCall>>,
    provider_calls: Mutex<Vec<ProviderCall>>,
    provider_generation: Mutex<Option<GenerationId>>,
    provider_error: Mutex<Option<(BatchTool, AgentPortError)>>,
    provider_items: Mutex<Option<(BatchTool, usize)>>,
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
            provider_calls: Mutex::new(Vec::new()),
            provider_generation: Mutex::new(None),
            provider_error: Mutex::new(None),
            provider_items: Mutex::new(None),
        }
    }

    fn with_provider_generation(self, generation: GenerationId) -> Self {
        *self
            .provider_generation
            .lock()
            .expect("provider generation lock is available") = Some(generation);
        self
    }

    fn with_provider_error(self, tool: BatchTool, error: AgentPortError) -> Self {
        *self
            .provider_error
            .lock()
            .expect("provider error lock is available") = Some((tool, error));
        self
    }

    fn with_provider_items(self, tool: BatchTool, items: usize) -> Self {
        *self
            .provider_items
            .lock()
            .expect("provider items lock is available") = Some((tool, items));
        self
    }
}

impl AgentToolPort<TestCancellation> for FakePort {
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

    fn execute(
        &self,
        request: AgentToolRequest,
        context: AgentCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>> {
        let tool = request.tool();
        let mut pinned = context
            .pinned_identity()
            .cloned()
            .expect("plan providers receive the pinned identity");
        if let Some(generation) = *self
            .provider_generation
            .lock()
            .expect("provider generation lock is available")
        {
            pinned.generation.generation_id = generation;
        }
        self.provider_calls
            .lock()
            .expect("provider call lock is available")
            .push(ProviderCall {
                tool,
                budget: context.budget().clone(),
                identity: pinned.clone(),
                cancelled: context.cancellation().is_cancelled(),
                deadline: context.deadline(),
            });
        let error = {
            let mut configured = self
                .provider_error
                .lock()
                .expect("provider error lock is available");
            if configured
                .as_ref()
                .is_some_and(|(configured_tool, _)| *configured_tool == tool)
            {
                configured.take().map(|(_, error)| error)
            } else {
                None
            }
        };
        let item_count = self
            .provider_items
            .lock()
            .expect("provider items lock is available")
            .as_ref()
            .filter(|(configured_tool, _)| *configured_tool == tool)
            .map(|(_, count)| *count)
            .unwrap_or(1);
        Box::pin(async move {
            match error {
                Some(error) => Err(error),
                None => Ok(provider_envelope(tool, pinned, item_count)),
            }
        })
    }
}

impl PlanChangePort<TestCancellation> for FakePort {
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

fn provider_envelope(
    tool: BatchTool,
    pinned: AgentResolvedIdentity,
    item_count: usize,
) -> ReadEnvelope<Value> {
    let resolved_changes = (0..item_count)
        .map(|_| {
            json!({
                "symbol_id": symbol(),
                "file_id": null,
                "classification": "body",
                "kind": "function"
            })
        })
        .collect::<Vec<_>>();
    let data = match tool {
        BatchTool::ChangeImpact => json!({
            "resolved_changes": resolved_changes,
            "impacted": [],
            "service_impacts": [],
            "tests": [],
            "risk_summary": {
                "level": "low",
                "reasons": [],
                "coverage": "bounded",
                "breaking_surface": false,
                "fanout": 0,
                "dynamic_blind_spots": false
            }
        }),
        BatchTool::SymbolRelationships => json!({
            "groups": [],
            "unresolved": [{
                "seed": symbol(),
                "relation": "calls",
                "candidate_count": 1,
                "reason": "ambiguous target"
            }],
            "totals": {"returned_edges": 0, "total_edges": 0, "exact": true}
        }),
        BatchTool::TestsSelect => json!({
            "tests": [],
            "coverage_strategy": {
                "direct_edges": true,
                "transitive_signals": true,
                "history_signals": false,
                "file_colocation_signals": true
            },
            "gaps": [{"scope": "target", "reason": "no_direct_test"}]
        }),
        BatchTool::ArchitectureOverview => json!({
            "components": [{
                "id": "component-a",
                "kind": "module",
                "name": "component a",
                "symbol_count": 1,
                "file_count": 1,
                "responsibility_evidence": ["declaring_file"],
                "source_refs": [],
                "confidence": 900,
                "trust": "untrusted_repository_data"
            }],
            "connections": [],
            "hotspots": [],
            "views": []
        }),
        _ => panic!("unexpected plan evidence provider {tool:?}"),
    };
    ReadEnvelope {
        schema_version: rootlight_mcp_contract::SchemaVersion::V1_0,
        repository: pinned.repository,
        generation: pinned.generation,
        coverage: pinned.coverage,
        data,
        truncated: false,
        completeness: rootlight_mcp_contract::completeness::ResultCompleteness::complete(),
        next_cursor: RequiredNullable(None),
        usage: UsageSummary {
            rows: 1,
            edges: 1,
            source_bytes: 0,
            json_bytes: 64,
            estimated_tokens: 16,
            wall_time_ms: 1,
            cache_status: CacheStatus::Miss,
            trace_id: format!("plan-provider-{tool:?}").to_ascii_lowercase(),
        },
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    }
}

fn plan_result() -> PlanChangeResult {
    PlanChangeResult {
        plan: vec![ChangePlanStep {
            step: 1,
            action: "preserve request admission".to_owned(),
            rationale: "structural planner proposal".to_owned(),
            evidence_refs: Vec::new(),
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
    let port = Arc::new(FakePort::new(
        Some(Ok(identity(generation(2)))),
        Some(Ok(plan_output(generation(2)))),
    ));
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

    assert_eq!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .len(),
        1
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
    let provider_calls = port
        .provider_calls
        .lock()
        .expect("provider call lock is available");
    assert_eq!(
        provider_calls
            .iter()
            .map(|call| call.tool)
            .collect::<Vec<_>>(),
        vec![
            BatchTool::ChangeImpact,
            BatchTool::SymbolRelationships,
            BatchTool::TestsSelect,
            BatchTool::ArchitectureOverview,
        ]
    );
    assert!(provider_calls.iter().all(|call| {
        call.identity.repository.repository_id == repository()
            && call.identity.generation.generation_id == generation(2)
            && !call.cancelled
            && call.deadline == Some(deadline)
    }));
    assert!(provider_calls.windows(2).all(|calls| {
        calls[0].budget.max_tokens >= calls[1].budget.max_tokens
            && calls[0].budget.max_results >= calls[1].budget.max_results
    }));
    assert_eq!(output.generation.generation_id, generation(2));
    assert_eq!(output.usage.rows, 16);
    assert_eq!(output.usage.edges, 8);
    assert_eq!(output.usage.source_bytes, 0);
    assert_eq!(output.usage.json_bytes, 0);
    assert_eq!(output.usage.estimated_tokens, 0);
    assert_eq!(output.usage.trace_id, "plan-change-orchestration");
    assert!(output.truncated);
    assert_eq!(output.trust, TrustClassification::UntrustedRepositoryData);
    assert_eq!(output.data.affected_scope.risk_level, RiskLevel::High);
    assert_eq!(output.data.provider_coverage.len(), 7);
    assert_eq!(
        output
            .data
            .provider_coverage
            .iter()
            .map(|coverage| coverage.provider)
            .collect::<Vec<_>>(),
        vec![
            PlanEvidenceProvider::ChangeImpact,
            PlanEvidenceProvider::Relationships,
            PlanEvidenceProvider::Tests,
            PlanEvidenceProvider::Architecture,
            PlanEvidenceProvider::History,
            PlanEvidenceProvider::Source,
            PlanEvidenceProvider::Ownership,
        ]
    );
    assert_eq!(
        output.data.provider_coverage[4]
            .omission
            .as_ref()
            .map(|value| value.reason),
        Some(PlanEvidenceOmissionReason::HistoryBaselineUnavailable)
    );
    assert_eq!(
        output.data.provider_coverage[4].state,
        PlanProviderState::Unsupported
    );
    assert_eq!(output.data.plan[0].evidence_refs.len(), 1);
    assert!(output.data.explanation.is_none());
    let encoded = serde_json::to_value(output).expect("change-plan envelope serializes");
    serde_json::from_value::<ReadEnvelope<PlanChangeData>>(encoded)
        .expect("change-plan envelope retains its public schema");
}

#[tokio::test]
async fn accepted_budget_is_preserved_and_max_steps_only_reduces_results() {
    for (requested_results, max_steps, effective_results, remaining_facts) in
        [(20, 4, 4, 983), (8, 4, 4, 985), (1_000, 100, 100, 983)]
    {
        let requested_budget = ResponseBudget {
            max_results: Some(requested_results),
            max_tokens: Some(4_000),
            max_source_bytes: Some(654),
            max_traversal_facts: Some(987),
            max_depth: Some(3),
            max_paths: Some(5),
            timeout_ms: Some(900),
            evidence_level: Some(ProvenanceLevel::Compact),
        };
        let mut request = input(generation(2));
        request.max_steps = Some(max_steps);
        request.budget = Some(requested_budget.clone());
        let port = Arc::new(FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Ok(plan_output(generation(2)))),
        ));

        PlanChangeService
            .execute(
                Arc::clone(&port),
                request,
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("change-plan request succeeds");

        let calls = port.plan_calls.lock().expect("plan call lock is available");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].budget.max_results, Some(effective_results));
        assert!(
            calls[0]
                .budget
                .max_tokens
                .is_some_and(|tokens| tokens < 4_000)
        );
        assert_eq!(calls[0].budget.max_source_bytes, Some(654));
        assert_eq!(calls[0].budget.max_traversal_facts, Some(remaining_facts));
        assert_eq!(calls[0].budget.max_depth, Some(3));
        assert_eq!(calls[0].budget.max_paths, Some(5));
        assert_eq!(calls[0].budget.timeout_ms, Some(900));
        assert_eq!(
            calls[0].budget.evidence_level,
            Some(ProvenanceLevel::Compact)
        );
        assert!(u16::from(max_steps) >= effective_results);
        assert!(requested_results >= effective_results);
    }
}

#[tokio::test]
async fn single_step_budget_preserves_planning_when_optional_evidence_cannot_run() {
    let mut request = input(generation(2));
    request.max_steps = Some(1);
    request.budget = Some(ResponseBudget {
        max_results: Some(1),
        max_tokens: Some(4_000),
        max_source_bytes: Some(1_024),
        max_traversal_facts: Some(1_024),
        max_depth: Some(4),
        max_paths: Some(4),
        timeout_ms: Some(900),
        evidence_level: Some(ProvenanceLevel::Compact),
    });
    let port = Arc::new(FakePort::new(
        Some(Ok(identity(generation(2)))),
        Some(Ok(plan_output(generation(2)))),
    ));

    let output = PlanChangeService
        .execute(
            Arc::clone(&port),
            request,
            TestCancellation(false),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("the core plan retains its reserved result capacity");

    assert!(
        port.provider_calls
            .lock()
            .expect("provider call lock is available")
            .is_empty()
    );
    let calls = port.plan_calls.lock().expect("plan call lock is available");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].budget.max_results, Some(1));
    assert_eq!(output.data.plan.len(), 1);
    assert!(output.data.provider_coverage[..4].iter().all(|coverage| {
        coverage.state == PlanProviderState::Omitted
            && coverage.omission.as_ref().is_some_and(|omission| {
                omission.reason == PlanEvidenceOmissionReason::SharedBudgetExhausted
            })
    }));
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
    let port = Arc::new(FakePort::new(
        Some(Ok(identity(generation(2)))),
        Some(Ok(plan_output(generation(9)))),
    ));

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
        Some(Ok(identity(generation(2)))),
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
        let port = Arc::new(FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Err(port_error)),
        ));
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

#[tokio::test]
async fn pinned_identity_path_skips_resolution_and_preserves_plan_behavior() {
    let port = Arc::new(FakePort::new(
        Some(Err(AgentPortError::Unavailable)),
        Some(Ok(plan_output(generation(2)))),
    ));
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut request = input(generation(9));
    request.generation = None;

    let output = PlanChangeService
        .execute_with_identity(
            Arc::clone(&port),
            request,
            identity(generation(2)),
            TestCancellation(false),
            deadline,
        )
        .await
        .expect("change plan succeeds under the pinned identity");

    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    let calls = port.plan_calls.lock().expect("plan call lock is available");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].request.generation(),
        &GenerationSelector::Explicit(generation(2))
    );
    assert_eq!(calls[0].deadline, Some(deadline));
    assert_eq!(output.generation.generation_id, generation(2));
    assert_eq!(output.usage.rows, 16);
    assert_eq!(output.usage.edges, 8);
    assert_eq!(output.usage.trace_id, "plan-change-orchestration");
}

#[tokio::test]
async fn minimum_publication_budget_fails_before_provider_or_planner_work() {
    let mut request = input(generation(2));
    request.budget = Some(ResponseBudget {
        max_results: None,
        max_tokens: Some(100),
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: None,
        evidence_level: None,
    });
    let port = Arc::new(FakePort::new(Some(Ok(identity(generation(2)))), None));

    assert_eq!(
        PlanChangeService
            .execute(
                Arc::clone(&port),
                request,
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::BudgetExceeded)
    );
    assert!(
        port.provider_calls
            .lock()
            .expect("provider call lock is available")
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
async fn provider_identity_race_fails_closed_before_structural_planning() {
    let port = Arc::new(
        FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Ok(plan_output(generation(2)))),
        )
        .with_provider_generation(generation(9)),
    );

    assert_eq!(
        PlanChangeService
            .execute(
                Arc::clone(&port),
                input(generation(2)),
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::InvalidResponse)
    );
    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn unavailable_provider_is_an_explicit_omission_not_fabricated_evidence() {
    let port = Arc::new(
        FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Ok(plan_output(generation(2)))),
        )
        .with_provider_error(BatchTool::TestsSelect, AgentPortError::Unavailable),
    );

    let output = PlanChangeService
        .execute(
            port,
            input(generation(2)),
            TestCancellation(false),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("an unavailable optional provider preserves an honest partial plan");

    let tests = &output.data.provider_coverage[2];
    assert_eq!(tests.provider, PlanEvidenceProvider::Tests);
    assert_eq!(tests.state, PlanProviderState::Omitted);
    assert!(tests.evidence.is_empty());
    assert_eq!(
        tests.omission.as_ref().map(|omission| omission.reason),
        Some(PlanEvidenceOmissionReason::ProviderUnavailable)
    );
    assert_eq!(
        output.completeness.state,
        rootlight_mcp_contract::completeness::CompletenessState::Indeterminate
    );
}

#[tokio::test]
async fn provider_cancellation_stops_remaining_evidence_and_planning() {
    let port = Arc::new(
        FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Ok(plan_output(generation(2)))),
        )
        .with_provider_error(BatchTool::SymbolRelationships, AgentPortError::Cancelled),
    );

    assert_eq!(
        PlanChangeService
            .execute(
                Arc::clone(&port),
                input(generation(2)),
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await,
        Err(PlanChangeServiceError::Cancelled)
    );
    assert_eq!(
        port.provider_calls
            .lock()
            .expect("provider call lock is available")
            .iter()
            .map(|call| call.tool)
            .collect::<Vec<_>>(),
        vec![BatchTool::ChangeImpact, BatchTool::SymbolRelationships]
    );
    assert!(
        port.plan_calls
            .lock()
            .expect("plan call lock is available")
            .is_empty()
    );
}

#[tokio::test]
async fn provider_evidence_over_sixty_four_is_explicitly_truncated() {
    let mut structural = plan_output(generation(2));
    structural.truncated = false;
    structural.completeness = rootlight_mcp_contract::completeness::ResultCompleteness::complete();
    let port = Arc::new(
        FakePort::new(Some(Ok(identity(generation(2)))), Some(Ok(structural)))
            .with_provider_items(BatchTool::ChangeImpact, 65),
    );

    let output = PlanChangeService
        .execute(
            port,
            input(generation(2)),
            TestCancellation(false),
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("bounded evidence projection succeeds");

    let impact = &output.data.provider_coverage[0];
    assert_eq!(impact.evidence.len(), 64);
    assert_eq!(impact.state, PlanProviderState::Partial);
    assert_eq!(
        impact.completeness.state,
        rootlight_mcp_contract::completeness::CompletenessState::Truncated
    );
    assert!(
        impact
            .completeness
            .limiting_resources
            .iter()
            .any(|resource| {
                resource.kind == rootlight_mcp_contract::completeness::LimitingResourceKind::Results
                    && resource.limit == Some(64)
                    && resource.observed == Some(65)
            })
    );
    assert!(
        output
            .warnings
            .iter()
            .any(|warning| { warning.code.as_str() == "plan_provider_evidence_truncated" })
    );
}

#[tokio::test]
async fn objective_rationales_match_the_reviewed_golden() {
    let golden: BTreeMap<String, String> = serde_json::from_str(include_str!(
        "fixtures/plan_change_objective_rationales.json"
    ))
    .expect("objective rationale golden is valid");
    let objectives = [
        (PlanObjective::BugFix, "bug_fix"),
        (PlanObjective::Refactor, "refactor"),
        (PlanObjective::Migration, "migration"),
        (PlanObjective::Review, "review"),
    ];

    for (objective, label) in objectives {
        let mut request = input(generation(2));
        request.objective = objective;
        let port = Arc::new(FakePort::new(
            Some(Ok(identity(generation(2)))),
            Some(Ok(plan_output(generation(2)))),
        ));

        let output = PlanChangeService
            .execute(
                port,
                request,
                TestCancellation(false),
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect("objective-specific change plan succeeds");

        assert_eq!(
            output.data.plan[0].rationale, golden[label],
            "{label} rationale drifted from its reviewed golden"
        );
    }
}
